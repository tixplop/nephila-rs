//! Commande `update` : mise à jour incrémentale du graphe (REQ-011, REQ-010, REQ-008).

use std::collections::HashMap;
use std::path::Path;

use crate::diff::{self, Change};
use crate::error::Result;
use crate::filter::{Filters, Matcher, ProjectFile};
use crate::index::build::{self, BuildReport, IndexOptions};
use crate::index::lock::RepoLock;
use crate::index::structure;
use crate::llm::Llm;
use crate::model::{FileRecord, FileState};
use crate::scan::ScannedFile;
use crate::store::{db_path, Store};
use crate::trace;

/// Point d'entrée de `nephila update` : mêmes contrôles que `build` (dépôt, configuration, clé,
/// verrou) ; sans graphe, bascule sur un build complet.
pub fn run(
    repo: &Path,
    no_llm: bool,
    verbose: bool,
    batch_files: Option<usize>,
    format_retries: Option<u32>,
    cli_filters: Filters,
) -> Result<()> {
    trace::init(verbose);
    build::check_repo(repo)?;
    let project = ProjectFile::load(repo)?;
    let matcher = build::effective_matcher(repo, &project.filters, &cli_filters)?;
    let setup = build::setup(no_llm, &project.settings, batch_files, format_retries)?;
    let lock = RepoLock::acquire(repo)?;
    // Le test d'existence se fait sous verrou : aucun build concurrent ne peut le créer entre-temps.
    if !db_path(repo).is_file() {
        eprintln!("aucun graphe : build complet");
        let report = build::build(repo, &setup.opts, setup.llm.as_deref(), &lock, &matcher)?;
        let done = build::finish(&report, "build");
        build::warn_transient(&cli_filters);
        return done;
    }
    let done = match update(repo, &setup.opts, setup.llm.as_deref(), &lock, &matcher)? {
        None => {
            println!("graphe à jour");
            Ok(())
        }
        Some(report) => build::finish(&report, "update"),
    };
    build::warn_transient(&cli_filters);
    done
}

/// Vrai pour un fichier `settings.gradle(.kts)`.
fn is_settings(path: &str) -> bool {
    matches!(
        path.rsplit('/').next(),
        Some("settings.gradle" | "settings.gradle.kts")
    )
}

/// Vrai pour un script `build.gradle(.kts)` situé dans `dir` ou sous `dir` (`""` = racine).
fn is_build_script_under(path: &str, dir: &str) -> bool {
    let Some((parent, name)) = path.rsplit_once('/').or(Some(("", path))) else {
        return false;
    };
    matches!(name, "build.gradle" | "build.gradle.kts")
        && (dir.is_empty() || parent == dir || parent.starts_with(&format!("{dir}/")))
}

/// Met à jour le graphe ; `None` si rien n'a changé (aucun appel LLM, résumé conservé). Le verrou,
/// pris par l'appelant, prouve l'exclusivité avant toute écriture.
pub fn update(
    repo: &Path,
    opts: &IndexOptions,
    llm: Option<&dyn Llm>,
    _lock: &RepoLock,
    matcher: &Matcher,
) -> Result<Option<BuildReport>> {
    build::check_repo(repo)?;
    let mut store = Store::open_existing(repo)?;
    let d = diff::compute(repo, &store, matcher)?;
    trace!(
        "parcours: {} ajoutés, {} modifiés, {} supprimés, {} en attente{}",
        d.paths(Change::Added).count(),
        d.paths(Change::Modified).count(),
        d.paths(Change::Deleted).count(),
        d.paths(Change::Pending).count(),
        matcher.trace_suffix(d.excluded)
    );

    // Fichiers dont seule la date a changé : l'enregistrement est rafraîchi pour ne plus
    // recalculer leur empreinte, sans toucher au graphe.
    let known: HashMap<&str, &FileRecord> = d.known.iter().map(|k| (k.path.as_str(), k)).collect();
    let touched: Vec<FileRecord> = d
        .scanned
        .iter()
        .filter(|f| {
            known.get(f.path.as_str()).is_some_and(|k| {
                k.state == FileState::Indexed
                    && k.hash == f.hash
                    && (f.rehashed || k.size != f.size || k.mtime != f.mtime)
            })
        })
        .map(|f| record(f, FileState::Indexed))
        .collect();
    if !touched.is_empty() {
        store.insert_files(&touched)?;
    }
    // Le modèle effectif est enregistré avant la comparaison : un changement de modèle (ou de
    // version du prompt) rend les fichiers « à enrichir », et ce n'est donc pas « aucun
    // changement » (REQ-049). Les statuts et commandes LLM inactif comparent à ce modèle.
    if let Some(llm) = llm {
        store.set_enrich_model(llm.model())?;
    }
    // « Aucun changement » : rien d'ajouté, modifié, supprimé ou en attente, et (LLM inactif ou
    // aucun fichier à enrichir).
    if d.is_up_to_date() && (llm.is_none() || store.count_files_to_enrich()? == 0) {
        return Ok(None);
    }

    for path in d.paths(Change::Deleted) {
        store.remove_file(path)?;
    }
    // Les chemins de projet Gradle dépendent des settings : un settings ajouté, modifié ou
    // supprimé fait réanalyser les scripts de build de son arborescence.
    let settings_dirs: Vec<&str> = d
        .changes
        .iter()
        .filter(|c| is_settings(&c.path))
        .map(|c| c.path.rsplit_once('/').map_or("", |(dir, _)| dir))
        .collect();
    let changed: std::collections::HashSet<&str> = d
        .changes
        .iter()
        .filter(|c| c.change != Change::Deleted)
        .map(|c| c.path.as_str())
        .collect();
    let todo: Vec<ScannedFile> = d
        .scanned
        .iter()
        .filter(|f| {
            changed.contains(f.path.as_str())
                || settings_dirs
                    .iter()
                    .any(|dir| is_build_script_under(&f.path, dir))
        })
        .cloned()
        .collect();
    // Nouveaux et modifiés passent « en attente » (empreinte à jour) jusqu'à leur analyse :
    // un échec ou une interruption les laisse repérables par le prochain update.
    let records: Vec<FileRecord> = todo.iter().map(|f| record(f, FileState::Pending)).collect();
    store.insert_files(&records)?;

    let mut report = BuildReport {
        excluded: matcher.is_active().then_some(d.excluded),
        ..BuildReport::default()
    };
    structure::analyze(
        repo,
        &todo,
        opts.concurrency,
        &d.gradle_settings,
        &mut store,
        &mut report,
    )?;
    build::finalize(&mut store, llm, opts, repo, &mut report)?;
    Ok(Some(report))
}

fn record(f: &ScannedFile, state: FileState) -> FileRecord {
    FileRecord {
        path: f.path.clone(),
        hash: f.hash.clone(),
        size: f.size,
        mtime: f.mtime,
        recorded_at: 0,
        state,
    }
}
