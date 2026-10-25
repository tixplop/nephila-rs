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
use crate::out::Out;
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
    out: &mut Out,
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
    write_report(out, &report, "build")?;
    let done = finish(&report);
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

/// Écrit la ligne de bilan sur stdout ; en cas d'échec (E-17), la commande s'arrête là.
pub(crate) fn write_report(out: &mut Out, report: &BuildReport, verb: &str) -> Result<()> {
    writeln!(out, "{}", report.line_for(verb))
}

/// Erreurs de fin de commande ; une clé refusée est rendue telle quelle (REQ-054) ; un fichier resté en attente, un fichier à enrichir (LLM actif) ou un arrêt
/// de l'envoi rend une erreur (code non nul).
pub(crate) fn finish(report: &BuildReport) -> Result<()> {
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
    let stats = resolve::resolve(store).map_err(|e| pass_error(&e))?;
    let kind = match stats.pass {
        resolve::Pass::Full => "complète".to_string(),
        resolve::Pass::Incremental { reevaluated } => {
            format!("incrémentale, {reevaluated} références réévaluées")
        }
    };
    trace!(
        "résolution: {} ms, {} liens, {} non résolues ({kind})",
        started.elapsed().as_millis(),
        stats.edges,
        stats.unresolved
    );
    Ok(stats)
}

