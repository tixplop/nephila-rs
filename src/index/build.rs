//! Commande `build` : reconstruction complète du graphe (REQ-009, REQ-006).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::{Config, ProjectSettings, DEFAULT_CONCURRENCY, DEFAULT_MAX_CHUNK_CHARS};
use crate::error::{warn, Error, Result};
use crate::filter::{Filters, Matcher, ProjectFile};
use crate::index::lock::RepoLock;
use crate::index::{enrich, structure};
use crate::llm::{build_llm, Llm};
use crate::model::{FileRecord, FileState};
use crate::resolve;
use crate::scan;
use crate::store::{db_path, Store};
use crate::trace;

/// Bilan d'un build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildReport {
    pub processed: usize,
    /// Fichiers dont l'enrichissement conservé a été réappliqué par cette commande (REQ-049).
    pub retained: usize,
    pub pending: usize,
    pub nodes: usize,
    pub edges: usize,
    pub unresolved: usize,
    /// Nombre de fichiers dont l'analyse ou l'enrichissement a échoué.
    pub failed: usize,
    /// Fichiers indexés dont le contenu n'a pas été enrichi (résumés et concepts).
    pub to_enrich: usize,
    /// Vrai si un fournisseur LLM était actif pour cette commande.
    pub llm_active: bool,
    /// Cause de l'arrêt de l'envoi de nouveaux fichiers (clé refusée, panne réseau répétée).
    pub stop_cause: Option<String>,
    /// Message de la première clé refusée, quand elle a arrêté l'envoi (REQ-054).
    pub key_refused: Option<String>,
    /// Fichiers exclus par les filtres (REQ-037) ; `None` sans filtre actif.
    pub excluded: Option<usize>,
}

/// Réglages de l'indexation : ceux de la configuration, ou les défauts sans configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexOptions {
    /// Nombre d'appels LLM simultanés.
    pub concurrency: usize,
    /// Nombre de fils d'extraction (REQ-071) : `--jobs`, sinon `default_jobs()`.
    pub jobs: usize,
    pub max_chunk_chars: usize,
    /// Nombre maximal de fichiers par appel LLM (REQ-027).
    pub batch_files: usize,
    /// Nouveaux essais d'un appel LLM dont la réponse est non conforme (REQ-038).
    pub format_retries: u32,
    /// Enrichit aussi les fichiers de code : option ou clé `enrich_code` (REQ-050).
    pub enrich_code: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        IndexOptions {
            concurrency: DEFAULT_CONCURRENCY,
            jobs: crate::index::default_jobs(),
            max_chunk_chars: DEFAULT_MAX_CHUNK_CHARS,
            batch_files: crate::config::DEFAULT_BATCH_FILES,
            format_retries: crate::config::DEFAULT_FORMAT_RETRIES,
            enrich_code: false,
        }
    }
}

impl From<&Config> for IndexOptions {
    fn from(c: &Config) -> Self {
        IndexOptions {
            concurrency: c.concurrency,
            jobs: crate::index::default_jobs(),
            max_chunk_chars: c.max_chunk_chars,
            batch_files: c.batch_files,
            format_retries: c.format_retries,
            enrich_code: c.enrich_code,
        }
    }
}

/// Réglages et fournisseur LLM (absent : indexation sans enrichissement).
pub(crate) struct Setup {
    pub opts: IndexOptions,
    pub llm: Option<Box<dyn Llm>>,
}

