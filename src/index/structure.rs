//! Phase structure (DEC-019) : analyse syntaxique parallèle des fichiers, une transaction par
//! fichier écrite par le thread principal (seul écrivain, DEC-013).

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::mpsc;
use std::sync::Mutex;

use crate::error::{warn, Error, Result};
use crate::index::build::BuildReport;
use crate::model::{Node, NodeKind, Ref};
use crate::parse::{self, FileStructure, ParseCtx};
use crate::progress::{self, Phase};
use crate::scan::ScannedFile;
use crate::store::Store;
use crate::trace;

/// Pile des fils d'analyse : filet pour les récursions restantes (DEC-054).
const EXTRACT_STACK: usize = 16 * 1024 * 1024;

/// Nombre de fichiers enregistrés par transaction.
const BATCH: usize = 200;

type Batched = (String, Vec<Node>, Vec<Ref>);

enum Outcome {
    Parsed(String, FileStructure),
    Failed(String, Error),
}

/// Analyse `files` (prises par valeur : le contenu retenu par le parcours est pris par le fil
/// d'extraction puis libéré, REQ-073) avec `jobs` fils et enregistre leur structure. Un fichier illisible ou
/// dont l'analyse échoue reste « en attente » (avertissement nommant la cause) ; les autres
/// continuent. `gradle_settings` : table des `settings.gradle(.kts)` du parcours.
pub(crate) fn analyze(
    repo: &Path,
    files: Vec<ScannedFile>,
    jobs: usize,
    gradle_settings: &HashMap<String, Option<String>>,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    let ctx = ParseCtx {
        // Chemin absolu : `.` n'a pas de nom de dossier (module Gradle racine).
        repo_root: Some(repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf())),
        gradle_settings: gradle_settings.clone(),
    };
    let total = files.len();
    let queue: Mutex<VecDeque<ScannedFile>> = Mutex::new(files.into());
    let (tx, rx) = mpsc::channel::<Outcome>();
    let workers = jobs.clamp(1, total.max(1));
    let mut write_error: Option<Error> = None;
    let started = std::time::Instant::now();
    let failed_before = report.failed;
    let retained_before = report.retained;
    trace!("structure: début {total} fichiers, {workers} fils");
    let mut batch: Vec<Batched> = Vec::new();

    let mut meter = progress::Meter::start(Phase::Extraction, total);
    let mut spawn_error: Option<Error> = None;
    std::thread::scope(|scope| {
        let queue = &queue;
        let ctx = &ctx;
        let mut started_workers = 0usize;
        for _ in 0..workers {
            let tx = tx.clone();
            let spawned = std::thread::Builder::new()
                .name("nephila-extract".into())
                .stack_size(EXTRACT_STACK)
                .spawn_scoped(scope, move || loop {
                    let Some(mut file) =
                        queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front()
                    else {
                        break;
                    };
                    // Contenu retenu par le parcours, sinon lecture par le chemin réel.
                    let outcome = match file.content.take() {
                        Some(retained) => {
                            extract(&file.path, retained.as_str(), ctx)
                            // `retained` est libéré ici : le budget est rendu.
                        }
                        None => {
                            crate::scan::count_read(&file.path, crate::scan::ReadKind::Extract);
                            match std::fs::read_to_string(&file.abs) {
                                Err(e) => Outcome::Failed(
                                    file.path.clone(),
                                    Error::Message(format!(
                                        "lecture de {} impossible : {e}",
                                        file.path
                                    )),
                                ),
                                Ok(text) => extract(&file.path, &text, ctx),
                            }
                        }
                    };
                    if tx.send(outcome).is_err() {
                        break;
                    }
                });
            match spawned {
                Ok(_) => {
                    started_workers += 1;
                    #[cfg(test)]
                    tests::EXTRACT_THREADS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                Err(e) => {
                    if started_workers == 0 {
                        queue.lock().unwrap_or_else(|p| p.into_inner()).clear();
                        spawn_error = Some(Error::Message(format!(
                            "création des fils d'analyse impossible : {e}"
                        )));
                    }
                    break;
                }
            }
        }
        drop(tx);
        for outcome in rx {
            if let Some(m) = meter.as_mut() {
                m.advance(1);
            }
            match outcome {
                Outcome::Failed(path, e) => {
                    warn(&format!("{path} : {e}"));
                    report.failed += 1;
                }
                Outcome::Parsed(path, s) => {
                    if write_error.is_some() {
                        continue;
                    }
                    for w in &s.warnings {
                        warn(w);
                    }
                    let (nodes, refs, dropped) = to_records(&path, &s);
                    if dropped > 0 {
                        warn(&format!(
                            "{path} : {dropped} référence(s) ignorée(s), source non déclarée"
                        ));
                    }
                    batch.push((path, nodes, refs));
                    if batch.len() >= BATCH {
                        flush(store, &mut batch, queue, report, &mut write_error);
                    }
                }
            }
        }
        if write_error.is_none() {
            flush(store, &mut batch, queue, report, &mut write_error);
        }
    });
    trace!(
        "structure: fin {} ms, {} en attente, {} enrichissements conservés",
        started.elapsed().as_millis(),
        report.failed - failed_before,
        report.retained - retained_before
    );
    match spawn_error.or(write_error) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn extract(path: &str, text: &str, ctx: &ParseCtx) -> Outcome {
    match parse::extract(Path::new(path), text, ctx) {
        Ok(s) => Outcome::Parsed(path.to_string(), s),
        Err(e) => Outcome::Failed(path.to_string(), e),
    }
}

/// Enregistre le lot en une transaction. Une erreur vide la file (les fils s'arrêtent, les
/// résultats restants sont ignorés) et est conservée.
fn flush(
    store: &mut Store,
    batch: &mut Vec<Batched>,
    queue: &Mutex<VecDeque<ScannedFile>>,
    report: &mut BuildReport,
    write_error: &mut Option<Error>,
) {
    if batch.is_empty() {
        return;
    }
    match store.replace_structures_counted(batch) {
        Ok(retained) => {
            report.processed += batch.len();
            report.retained += retained;
        }
        Err(e) => {
            queue.lock().unwrap_or_else(|p| p.into_inner()).clear();
            *write_error = Some(e);
        }
    }
    batch.clear();
}

/// Nom court d'un nœud, selon son genre.
fn short_name(kind: NodeKind, qualified: &str) -> &str {
    let after = |seps: &[char]| qualified.rsplit(seps).next().unwrap_or(qualified);
    match kind {
        NodeKind::Document => after(&['/']),
        NodeKind::Module => after(&[':', '/']),
        NodeKind::Section => after(&['#']),
        NodeKind::Dependency => after(&[':']),
        // Chemin de clé : tout ce qui suit le premier `#` (un nom de membre peut en contenir).
        NodeKind::Key => qualified.split_once('#').map_or(qualified, |(_, k)| k),
        NodeKind::Table => after(&['.']),
        _ => after(&['.']),
    }
}

/// Convertit la structure d'un fichier en nœuds et références rattachés à ce fichier. Les
/// références dont la source n'est pas déclarée sont écartées (leur nombre est renvoyé).
fn to_records(path: &str, x: &FileStructure) -> (Vec<Node>, Vec<Ref>, usize) {
    let mut nodes: Vec<Node> = Vec::new();
    for d in &x.declares {
        let id = d.id();
        if nodes.iter().any(|n| n.id == id) {
            continue;
        }
        // Dépendances externes et concepts n'ont pas de fichier propre.
        let file = match d.kind {
            NodeKind::Dependency | NodeKind::Concept => None,
            _ => Some(path),
        };
        nodes.push(Node::new(
            d.kind,
            &d.qualified,
            d.short
                .as_deref()
                .unwrap_or_else(|| short_name(d.kind, &d.qualified)),
            file,
            d.subkind,
            d.summary.as_deref(),
        ));
    }
    let mut refs = Vec::new();
    let mut dropped = 0;
    for r in &x.references {
        if !nodes.iter().any(|n| n.id == r.source) {
            dropped += 1;
            continue;
        }
        refs.push(Ref {
            source: r.source.clone(),
            kind: r.kind,
            target_name: r.target.clone(),
            context: x.context.clone(),
        });
    }
    (nodes, refs, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Declared;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Compteur de diagnostic (AC-071-3) : fils d'extraction lancés, tous appels confondus.
    pub(super) static EXTRACT_THREADS: AtomicUsize = AtomicUsize::new(0);

    /// Fils d'extraction lancés par `analyze` sur `n` fichiers avec `jobs`.
    fn threads_for(n: usize, jobs: usize) -> usize {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for i in 0..n {
            std::fs::write(
                repo.join(format!("C{i}.java")),
                format!("package p;\npublic class C{i} {{}}\n"),
            )
            .unwrap();
        }
        let scanned = crate::scan::scan(repo, &[], &crate::filter::Matcher::none(), 1).unwrap();
        assert_eq!(scanned.files.len(), n);
        let mut store = Store::open_or_create(repo).unwrap();
        let records: Vec<crate::model::FileRecord> = scanned
            .files
            .iter()
            .map(|f| crate::model::FileRecord {
                path: f.path.clone(),
                hash: f.hash.clone(),
                size: f.size,
                mtime: f.mtime,
                recorded_at: 0,
                state: crate::model::FileState::Pending,
            })
            .collect();
        store.reset_with_pending_files(&records).unwrap();
        let before = EXTRACT_THREADS.load(Ordering::SeqCst);
        let mut report = BuildReport::default();
        analyze(
            repo,
            scanned.files,
            jobs,
            &scanned.gradle_settings,
            &mut store,
            &mut report,
        )
        .unwrap();
        EXTRACT_THREADS.load(Ordering::SeqCst) - before
    }

    /// AC-071-3 : le nombre de fils lancés vaut min(jobs, fichiers), au moins 1.
    #[test]
    fn req071_ac3_fils_d_extraction_lances() {
        assert_eq!(threads_for(30, 3), 3);
        assert_eq!(threads_for(5, 50), 5);
        assert_eq!(threads_for(30, 1), 1);
        let d = crate::index::default_jobs();
        assert!((1..=crate::index::MAX_JOBS).contains(&d), "{d}");
    }

    fn module(q: &str, short: Option<&str>) -> FileStructure {
        FileStructure {
            declares: vec![Declared {
                kind: NodeKind::Module,
                qualified: q.into(),
                subkind: None,
                summary: None,
                short: short.map(str::to_string),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn dec024_nom_court_porte_par_la_declaration() {
        let (n, _, _) = to_records("api/pom.xml", &module("com.acme:api", Some("api")));
        assert_eq!(n[0].short, "api");
    }

    #[test]
    fn short_name_key_et_table() {
        assert_eq!(
            short_name(NodeKind::Key, "app/application.yml#spring.datasource.url"),
            "spring.datasource.url"
        );
        assert_eq!(short_name(NodeKind::Key, "c.json#a#b.c"), "a#b.c");
        assert_eq!(short_name(NodeKind::Table, "shop.products"), "products");
        assert_eq!(short_name(NodeKind::Table, "orders"), "orders");
        assert_eq!(
            short_name(NodeKind::Document, "db/V1__init.sql"),
            "V1__init.sql"
        );
    }

    #[test]
    fn dec024_nom_court_derive_du_module() {
        let (n, _, _) = to_records("pom.xml", &module("com.acme:core", None));
        assert_eq!(n[0].short, "core");
        let (n, _, _) = to_records("pom.xml", &module("core", None));
        assert_eq!(n[0].short, "core");
        let (n, _, _) = to_records("pom.xml", &module("a/b", None));
        assert_eq!(n[0].short, "b");
    }

    /// AC-010-8 : un fichier pris en charge devenu illisible entre le parcours et l'analyse.
    #[test]
    fn req010_ac8_unreadable_file_stays_pending_others_analyzed_then_update_fixes() {
        use crate::index::build::{finalize, finish, IndexOptions};
        use crate::index::lock::RepoLock;
        use crate::index::update::update;
        use crate::model::{FileRecord, FileState};

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(repo.join("A.java"), "package p;\npublic class A {}\n").unwrap();
        std::fs::write(repo.join("B.java"), "package p;\npublic class B {}\n").unwrap();
        let scanned = crate::scan::scan(repo, &[], &crate::filter::Matcher::none(), 1).unwrap();
        assert_eq!(scanned.files.len(), 2);
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
        // La lecture de B échoue : le fichier disparaît après le parcours.
        std::fs::remove_file(repo.join("B.java")).unwrap();
        let mut report = BuildReport::default();
        crate::progress::init(true);
        crate::progress::take_captured();
        let mut files = scanned.files;
        // B.java : cas d'un fichier que l'extraction lit elle-même (contenu non retenu, REQ-073).
        for f in &mut files {
            if f.path == "B.java" {
                f.content = None;
            }
        }
        analyze(
            repo,
            files,
            2,
            &scanned.gradle_settings,
            &mut store,
            &mut report,
        )
        .unwrap();
        crate::progress::init(false);
        assert_eq!(
            crate::progress::take_captured(),
            vec![
                "progression: extraction 0 % (0/2 fichiers)",
                "progression: extraction 50 % (1/2 fichiers)",
                "progression: extraction 100 % (2/2 fichiers)",
            ]
        );
        let opts = IndexOptions::default();
        finalize(
            &mut store,
            None,
            &opts,
            repo,
            &std::collections::HashMap::new(),
            &mut report,
        )
        .unwrap();
        assert_eq!((report.processed, report.failed, report.pending), (1, 1, 1));
        let state = |store: &Store, p: &str| {
            store
                .files()
                .unwrap()
                .into_iter()
                .find(|f| f.path == p)
                .unwrap()
                .state
        };
        assert_eq!(state(&store, "A.java"), FileState::Indexed);
        assert_eq!(state(&store, "B.java"), FileState::Pending);
        let ids: Vec<String> = store.nodes().unwrap().into_iter().map(|n| n.id).collect();
        assert!(ids.contains(&"type:p.A".to_string()), "{ids:?}");
        assert!(!ids.contains(&"type:p.B".to_string()));
        assert!(finish(&report, "build").is_err());

        // Après correction, un update analyse le fichier.
        drop(store);
        std::fs::write(repo.join("B.java"), "package p;\npublic class B {}\n").unwrap();
        let lock = RepoLock::acquire(repo).unwrap();
        let report = update(repo, &opts, None, &lock, &crate::filter::Matcher::none())
            .unwrap()
            .unwrap();
        assert_eq!(report.pending, 0);
        assert!(finish(&report, "update").is_ok());
        let store = Store::open_readonly(repo).unwrap();
        assert!(store.nodes().unwrap().iter().any(|n| n.id == "type:p.B"));
    }
}
