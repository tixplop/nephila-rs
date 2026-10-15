//! Commande `update` : mise à jour incrémentale du graphe (REQ-011, REQ-010, REQ-008).

use std::collections::HashMap;
use std::path::Path;

use crate::diff::{self, Change};
use crate::error::Result;
use crate::index::build::{self, BuildReport, IndexOptions};
use crate::index::lock::RepoLock;
use crate::index::structure;
use crate::llm::Llm;
use crate::model::{FileRecord, FileState};
use crate::scan::ScannedFile;
use crate::store::{db_path, Store};
use crate::trace;

/// Point d'entrée de `rkg update` : mêmes contrôles que `build` (dépôt, configuration, clé,
/// verrou) ; sans graphe, bascule sur un build complet.
pub fn run(repo: &Path, no_llm: bool, verbose: bool) -> Result<()> {
    trace::init(verbose);
    build::check_repo(repo)?;
    let setup = build::setup(no_llm)?;
    let lock = RepoLock::acquire(repo)?;
    // Le test d'existence se fait sous verrou : aucun build concurrent ne peut le créer entre-temps.
    if !db_path(repo).is_file() {
        eprintln!("aucun graphe : build complet");
        let report = build::build(repo, &setup.opts, setup.llm.as_deref(), &lock)?;
        return build::finish(&report, "build");
    }
    match update(repo, &setup.opts, setup.llm.as_deref(), &lock)? {
        None => {
            println!("graphe à jour");
            Ok(())
        }
        Some(report) => build::finish(&report, "update"),
    }
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
) -> Result<Option<BuildReport>> {
    build::check_repo(repo)?;
    let mut store = Store::open_existing(repo)?;
    let d = diff::compute(repo, &store)?;
    trace!(
        "parcours: {} ajoutés, {} modifiés, {} supprimés, {} en attente",
        d.paths(Change::Added).count(),
        d.paths(Change::Modified).count(),
        d.paths(Change::Deleted).count(),
        d.paths(Change::Pending).count()
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

    let mut report = BuildReport::default();
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