/// Prépare une commande d'indexation : avec `--no-llm` la configuration n'est pas lue ; sans
/// fichier de configuration le LLM est inactif ; sinon validation (la clé n'est constatée qu'au premier appel, REQ-054).
pub(crate) fn setup(
    no_llm: bool,
    enrich_code: bool,
    project: &ProjectSettings,
    batch_files: Option<usize>,
    format_retries: Option<u32>,
) -> Result<Setup> {
    if no_llm {
        trace!("config: llm inactif (--no-llm)");
        return Ok(Setup {
            opts: IndexOptions {
                enrich_code,
                ..IndexOptions::default()
            },
            llm: None,
        });
    }
    match Config::load_optional()? {
        None => {
            trace!("config: llm inactif (aucune configuration)");
            Ok(Setup {
                opts: IndexOptions {
                    enrich_code,
                    ..IndexOptions::default()
                },
                llm: None,
            })
        }
        Some(config) => {
            let (mut config, from_project) =
                config.with_project(project, batch_files, format_retries);
            config.enrich_code |= enrich_code;
            let mut tls_trace = String::new();
            if let Some(p) = &config.tls_ca_cert {
                tls_trace.push_str(&format!(" tls_ca_cert={}", p.display()));
            }
            if config.tls_insecure {
                tls_trace.push_str(" tls_insecure");
            }
            if !from_project.is_empty() {
                tls_trace.push_str(&format!(" (.nephila.toml : {})", from_project.join(",")));
            }
            trace!(
                "config: llm={} model={} enrich_code={} thinking={} effort={} max_output_tokens={} concurrency={} timeout_secs={} max_chunk_chars={} batch_files={} format_retries={}{}",
                config.provider.as_str(),
                config.model,
                config.enrich_code,
                config.thinking,
                config.effort.as_str(),
                config.max_output_tokens,
                config.concurrency,
                config.timeout_secs,
                config.max_chunk_chars,
                config.batch_files,
                config.format_retries,
                tls_trace
            );
            let llm = build_llm(&config)?;
            Ok(Setup {
                opts: IndexOptions::from(&config),
                llm: Some(llm),
            })
        }
    }
}

impl BuildReport {
    pub fn line(&self) -> String {
        self.line_for("build")
    }

    pub fn line_for(&self, verb: &str) -> String {
        let mut line = format!(
            "{verb} terminé : {} fichiers traités, {} nœuds, {} liens, {} références non résolues, {} fichiers en attente, {} fichiers à enrichir",
            self.processed, self.nodes, self.edges, self.unresolved, self.pending, self.to_enrich
        );
        if !self.llm_active {
            line.push_str(", enrichissement : désactivé");
        }
        if let Some(k) = self.excluded {
            line.push_str(&format!(", {k} fichiers exclus par les filtres"));
        }
        line
    }
}

/// Options de la ligne de commande qui remplacent les réglages (REQ-071, REQ-028, REQ-038).
#[derive(Debug, Clone, Copy, Default)]
pub struct Overrides {
    pub jobs: Option<usize>,
    pub batch_files: Option<usize>,
    pub format_retries: Option<u32>,
}

/// Point d'entrée de `nephila build` : valide le dépôt, la configuration et la clé, puis construit.
pub fn run(
    repo: &Path,
    no_llm: bool,
    enrich_code: bool,
    verbose: bool,
    over: Overrides,
    cli_filters: Filters,
) -> Result<()> {
    trace::init(verbose);
    check_repo(repo)?;
    // Le fichier de projet est lu avant la configuration LLM, le verrou et toute écriture.
    let project = ProjectFile::load(repo)?;
    let matcher = effective_matcher(repo, &project.filters, &cli_filters)?;
    let mut setup = setup(
        no_llm,
        enrich_code,
        &project.settings,
        over.batch_files,
        over.format_retries,
    )?;
    if let Some(j) = over.jobs {
        setup.opts.jobs = j;
    }
    // Le verrou précède toute écriture, mais suit les contrôles qui ne doivent rien créer.
    let lock = RepoLock::acquire(repo)?;
    let report = build(repo, &setup.opts, setup.llm.as_deref(), &lock, &matcher)?;
    let done = finish(&report, "build");
    warn_transient(&cli_filters);
    done
}

/// Filtres effectifs (DEC-045) : fichier de projet puis options de la commande.
pub(crate) fn effective_matcher(
    repo: &Path,
    project_filters: &Filters,
    cli_filters: &Filters,
) -> Result<Matcher> {
    project_filters
        .clone()
        .merge(cli_filters.clone())
        .compile(repo)
}

/// Avertissement A-3 : les options de filtre ne valent que pour la commande en cours. Écrit une
/// seule fois, après le bilan, seulement si une option a été donnée.
pub(crate) fn warn_transient(cli_filters: &Filters) {
    if cli_filters.is_active() {
        warn("--exclude et --exclude-type valent pour cette commande seulement : status et update sans option ne les appliquent pas (filtre durable : .nephila.toml)");
    }
}

