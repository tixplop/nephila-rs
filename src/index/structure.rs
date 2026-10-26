//! Phase structure (DEC-019) : analyse syntaxique parallèle des fichiers, une transaction par
//! fichier écrite par le thread principal (seul écrivain, DEC-013).

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::mpsc;
use std::sync::Mutex;

use crate::error::{warn, Error, Result};
use crate::index::build::BuildReport;
use crate::parse::{self, ParseCtx};
use crate::progress::{self, Phase};
use crate::scan::ScannedFile;
use crate::store::{PreparedStructure, Store};
use crate::trace;

/// Pile des fils d'analyse : filet pour les récursions restantes (DEC-054).
pub(crate) const EXTRACT_STACK: usize = 16 * 1024 * 1024;

/// Nombre de fichiers enregistrés par transaction.
const BATCH: usize = 200;

enum Outcome {
    Parsed(String, Result<PreparedStructure>, Vec<String>),
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
    analyze_with(
        repo,
        files,
        jobs,
        gradle_settings,
        store,
        report,
        &crate::workers::Spawner,
    )
}

/// `analyze` avec le lanceur de fils donné (DEC-109).
pub(crate) fn analyze_with<L: crate::workers::Launcher>(
    repo: &Path,
    files: Vec<ScannedFile>,
    jobs: usize,
    gradle_settings: &HashMap<String, Option<String>>,
    store: &mut Store,
    report: &mut BuildReport,
    launcher: &L,
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
    let mut batch: Vec<PreparedStructure> = Vec::new();
    // Avertissements de la phase, rangés par chemin converti et écrits triés (REQ-104, AC-104-5).
    let mut notes: Vec<(String, Vec<String>)> = Vec::new();

    let mut meter = progress::Meter::start(Phase::Extraction, total);
    let mut spawn_error: Option<Error> = None;
    std::thread::scope(|scope| {
        let queue = &queue;
        let ctx = &ctx;
        let (started_workers, start_error) = crate::workers::start_workers(
            scope,
            workers,
            launcher,
            || {
                std::thread::Builder::new()
                    .name("nephila-extract".into())
                    .stack_size(EXTRACT_STACK)
            },
            || {
                let tx = tx.clone();
                Box::new(move || loop {
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
                })
            },
        );
        crate::diag::add(crate::diag::Counter::ExtractThreads, started_workers);
        if let Some(e) = start_error {
            if started_workers == 0 {
                queue.lock().unwrap_or_else(|p| p.into_inner()).clear();
                spawn_error = Some(Error::Message(format!(
                    "création des fils d'analyse impossible : {e}"
                )));
            }
        }
        if started_workers > 0 && started_workers < workers {
            trace!("structure: {started_workers} fils démarrés sur {workers} demandés");
        }
        drop(tx);
        for outcome in rx {
            if let Some(m) = meter.as_mut() {
                m.advance(1);
            }
            match outcome {
                Outcome::Failed(path, e) => {
                    notes.push((path.clone(), vec![format!("{path} : {e}")]));
                    report.failed += 1;
                }
                Outcome::Parsed(path, prepared, warnings) => {
                    if write_error.is_some() {
                        continue;
                    }
                    let mut lines = warnings;
                    let p = match prepared {
                        Ok(p) => p,
                        Err(e) => {
                            queue.lock().unwrap_or_else(|q| q.into_inner()).clear();
                            if !lines.is_empty() {
                                notes.push((path, lines));
                            }
                            write_error = Some(e);
                            continue;
                        }
                    };
                    if p.dropped > 0 {
                        lines.push(format!(
                            "{} : {} référence(s) ignorée(s), source non déclarée",
                            p.path, p.dropped
                        ));
                    }
                    if !lines.is_empty() {
                        notes.push((path, lines));
                    }
                    batch.push(p);
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
    // Ordre des octets du chemin ; le tri est stable, l'ordre interne d'un fichier est gardé.
    notes.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for line in notes.iter().flat_map(|(_, lines)| lines) {
        warn(line);
    }
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
        // Conversion, contrôles et clés : dans le fil d'extraction (DEC-086).
        Ok(s) => Outcome::Parsed(
            path.to_string(),
            crate::store::prepare_structure(path, &s),
            s.warnings,
        ),
        Err(e) => Outcome::Failed(path.to_string(), e),
    }
}

/// Enregistre le lot en une transaction. Une erreur vide la file (les fils s'arrêtent, les
/// résultats restants sont ignorés) et est conservée.
fn flush(
    store: &mut Store,
    batch: &mut Vec<PreparedStructure>,
    queue: &Mutex<VecDeque<ScannedFile>>,
    report: &mut BuildReport,
    write_error: &mut Option<Error>,
) {
    if batch.is_empty() {
        return;
    }
    match store.write_prepared(batch) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;
    use crate::parse::{Declared, FileStructure};
    use crate::store::prepare_structure;
    use std::sync::atomic::Ordering;

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
        let d = crate::diag::scope();
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
        d.get(crate::diag::Counter::ExtractThreads)
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

    fn write_repo(repo: &Path, n: usize) {
        for i in 0..n {
            if i % 10 == 9 {
                std::fs::write(
                    repo.join(format!("doc{i}.md")),
                    format!(
                        "# Doc {i}\n\nVoir [suite](doc{}.md) et `C{}`.\n\n## Détail\n",
                        i - 1,
                        i - 2
                    ),
                )
                .unwrap();
            } else {
                std::fs::write(
                    repo.join(format!("C{i}.java")),
                    format!(
                        "package p;\nimport q.D{i};\npublic class C{i} extends D{i} {{\n  void m() {{ C{}.go(); D{i}.run(); }}\n}}\n",
                        (i + 1) % n
                    ),
                )
                .unwrap();
            }
        }
    }

    fn pending_records(files: &[crate::scan::ScannedFile]) -> Vec<crate::model::FileRecord> {
        files
            .iter()
            .map(|f| crate::model::FileRecord {
                path: f.path.clone(),
                hash: f.hash.clone(),
                size: f.size,
                mtime: f.mtime,
                recorded_at: 0,
                state: crate::model::FileState::Pending,
            })
            .collect()
    }

    /// Contenu des tables écrites par la phase structure (identifiants de référence exclus : ils
    /// suivent l'ordre d'arrivée des fils), après la passe de résolution.
    fn dump(repo: &Path) -> Vec<(String, Vec<String>)> {
        let conn = rusqlite::Connection::open(repo.join(".nephila/graph.db")).unwrap();
        let queries = [
            ("nodes", "SELECT id, kind, qualified, short, file, subkind, summary, summary_file, qualified_rev FROM nodes ORDER BY id"),
            ("declarations", "SELECT file, node, origin, short, subkind, located, summary FROM declarations ORDER BY file, node"),
            ("refs", "SELECT source, file, kind, target_name, origin, target_key, target_head, unresolved FROM refs ORDER BY file, source, kind, target_name, origin"),
            ("file_contexts", "SELECT file, context FROM file_contexts ORDER BY file"),
            ("edges", "SELECT source, target, kind FROM edges ORDER BY source, target, kind"),
        ];
        queries
            .iter()
            .map(|(name, sql)| {
                let mut st = conn.prepare(sql).unwrap();
                let cols = st.column_count();
                let rows = st
                    .query_map([], |r| {
                        Ok((0..cols)
                            .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                            .collect::<Vec<_>>()
                            .join("|"))
                    })
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();
                (name.to_string(), rows)
            })
            .collect()
    }

    fn resolve(store: &mut Store, repo: &Path) {
        let mut report = BuildReport::default();
        crate::index::build::finalize(
            store,
            None,
            &crate::index::build::IndexOptions::default(),
            repo,
            &HashMap::new(),
            &mut report,
        )
        .unwrap();
    }

    /// AC-074-2 : la préparation et le calcul des clés se font dans les fils d'extraction, et les
    /// tables écrites sont celles d'une écriture des mêmes structures par `replace_structures`
    /// (base fraîche, puis chemin général).
    #[test]
    fn req074_ac2_preparation_dans_les_fils_tables_identiques() {
        use crate::store::{preparations_on_thread, preparations_total};
        let _d = crate::diag::scope();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write_repo(a.path(), 50);
        write_repo(b.path(), 50);
        let mut sa = Store::open_or_create(a.path()).unwrap();
        let mut sb = Store::open_or_create(b.path()).unwrap();
        let matcher = crate::filter::Matcher::none();
        for round in 0..2 {
            // A : analyze avec 4 fils.
            let scanned = crate::scan::scan(a.path(), &[], &matcher, 1).unwrap();
            assert_eq!(scanned.files.len(), 50);
            if round == 0 {
                sa.reset_with_pending_files(&pending_records(&scanned.files))
                    .unwrap();
            }
            let (t0, k0, g0) = (
                preparations_on_thread(),
                crate::resolve::name_keys_calls(),
                preparations_total(),
            );
            let mut report = BuildReport::default();
            analyze(
                a.path(),
                scanned.files,
                4,
                &scanned.gradle_settings,
                &mut sa,
                &mut report,
            )
            .unwrap();
            assert_eq!(
                preparations_on_thread(),
                t0,
                "préparation sur le fil appelant"
            );
            assert_eq!(
                crate::resolve::name_keys_calls(),
                k0,
                "name_keys sur le fil appelant"
            );
            assert_eq!(preparations_total() - g0, 50);
            assert_eq!(report.processed, 50);

            // B : mêmes structures, écrites par l'API brute avec la conversion de la 0.22.0.
            let scanned = crate::scan::scan(b.path(), &[], &matcher, 1).unwrap();
            if round == 0 {
                sb.reset_with_pending_files(&pending_records(&scanned.files))
                    .unwrap();
            }
            let ctx = ParseCtx {
                repo_root: Some(b.path().canonicalize().unwrap()),
                gradle_settings: scanned.gradle_settings.clone(),
            };
            let items: Vec<(String, Vec<crate::model::Node>, Vec<crate::model::Ref>)> = scanned
                .files
                .iter()
                .map(|f| {
                    let text = std::fs::read_to_string(b.path().join(&f.path)).unwrap();
                    let x = parse::extract(Path::new(&f.path), &text, &ctx).unwrap();
                    let (n, r, _) = crate::store::prepare::oracle::to_records(&f.path, &x);
                    (f.path.clone(), n, r)
                })
                .collect();
            sb.replace_structures(&items).unwrap();

            resolve(&mut sa, a.path());
            resolve(&mut sb, b.path());
            let (da, db) = (dump(a.path()), dump(b.path()));
            for ((name, ra), (_, rb)) in da.iter().zip(&db) {
                assert_eq!(ra, rb, "table {name}, tour {round}");
            }
            assert!(da[2].1.len() > 100, "refs : {}", da[2].1.len());
            assert!(!da[3].1.is_empty());
            assert!(!da[4].1.is_empty());
        }
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
        let n = prepare_structure("api/pom.xml", &module("com.acme:api", Some("api")))
            .unwrap()
            .nodes;
        assert_eq!(n[0].short, "api");
    }

    #[test]
    fn dec024_nom_court_derive_du_module() {
        let n = prepare_structure("pom.xml", &module("com.acme:core", None))
            .unwrap()
            .nodes;
        assert_eq!(n[0].short, "core");
        let n = prepare_structure("pom.xml", &module("core", None))
            .unwrap()
            .nodes;
        assert_eq!(n[0].short, "core");
        let n = prepare_structure("pom.xml", &module("a/b", None))
            .unwrap()
            .nodes;
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
        assert!(finish(&report).is_err());

        // Après correction, un update analyse le fichier.
        drop(store);
        std::fs::write(repo.join("B.java"), "package p;\npublic class B {}\n").unwrap();
        let lock = RepoLock::acquire(repo).unwrap();
        let report = update(repo, &opts, None, &lock, &crate::filter::Matcher::none())
            .unwrap()
            .unwrap();
        assert_eq!(report.pending, 0);
        assert!(finish(&report).is_ok());
        let store = Store::open_readonly(repo).unwrap();
        assert!(store.nodes().unwrap().iter().any(|n| n.id == "type:p.B"));
    }

    fn write_mixed_repo(repo: &Path) {
        for i in 0..30 {
            std::fs::write(
                repo.join(format!("J{i}.java")),
                format!("package p;\nimport q.D{i};\npublic class J{i} extends D{i} {{ void m() {{ J{}.go(); }} }}\n", (i + 1) % 30),
            )
            .unwrap();
            std::fs::write(
                repo.join(format!("t{i}.ts")),
                format!(
                    "import {{ x }} from './t{}';\nexport class T{i} {{ run() {{ x(); }} }}\n",
                    (i + 1) % 30
                ),
            )
            .unwrap();
        }
        for i in 0..8 {
            std::fs::write(
                repo.join(format!("K{i}.kt")),
                format!("package k\nclass K{i} {{ fun f() = K{}() }}\n", (i + 1) % 8),
            )
            .unwrap();
        }
        for d in ["a", "b"] {
            std::fs::create_dir(repo.join(d)).unwrap();
            std::fs::write(
                repo.join(d).join("build.gradle.kts"),
                "plugins { kotlin(\"jvm\") }\ndependencies { implementation(\"x:y:1\") }\n",
            )
            .unwrap();
        }
    }

    /// Analyse le dépôt mixte avec `jobs` fils ; rend (parsers créés, tables).
    fn analyze_mixed(jobs: usize, fresh: bool) -> (usize, Vec<(String, Vec<String>)>) {
        let d = crate::diag::scope();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        write_mixed_repo(repo);
        let scanned = crate::scan::scan(repo, &[], &crate::filter::Matcher::none(), 1).unwrap();
        assert_eq!(scanned.files.len(), 70);
        let mut store = Store::open_or_create(repo).unwrap();
        store
            .reset_with_pending_files(&pending_records(&scanned.files))
            .unwrap();
        d.set_force_fresh(fresh);
        let mut report = BuildReport::default();
        let r = analyze(
            repo,
            scanned.files,
            jobs,
            &scanned.gradle_settings,
            &mut store,
            &mut report,
        );
        d.set_force_fresh(false);
        r.unwrap();
        let created = d.get(crate::diag::Counter::ParsersCreated);
        resolve(&mut store, repo);
        drop(store);
        (created, dump(repo))
    }

    /// AC-078-1 : au plus un parser par fil et par grammaire ; graphe identique à celui d'un
    /// parser neuf par fichier.
    #[test]
    fn req078_ac1_un_parser_par_fil_et_par_grammaire() {
        let (created, tables) = analyze_mixed(3, false);
        assert!((3..=9).contains(&created), "{created} parsers créés");
        let (created1, tables1) = analyze_mixed(1, false);
        assert_eq!(created1, 3);
        let (fresh_created, oracle) = analyze_mixed(3, true);
        assert_eq!(fresh_created, 0, "le crochet contourne le cache");
        assert!(oracle
            .iter()
            .any(|(n, rows)| n == "nodes" && rows.len() > 60));
        assert_eq!(tables, oracle);
        assert_eq!(tables1, oracle);
    }

    type Tables = Vec<(String, Vec<String>)>;

    /// Dépôt de 6 fichiers analysé avec `jobs` et le lanceur donné : (résultat, traces, tables).
    fn run_with_launcher<L: crate::workers::Launcher>(
        jobs: usize,
        launcher: &L,
    ) -> (Result<()>, Vec<String>, Tables) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        write_repo(repo, 6);
        let scanned = crate::scan::scan(repo, &[], &crate::filter::Matcher::none(), 1).unwrap();
        assert_eq!(scanned.files.len(), 6);
        let mut store = Store::open_or_create(repo).unwrap();
        store
            .reset_with_pending_files(&pending_records(&scanned.files))
            .unwrap();
        let mut report = BuildReport::default();
        let (r, events) = crate::trace::capture(|| {
            analyze_with(
                repo,
                scanned.files,
                jobs,
                &scanned.gradle_settings,
                &mut store,
                &mut report,
                launcher,
            )
        });
        resolve(&mut store, repo);
        drop(store);
        (r, events, dump(repo))
    }

    fn started_traces(events: &[String]) -> Vec<&String> {
        events
            .iter()
            .filter(|e| e.contains("fils démarrés"))
            .collect()
    }

    /// AC-112-1 : échec au 1er démarrage.
    #[test]
    fn req112_ac1_echec_au_premier_fil() {
        let l = crate::workers::tests::FailAt::new(1);
        let (r, events, tables) = run_with_launcher(4, &l);
        assert_eq!(
            r.unwrap_err().to_string(),
            "création des fils d'analyse impossible : refus simulé"
        );
        assert!(tables
            .iter()
            .all(|(n, rows)| n != "nodes" || rows.is_empty()));
        assert!(started_traces(&events).is_empty());
        // AC-112-4 : la fin est tracée.
        assert!(events.iter().any(|e| e.starts_with("structure: fin ")));
    }

    /// AC-112-2 et AC-112-4 : échec au 3e démarrage, analyse avec 2 fils.
    #[test]
    fn req112_ac2_echec_au_troisieme_fil() {
        let (_, _, oracle) = run_with_launcher(2, &crate::workers::Spawner);
        assert!(oracle
            .iter()
            .any(|(n, rows)| n == "nodes" && !rows.is_empty()));
        let d = crate::diag::scope();
        let l = crate::workers::tests::FailAt::new(3);
        let (r, events, tables) = run_with_launcher(4, &l);
        r.unwrap();
        assert_eq!(d.get(crate::diag::Counter::ExtractThreads), 2);
        assert_eq!(tables, oracle);
        let begin = events
            .iter()
            .position(|e| e == "structure: début 6 fichiers, 4 fils")
            .unwrap();
        assert_eq!(
            events[begin + 1],
            "structure: 2 fils démarrés sur 4 demandés"
        );
        assert_eq!(started_traces(&events).len(), 1);
        let end = events
            .iter()
            .position(|e| e.starts_with("structure: fin "))
            .unwrap();
        assert!(end > begin + 1);
    }

    /// AC-112-4 : échec au 2e démarrage, pluriel figé.
    #[test]
    fn req112_ac4_un_seul_fil_demarre() {
        let l = crate::workers::tests::FailAt::new(2);
        let (r, events, _) = run_with_launcher(4, &l);
        r.unwrap();
        let begin = events
            .iter()
            .position(|e| e == "structure: début 6 fichiers, 4 fils")
            .unwrap();
        assert_eq!(
            events[begin + 1],
            "structure: 1 fils démarrés sur 4 demandés"
        );
        assert_eq!(started_traces(&events).len(), 1);
    }

    /// AC-112-3 : aucun échec, aucune trace.
    #[test]
    fn req112_ac3_aucun_echec_aucune_trace() {
        let l = crate::workers::tests::FailAt::new(0);
        let (r, events, _) = run_with_launcher(4, &l);
        r.unwrap();
        assert!(started_traces(&events).is_empty());
        assert_eq!(l.calls.load(Ordering::SeqCst), 4);
    }
}