/// Erreur E-18 (V-006) : phase nommée et remède, sur une seule ligne ; seul point qui pose ce texte.
fn pass_error(cause: &Error) -> Error {
    let text = cause.to_string();
    let first = text.lines().next().unwrap_or("").trim_end();
    Error::Message(format!(
        "résolution : {first} ; lancez nephila build pour reconstruire le graphe"
    ))
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
    fn sink() -> Out {
        Out::new(Box::new(Vec::new()))
    }

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
        let mut doc = oracle_build_json(&store).unwrap();
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

    /// Export de la 0.23.2 (`build_json` de HEAD avant REQ-080), résolution complète comprise :
    /// oracle d'AC-080-1 et d'AC-080-2.
    fn oracle_build_json(store: &Store) -> Result<serde_json::Value> {
        use serde_json::json;
        let nodes: Vec<serde_json::Value> = store
            .nodes()?
            .into_iter()
            .map(|n| {
                json!({
                    "id": n.id,
                    "kind": n.kind.as_str(),
                    "qualified": n.qualified,
                    "short": n.short,
                    "file": n.file,
                    "subkind": n.subkind.map(|s| s.as_str()),
                    "summary": n.summary,
                })
            })
            .collect();
        let edges: Vec<serde_json::Value> = store
            .edges()?
            .into_iter()
            .map(|e| json!({"source": e.source, "target": e.target, "kind": e.kind.as_str()}))
            .collect();
        let unresolved: Vec<serde_json::Value> = resolve::unresolved(store)?
            .into_iter()
            .map(|r| json!({"source": r.source, "kind": r.kind.as_str(), "target": r.target_name}))
            .collect();
        Ok(
            json!({"version": crate::export::EXPORT_VERSION, "nodes": nodes, "edges": edges, "unresolved": unresolved}),
        )
    }

    /// Dépôts de REQ-065 (liste de `tests/sqlite_lot3.rs`) : dossier et dossier du mock.
    const REQ065_REPOS: &[(&str, Option<&str>)] = &[
        ("maven3/repo", Some("maven3/mock")),
        ("maven2", Some("mock")),
        ("gradle/edge", None),
        ("gradle/groovy", None),
        ("gradle/kotlin", None),
        ("ivy", None),
        ("data/repo", Some("data/mock")),
        ("js/mixed", Some("js/mock")),
        ("langs/mixed", None),
    ];

    /// `build` avec le mock (enrichissement du code compris) quand `mock` est donné.
    fn build_with_mock(repo: &Path, mock: Option<&Path>) {
        let llm = mock.map(|d| {
            crate::llm::mock::MockLlm::new(crate::llm::mock::MockOptions {
                dir: Some(d.to_path_buf()),
                ..Default::default()
            })
        });
        let opts = IndexOptions {
            enrich_code: true,
            ..IndexOptions::default()
        };
        let lock = RepoLock::acquire(repo).unwrap();
        build_with_budget(
            repo,
            &opts,
            llm.as_ref().map(|l| l as &dyn Llm),
            &lock,
            &Matcher::none(),
            scan::RETAIN_BUDGET,
        )
        .unwrap();
    }

    fn graph_db_hash(repo: &Path) -> String {
        let bytes = std::fs::read(crate::store::db_path(repo)).unwrap();
        blake3::hash(&bytes).to_hex().to_string()
    }

    /// Schéma 7 de la 0.21.0 (copie de l'assistant du module de tests du stockage).
    const SCHEMA7_DOWNGRADE: &str = "BEGIN;
        DROP INDEX idx_refs_key;
        DROP INDEX idx_refs_head;
        DROP INDEX idx_refs_unresolved;
        ALTER TABLE refs DROP COLUMN target_key;
        ALTER TABLE refs DROP COLUMN target_head;
        ALTER TABLE refs DROP COLUMN unresolved;
        DELETE FROM meta WHERE key = 'resolution';
        UPDATE meta SET value = '7' WHERE key = 'schema_version';
        PRAGMA user_version = 7;
        COMMIT;";

    fn exec(repo: &Path, sql: &str) {
        rusqlite::Connection::open(crate::store::db_path(repo))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    /// AC-080-1 : l'export en flux est celui de la 0.23.2, octet pour octet (flux en mémoire, puis
    /// fichier écrit par la commande), pour les dépôts de REQ-065 et pour un graphe vide.
    #[test]
    fn req080_ac1_stream_equals_the_0_23_2_export_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cases: Vec<(String, PathBuf)> = Vec::new();
        for (i, (rel, mock)) in REQ065_REPOS.iter().enumerate() {
            let repo = tmp.path().join(format!("r{i}"));
            copy_dir(&fixture(rel), &repo);
            let mock = mock.map(fixture);
            build_with_mock(&repo, mock.as_deref());
            cases.push((rel.to_string(), repo));
        }
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        drop(Store::open_or_create(&empty).unwrap());
        cases.push(("graphe vide".into(), empty));
        for (name, repo) in cases {
            let store = Store::open_readonly(&repo).unwrap();
            let expected = oracle_build_json(&store).unwrap().to_string();
            let mut out: Vec<u8> = Vec::new();
            crate::export::write_json(&store, &mut out).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), expected, "{name}");
            drop(store);
            let file = tmp.path().join("out.json");
            crate::export::run(
                &repo,
                Some(&file),
                crate::cli::ExportFormat::Json,
                &mut sink(),
            )
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                format!("{expected}\n"),
                "{name} (fichier)"
            );
        }
    }

    /// AC-080-2 : une passe de résolution seulement si le graphe n'est pas `à jour` de schéma 8 ;
    /// jamais pour la page HTML ; le graphe n'est jamais modifié.
    #[test]
    fn req080_ac2_export_resolves_only_a_graph_that_is_not_current() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixture("maven2"), &repo);
        build_with_mock(&repo, Some(&fixture("mock")));
        let json = tmp.path().join("out.json");
        let html = tmp.path().join("out.html");
        let slug = crate::export::slug(&repo);
        let states: [(&str, Option<&str>, usize); 3] = [
            ("à jour", None, 0),
            (
                "à résoudre",
                Some("UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'"),
                1,
            ),
            ("schéma 7", Some(SCHEMA7_DOWNGRADE), 1),
        ];
        for (label, change, passes) in states {
            if let Some(sql) = change {
                exec(&repo, sql);
            }
            let before = graph_db_hash(&repo);
            resolve::passes::reset();
            crate::export::run(
                &repo,
                Some(&json),
                crate::cli::ExportFormat::Json,
                &mut sink(),
            )
            .unwrap();
            assert_eq!(resolve::passes::count(), passes, "{label} : json");
            resolve::passes::reset();
            crate::export::run(
                &repo,
                Some(&html),
                crate::cli::ExportFormat::Html,
                &mut sink(),
            )
            .unwrap();
            assert_eq!(resolve::passes::count(), 0, "{label} : html");
            assert_eq!(graph_db_hash(&repo), before, "{label} : graph.db modifié");
            let store = Store::open_readonly(&repo).unwrap();
            let doc = oracle_build_json(&store).unwrap();
            assert_eq!(
                std::fs::read_to_string(&json).unwrap(),
                format!("{doc}\n"),
                "{label}"
            );
            assert_eq!(
                std::fs::read_to_string(&html).unwrap(),
                format!("{}\n", crate::view::render(&slug, &doc)),
                "{label} : page"
            );
        }
    }

    /// Écrivain qui accepte `limit` octets puis échoue (AC-080-4, AC-080-7).
    struct FailAfter {
        limit: usize,
        written: usize,
    }

    impl std::io::Write for FailAfter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.written >= self.limit {
                return Err(std::io::Error::other("disque plein simulé"));
            }
            let n = buf.len().min(self.limit - self.written);
            self.written += n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// AC-080-4 et AC-080-7 : une écriture interrompue donne une erreur rendue sur une ligne, et le
    /// fichier commencé est supprimé (JSON comme HTML) ; vers stdout, le texte d'E-17.
    #[test]
    fn req080_ac4_ac7_failing_writer_removes_the_file_and_renders_the_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixture("maven2"), &repo);
        build_with_mock(&repo, Some(&fixture("mock")));
        let wrap: &dyn Fn(Box<dyn std::io::Write>) -> Box<dyn std::io::Write> = &|_real| {
            Box::new(FailAfter {
                limit: 100,
                written: 0,
            })
        };
        for format in [
            crate::cli::ExportFormat::Json,
            crate::cli::ExportFormat::Html,
        ] {
            let file = tmp.path().join("out.export");
            let err = crate::export::run_wrapped(&repo, Some(&file), format, wrap, &mut sink())
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "écriture de {} impossible : disque plein simulé",
                    file.display()
                ),
                "{format:?}"
            );
            assert!(!file.exists(), "{format:?} : fichier laissé");
            let err =
                crate::export::run_wrapped(&repo, Some(Path::new("-")), format, wrap, &mut sink())
                    .unwrap_err();
            assert_eq!(
                err.to_string(),
                "écriture sur stdout impossible : disque plein simulé",
                "{format:?}"
            );
        }
        // Un export du même nom qui existait est perdu (DEC-092, sans fichier temporaire).
        let file = tmp.path().join("old.json");
        std::fs::write(&file, "ancien").unwrap();
        crate::export::run_wrapped(
            &repo,
            Some(&file),
            crate::cli::ExportFormat::Json,
            wrap,
            &mut sink(),
        )
        .unwrap_err();
        assert!(!file.exists());
    }

    /// AC-089-2 : une panne de la base pendant la passe, complète comme incrémentale, donne le texte
    /// E-18 exact ; `build` et `update` passent par `resolve_pass` (lecture de `finalize`).
    #[test]
    fn req089_ac2_failing_pass_renders_the_named_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixture("maven3/repo"), &repo);
        build_with_mock(&repo, None);
        let trigger = "CREATE TRIGGER boom BEFORE INSERT ON edges \
            BEGIN SELECT RAISE(ABORT, 'panne simulée'); END;";
        exec(&repo, trigger);
        // SQLite rend `panne simulée` seul pour un RAISE(ABORT) : le texte exact en découle.
        let expect = |err: Error| {
            let text = err.to_string();
            assert!(
                text.starts_with("résolution : base du graphe : ")
                    && text.contains("panne simulée")
                    && text.ends_with(" ; lancez nephila build pour reconstruire le graphe")
                    && !text.contains('\n'),
                "{text}"
            );
            assert_eq!(text.matches(" ; lancez").count(), 1, "{text}");
        };
        // Passe complète : une référence à lier est écrite, graphe à résoudre (aucun journal).
        exec(
            &repo,
            "UPDATE refs SET unresolved = 1 WHERE id = (SELECT MIN(id) FROM refs);
             UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'",
        );
        let mut store = Store::open_or_create(&repo).unwrap();
        crate::store::reset_diagnostics();
        expect(resolve_pass(&mut store).unwrap_err());
        let d = crate::store::diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (1, 0));
        drop(store);
        // Passe incrémentale : graphe résolu, un fichier modifié, `update`.
        exec(&repo, "DROP TRIGGER boom");
        let mut store = Store::open_or_create(&repo).unwrap();
        resolve_pass(&mut store).unwrap();
        drop(store);
        exec(&repo, trigger);
        let java = repo.join("core/src/main/java/com/acme/core/OrderService.java");
        let mut text = std::fs::read_to_string(&java).unwrap();
        text.push_str("\nclass Added { void run() { new OrderService(); } }\n");
        std::fs::write(&java, text).unwrap();
        let lock = RepoLock::acquire(&repo).unwrap();
        crate::store::reset_diagnostics();
        let err = crate::index::update::update(
            &repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap_err();
        expect(err);
        let d = crate::store::diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (0, 1));
    }
}