/// Affiche le bilan ; une clé refusée est rendue telle quelle (REQ-054) ; un fichier resté en attente, un fichier à enrichir (LLM actif) ou un arrêt
/// de l'envoi rend une erreur (code non nul).
pub(crate) fn finish(report: &BuildReport, verb: &str) -> Result<()> {
    println!("{}", report.line_for(verb));
    if let Some(message) = &report.key_refused {
        return Err(Error::InvalidKey(message.clone()));
    }
    let mut problems = Vec::new();
    if let Some(cause) = &report.stop_cause {
        problems.push(format!("envoi interrompu : {cause}"));
    }
    if report.pending > 0 {
        problems.push(format!("{} fichier(s) en attente", report.pending));
    }
    if report.llm_active && report.to_enrich > 0 {
        problems.push(format!("{} fichier(s) à enrichir", report.to_enrich));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Message(problems.join(" ; ")))
    }
}

/// Une passe de résolution, résumé global compris (une transaction), tracée.
fn resolve_pass(store: &mut Store) -> Result<resolve::ResolveStats> {
    let started = Instant::now();
    let stats = resolve::resolve(store)?;
    trace!(
        "résolution: {} ms, {} liens, {} non résolues",
        started.elapsed().as_millis(),
        stats.edges,
        stats.unresolved
    );
    Ok(stats)
}

pub(crate) fn check_repo(repo: &Path) -> Result<()> {
    if !repo.is_dir() {
        return Err(Error::Message(format!(
            "{} n'est pas un dossier lisible",
            repo.display()
        )));
    }
    std::fs::read_dir(repo).map_err(|e| {
        Error::Message(format!(
            "{} n'est pas un dossier lisible : {e}",
            repo.display()
        ))
    })?;
    Ok(())
}

/// Construit le graphe de `repo` (DEC-019) : préparation, phase structure, résolution et résumé
/// global, puis, `llm` présent, phase enrichissement suivie d'une nouvelle résolution. Le verrou
/// d'écriture, pris par l'appelant, prouve l'exclusivité avant toute écriture.
pub fn build(
    repo: &Path,
    opts: &IndexOptions,
    llm: Option<&dyn Llm>,
    _lock: &RepoLock,
    matcher: &Matcher,
) -> Result<BuildReport> {
    build_with_budget(repo, opts, llm, _lock, matcher, scan::RETAIN_BUDGET)
}

/// `build` avec un budget de contenus retenus explicite (réglé par les tests seulement).
pub(crate) fn build_with_budget(
    repo: &Path,
    opts: &IndexOptions,
    llm: Option<&dyn Llm>,
    _lock: &RepoLock,
    matcher: &Matcher,
    budget: usize,
) -> Result<BuildReport> {
    check_repo(repo)?;
    let mut store = Store::open_or_create(repo)?;
    let known = if db_path(repo).is_file() {
        store.files()?
    } else {
        Vec::new()
    };
    let scanned = scan::scan_with_budget(repo, &known, matcher, opts.jobs, budget)?;
    let records: Vec<FileRecord> = scanned
        .files
        .iter()
        .map(|f| FileRecord {
            path: f.path.clone(),
            hash: f.hash.clone(),
            size: f.size,
            mtime: f.mtime,
            recorded_at: 0,
            state: FileState::Pending,
        })
        .collect();
    trace!(
        "parcours: {} fichiers{}",
        records.len(),
        matcher.trace_suffix(scanned.excluded)
    );
    store.reset_with_pending_files(&records)?;
    if let Some(llm) = llm {
        store.set_enrich_model(llm.model())?;
        store.set_enrich_code(opts.enrich_code)?;
    }
    if records.is_empty() {
        warn("aucun fichier pris en charge dans le dépôt : graphe vide");
    }

    let mut report = BuildReport {
        excluded: matcher.is_active().then_some(scanned.excluded),
        ..BuildReport::default()
    };
    let abs = abs_paths(&scanned.files);
    structure::analyze(
        repo,
        scanned.files,
        opts.jobs,
        &scanned.gradle_settings,
        &mut store,
        &mut report,
    )?;
    finalize(&mut store, llm, opts, repo, &abs, &mut report)?;
    Ok(report)
}

/// Chemin converti (celui du graphe) vers chemin réel, pour les lectures (DEC-070).
pub(crate) fn abs_paths(files: &[crate::scan::ScannedFile]) -> HashMap<String, PathBuf> {
    files
        .iter()
        .map(|f| (f.path.clone(), f.abs.clone()))
        .collect()
}

/// Fin commune à `build` et `update` : résolution et résumé global (graphe interrogeable), puis
/// enrichissement des fichiers à enrichir si `llm` est présent, et bilan.
pub(crate) fn finalize(
    store: &mut Store,
    llm: Option<&dyn Llm>,
    opts: &IndexOptions,
    repo: &Path,
    abs: &HashMap<String, PathBuf>,
    report: &mut BuildReport,
) -> Result<()> {
    store.prune_enrichments()?;
    let mut stats = resolve_pass(store)?;
    report.llm_active = llm.is_some();
    if let Some(llm) = llm {
        let files = store.files_to_enrich()?;
        // Sans fichier à enrichir, le graphe n'a pas changé : pas de seconde résolution.
        if !files.is_empty() {
            enrich::enrich_all(repo, abs, &files, opts, llm, store, report)?;
            stats = resolve_pass(store)?;
        }
    }
    report.nodes = store.count_nodes()?;
    report.edges = stats.edges;
    report.unresolved = stats.unresolved;
    report.pending = store
        .files()?
        .iter()
        .filter(|f| f.state == FileState::Pending)
        .count();
    report.to_enrich = store.count_files_to_enrich()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::reads;
    use std::sync::atomic::Ordering;

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dest = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &dest);
            } else {
                std::fs::copy(e.path(), dest).unwrap();
            }
        }
    }

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel)
    }

    /// Export normalisé (DEC-081) : `unresolved` trié, doublons conservés.
    fn normalized_export(repo: &Path) -> String {
        let store = Store::open_readonly(repo).unwrap();
        let mut doc = crate::export::build_json(&store).unwrap();
        if let Some(list) = doc.get_mut("unresolved").and_then(|u| u.as_array_mut()) {
            let key = |v: &serde_json::Value| {
                let f = |k: &str| v.get(k).map(|x| x.to_string()).unwrap_or_default();
                (f("source"), f("kind"), f("target"))
            };
            list.sort_by_key(key);
        }
        serde_json::to_string(&doc).unwrap()
    }

    fn indexed_paths(repo: &Path) -> Vec<String> {
        let store = Store::open_readonly(repo).unwrap();
        store.files().unwrap().into_iter().map(|f| f.path).collect()
    }

    fn do_build(repo: &Path, budget: usize) -> BuildReport {
        let lock = RepoLock::acquire(repo).unwrap();
        build_with_budget(
            repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
            budget,
        )
        .unwrap()
    }

    fn do_update(repo: &Path) {
        let lock = RepoLock::acquire(repo).unwrap();
        crate::index::update::update(
            repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap();
    }

    /// AC-073-1 : chaque fichier pris en charge est lu une fois ; graphe identique à celui d'un
    /// budget nul (aucun contenu retenu : deux lectures par fichier).
    #[test]
    fn req073_ac1_each_file_read_once_same_graph_as_zero_budget() {
        for rel in ["maven3/repo", "gradle/groovy", "js/mixed"] {
            let dir = tempfile::tempdir().unwrap();
            let one = dir.path().join("one");
            let zero = dir.path().join("zero");
            copy_dir(&fixture(rel), &one);
            copy_dir(&fixture(rel), &zero);

            reads::reset();
            do_build(&one, scan::RETAIN_BUDGET);
            let paths = indexed_paths(&one);
            assert!(!paths.is_empty(), "{rel}");
            let got = reads::all();
            for p in &paths {
                assert_eq!(got.get(p), Some(&1), "{rel} : {p} lu {:?} fois", got.get(p));
            }
            assert_eq!(reads::kind(reads::Kind::Extract), 0, "{rel}");
            assert_eq!(reads::kind(reads::Kind::Settings), 0, "{rel}");

            reads::reset();
            do_build(&zero, 0);
            let got0 = reads::all();
            for p in &paths {
                assert!(got0.get(p).copied().unwrap_or(0) >= 2, "{rel} : {p}");
            }
            assert_eq!(normalized_export(&one), normalized_export(&zero), "{rel}");
        }
    }

    /// AC-073-2 : second build puis update.
    #[test]
    fn req073_ac2_second_build_and_update_read_each_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        copy_dir(&fixture("maven3/repo"), &repo);
        do_build(&repo, scan::RETAIN_BUDGET);
        let paths = indexed_paths(&repo);

        reads::reset();
        do_build(&repo, scan::RETAIN_BUDGET);
        let got = reads::all();
        for p in &paths {
            assert_eq!(got.get(p), Some(&1), "second build : {p}");
        }

        let java = paths.iter().find(|p| p.ends_with(".java")).unwrap().clone();
        let mut text = std::fs::read_to_string(repo.join(&java)).unwrap();
        text.push_str("\n// modifié\n");
        std::fs::write(repo.join(&java), text).unwrap();
        std::fs::write(repo.join("G.java"), "package p;\npublic class G {}\n").unwrap();
        reads::reset();
        do_update(&repo);
        let got = reads::all();
        assert_eq!(got.get(&java), Some(&1), "fichier modifié");
        assert_eq!(got.get("G.java"), Some(&1), "fichier ajouté");
        for (p, n) in &got {
            assert!(*n <= 1, "{p} lu {n} fois");
        }
    }

    /// AC-073-3 : budget de 1 000 octets, 5 fichiers de 400 octets.
    #[test]
    fn req073_ac3_budget_bounds_retained_contents() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small");
        let big = dir.path().join("big");
        for r in [&small, &big] {
            std::fs::create_dir_all(r).unwrap();
            for i in 0..5 {
                let mut t = format!("# Titre {i}\n\n");
                while t.len() < 399 {
                    t.push('x');
                }
                t.push('\n');
                assert_eq!(t.len(), 400);
                std::fs::write(r.join(format!("d{i}.md")), t).unwrap();
            }
        }
        reads::reset();
        scan::RETAINED_PEAK.store(0, Ordering::SeqCst);
        do_build(&small, 1_000);
        let peak = scan::RETAINED_PEAK.load(Ordering::SeqCst);
        assert!(peak > 0 && peak <= 1_000, "pic {peak}");
        assert_eq!(reads::kind(reads::Kind::Hash), 5);
        assert_eq!(reads::kind(reads::Kind::Extract), 3);
        do_build(&big, scan::RETAIN_BUDGET);
        assert_eq!(normalized_export(&small), normalized_export(&big));
    }

    /// AC-073-4 : contenu réécrit (même taille) entre `scan` et `analyze`.
    #[test]
    fn req073_ac4_analysis_uses_the_content_of_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let before = "package p;\npublic class A {}\n";
        let after = "package p;\npublic class B {}\n";
        assert_eq!(before.len(), after.len());
        std::fs::write(repo.join("A.java"), before).unwrap();
        let scanned = scan::scan(repo, &[], &Matcher::none(), 1).unwrap();
        let mut store = Store::open_or_create(repo).unwrap();
        let records: Vec<FileRecord> = scanned
            .files
            .iter()
            .map(|f| FileRecord {
                path: f.path.clone(),
                hash: f.hash.clone(),
                size: f.size,
                mtime: f.mtime,
                recorded_at: 0,
                state: FileState::Pending,
            })
            .collect();
        store.reset_with_pending_files(&records).unwrap();
        std::fs::write(repo.join("A.java"), after).unwrap();
        let mut report = BuildReport::default();
        structure::analyze(
            repo,
            scanned.files,
            1,
            &scanned.gradle_settings,
            &mut store,
            &mut report,
        )
        .unwrap();
        let ids: Vec<String> = store.nodes().unwrap().into_iter().map(|n| n.id).collect();
        assert!(ids.contains(&"type:p.A".to_string()), "{ids:?}");
        assert!(!ids.contains(&"type:p.B".to_string()), "{ids:?}");
        let rec = store.files().unwrap().remove(0);
        assert_eq!(
            rec.hash,
            blake3::hash(before.as_bytes()).to_hex().to_string()
        );
        drop(store);

        // Changement de date : l'update suivant analyse le nouvel état.
        let f = std::fs::File::options()
            .write(true)
            .open(repo.join("A.java"))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();
        drop(f);
        do_update(repo);
        let store = Store::open_readonly(repo).unwrap();
        let ids: Vec<String> = store.nodes().unwrap().into_iter().map(|n| n.id).collect();
        assert!(ids.contains(&"type:p.B".to_string()), "{ids:?}");
        assert!(!ids.contains(&"type:p.A".to_string()), "{ids:?}");
        let rec = store.files().unwrap().remove(0);
        assert_eq!(
            rec.hash,
            blake3::hash(after.as_bytes()).to_hex().to_string()
        );
    }
}
