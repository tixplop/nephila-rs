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
    enrich_code: bool,
    verbose: bool,
    over: build::Overrides,
    cli_filters: Filters,
) -> Result<()> {
    trace::init(verbose);
    build::check_repo(repo)?;
    let project = ProjectFile::load(repo)?;
    let matcher = build::effective_matcher(repo, &project.filters, &cli_filters)?;
    let mut setup = build::setup(
        no_llm,
        enrich_code,
        &project.settings,
        over.batch_files,
        over.format_retries,
    )?;
    if let Some(j) = over.jobs {
        setup.opts.jobs = j;
    }
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
        store.set_enrich_code(opts.enrich_code)?;
    }
    // « Aucun changement » : rien d'ajouté, modifié, supprimé ou en attente, et (LLM inactif ou
    // aucun fichier à enrichir).
    if d.is_up_to_date() && (llm.is_none() || store.count_files_to_enrich()? == 0) {
        return Ok(None);
    }

    // Tous les fichiers supprimés partent en une seule transaction (DEC-074).
    let deleted: Vec<&str> = d.paths(Change::Deleted).collect();
    store.remove_files(&deleted)?;
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
        opts.jobs,
        &d.gradle_settings,
        &mut store,
        &mut report,
    )?;
    let abs = build::abs_paths(&d.scanned);
    build::finalize(&mut store, llm, opts, repo, &abs, &mut report)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Matcher;
    use crate::index::lock::RepoLock;
    use crate::llm::mock::{MockLlm, MockOptions};
    use crate::store::{diagnostics, reset_diagnostics};
    use std::path::PathBuf;

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

    fn class_name(i: usize) -> String {
        format!("C{i:02}")
    }

    /// Dépôt de `n` classes Java, construit `--no-llm`.
    fn built_repo(dir: &Path, n: usize) -> PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for i in 0..n {
            let c = class_name(i);
            std::fs::write(
                repo.join(format!("{c}.java")),
                format!("package p;\nimport java.util.List;\npublic class {c} {{}}\n"),
            )
            .unwrap();
        }
        let lock = RepoLock::acquire(&repo).unwrap();
        build::build(
            &repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap();
        repo
    }

    fn update_commits(repo: &Path, deleted: &[usize]) -> u64 {
        for &i in deleted {
            std::fs::remove_file(repo.join(format!("{}.java", class_name(i)))).unwrap();
        }
        let lock = RepoLock::acquire(repo).unwrap();
        reset_diagnostics();
        update(
            repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap()
        .unwrap();
        diagnostics().commits()
    }

    #[test]
    fn req060_ac1_deleted_files_are_removed_in_one_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let repo = built_repo(dir.path(), 40);
        let copy = dir.path().join("copy");
        copy_dir(&repo, &copy);

        let c1 = update_commits(&repo, &[7]);
        let gone: Vec<usize> = (10..30).collect();
        let c20 = update_commits(&copy, &gone);
        assert_eq!(c1, c20, "une validation par fichier supprimé");

        let store = Store::open_readonly(&copy).unwrap();
        let files: Vec<String> = store.files().unwrap().into_iter().map(|f| f.path).collect();
        let nodes: Vec<String> = store.nodes().unwrap().into_iter().map(|n| n.id).collect();
        let decls = store.declarations().unwrap();
        let contexts = store.file_contexts().unwrap();
        let refs = store.file_refs().unwrap();
        for i in 0..40 {
            let c = class_name(i);
            let path = format!("{c}.java");
            let kept = !gone.contains(&i);
            assert_eq!(files.contains(&path), kept, "{path}");
            assert_eq!(nodes.contains(&format!("type:p.{c}")), kept, "{path}");
            assert_eq!(decls.iter().any(|(f, _)| *f == path), kept, "{path}");
            assert_eq!(contexts.contains_key(&path), kept, "{path}");
            assert_eq!(refs.iter().any(|r| r.file == path), kept, "{path}");
            assert!(store.enrichment_entry(&path).unwrap().is_none());
        }
        assert_eq!(files.len(), 20);
    }

    /// AC-060-2 : `batch_files = 10`, 20 documents, deux appels : au plus une validation par appel.
    #[test]
    fn req060_ac2_enrichments_are_committed_per_received_message() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let mockdir = dir.path().join("mock");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&mockdir).unwrap();
        for i in 0..20 {
            std::fs::write(repo.join(format!("d{i:02}.md")), format!("# Titre {i}\n")).unwrap();
            std::fs::write(
                mockdir.join(format!("d{i:02}.md.json")),
                format!(
                    "{{\"version\":2,\"summaries\":[{{\"node\":\"document:d{i:02}.md\",\"summary\":\"résumé {i}\"}}],\"concepts\":[{{\"name\":\"Idée{i}\",\"summary\":\"concept {i}\",\"sections\":[\"section:d{i:02}.md#titre-{i}\"]}}]}}"
                ),
            )
            .unwrap();
        }
        let lock = RepoLock::acquire(&repo).unwrap();
        let opts = IndexOptions {
            concurrency: 1,
            batch_files: 10,
            ..IndexOptions::default()
        };
        build::build(&repo, &opts, None, &lock, &Matcher::none()).unwrap();

        let log = dir.path().join("mock.log");
        let llm = MockLlm::new(MockOptions {
            dir: Some(mockdir),
            log: Some(log.clone()),
            ..MockOptions::default()
        });
        // Build sans LLM : les 20 documents sont indexés mais pas enrichis.
        update_with_llm(&repo, &opts, &llm);
        let calls = std::fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(calls, 2);
        let store = Store::open_readonly(&repo).unwrap();
        assert_eq!(store.files_to_enrich().unwrap().len(), 0);
        for i in 0..20 {
            let path = format!("d{i:02}.md");
            assert!(store.enrichment_entry(&path).unwrap().is_some(), "{path}");
            let node = store
                .node_by_id(&format!("document:{path}"))
                .unwrap()
                .unwrap();
            assert_eq!(node.summary.as_deref(), Some(&*format!("résumé {i}")));
        }
        assert!(store
            .nodes()
            .unwrap()
            .iter()
            .any(|n| n.kind == crate::model::NodeKind::Concept));
    }

    /// Enrichit les fichiers à enrichir et compte les validations de cette phase seule.
    fn update_with_llm(repo: &Path, opts: &IndexOptions, llm: &MockLlm) {
        let mut store = Store::open_existing(repo).unwrap();
        store.set_enrich_model(crate::llm::Llm::model(llm)).unwrap();
        let files = store.files_to_enrich().unwrap();
        assert_eq!(files.len(), 20);
        let scanned = crate::scan::scan(repo, &store.files().unwrap(), &Matcher::none()).unwrap();
        let abs = build::abs_paths(&scanned.files);
        let mut report = BuildReport::default();
        reset_diagnostics();
        crate::index::enrich::enrich_all(repo, &abs, &files, opts, llm, &mut store, &mut report)
            .unwrap();
        let commits = diagnostics().commits();
        assert!(commits <= 2, "{commits} validations pour 2 appels");
        assert!(commits >= 1);
    }

    /// Dépôt `maven3` (trois modules, des documents) construit `--no-llm`.
    fn maven_repo(dir: &Path) -> PathBuf {
        let repo = dir.join("repo");
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven3/repo"),
            &repo,
        );
        let lock = RepoLock::acquire(&repo).unwrap();
        build::build(
            &repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap();
        repo
    }

    /// `update` du dépôt ; passes (complètes, incrémentales) de la commande.
    fn passes_of_update(repo: &Path) -> (u64, u64) {
        let lock = RepoLock::acquire(repo).unwrap();
        reset_diagnostics();
        update(
            repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap()
        .expect("des fichiers ont changé");
        let d = diagnostics();
        (d.full_passes(), d.incremental_passes())
    }

    /// Le graphe de `repo` est celui d'un `build` neuf du même arbre.
    fn assert_same_as_fresh_build(repo: &Path, dir: &Path, what: &str) {
        let fresh = dir.join("fresh");
        let _ = std::fs::remove_dir_all(&fresh);
        copy_dir(repo, &fresh);
        std::fs::remove_dir_all(fresh.join(".nephila")).unwrap();
        let lock = RepoLock::acquire(&fresh).unwrap();
        build::build(
            &fresh,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap();
        let (a, b) = (
            Store::open_readonly(repo).unwrap(),
            Store::open_readonly(&fresh).unwrap(),
        );
        assert_eq!(a.nodes().unwrap(), b.nodes().unwrap(), "{what} : nœuds");
        assert_eq!(a.edges().unwrap(), b.edges().unwrap(), "{what} : liens");
        assert_eq!(a.count_unresolved().unwrap(), b.count_unresolved().unwrap());
        assert_eq!(
            a.summary().unwrap().unwrap().0,
            b.summary().unwrap().unwrap().0,
            "{what} : résumé"
        );
    }

    fn touch_java(repo: &Path, extra: &str) {
        let f = repo.join("api/src/main/java/com/acme/api/ApiController.java");
        let mut text = std::fs::read_to_string(&f).unwrap();
        let i = text.rfind('}').unwrap();
        text.insert_str(i, extra);
        std::fs::write(f, text).unwrap();
    }

    /// AC-066-3 (a) : graphe non marqué `à jour` -> passe complète.
    #[test]
    fn req066_ac3_a_unmarked_graph_gets_a_full_pass() {
        let dir = tempfile::tempdir().unwrap();
        let repo = maven_repo(dir.path());
        rusqlite::Connection::open(db_path(&repo))
            .unwrap()
            .execute(
                "UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'",
                [],
            )
            .unwrap();
        touch_java(&repo, "\n    void ajoutee() {}\n");
        assert_eq!(passes_of_update(&repo), (1, 0));
        assert_same_as_fresh_build(&repo, dir.path(), "marqueur absent");
        // La commande suivante, graphe marqué, est incrémentale.
        touch_java(&repo, "\n    void autre() {}\n");
        assert_eq!(passes_of_update(&repo), (0, 1));
        assert_same_as_fresh_build(&repo, dir.path(), "marqueur posé");
    }

    /// AC-066-3 (b) : un module nouveau -> passe complète.
    #[test]
    fn req066_ac3_b_new_module_gets_a_full_pass() {
        let dir = tempfile::tempdir().unwrap();
        let repo = maven_repo(dir.path());
        std::fs::create_dir_all(repo.join("extra")).unwrap();
        std::fs::write(
            repo.join("extra/pom.xml"),
            "<project><groupId>com.acme</groupId><artifactId>extra</artifactId>\
             <dependencies><dependency><groupId>com.acme</groupId><artifactId>core</artifactId>\
             </dependency></dependencies></project>",
        )
        .unwrap();
        assert_eq!(passes_of_update(&repo), (1, 0));
        let store = Store::open_readonly(&repo).unwrap();
        assert!(store
            .nodes()
            .unwrap()
            .iter()
            .any(|n| n.id == "module:com.acme:extra"));
        assert_same_as_fresh_build(&repo, dir.path(), "module ajouté");
    }

    /// AC-066-3 (c) : un `pom.xml` existant dont les dépendances externes changent -> passe
    /// incrémentale (constats d'AC-008-6).
    #[test]
    fn req066_ac3_c_modified_pom_gets_an_incremental_pass() {
        let dir = tempfile::tempdir().unwrap();
        let repo = maven_repo(dir.path());
        let dep = "dependency:org.slf4j:slf4j-api";
        {
            let store = Store::open_readonly(&repo).unwrap();
            assert!(store.nodes().unwrap().iter().any(|n| n.id == dep));
        }
        let pom = repo.join("core/pom.xml");
        let text = std::fs::read_to_string(&pom)
            .unwrap()
            .replace("slf4j-api", "newlib")
            .replace("org.slf4j", "org.new");
        std::fs::write(&pom, text).unwrap();
        assert_eq!(passes_of_update(&repo), (0, 1));
        let store = Store::open_readonly(&repo).unwrap();
        let nodes = store.nodes().unwrap();
        assert!(!nodes.iter().any(|n| n.id == dep));
        assert!(nodes.iter().any(|n| n.id == "dependency:org.new:newlib"));
        assert!(store.edges().unwrap().iter().any(|e| {
            e.source == "module:com.acme:core"
                && e.target == "dependency:org.new:newlib"
                && e.kind == crate::model::EdgeKind::DependsOn
        }));
        assert_same_as_fresh_build(&repo, dir.path(), "pom modifié");
    }

    /// Dépôt `maven2` et fixtures du mock : (dépôt, dossier du mock).
    fn maven2_with_mock(dir: &Path) -> (PathBuf, PathBuf) {
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let repo = dir.join("repo");
        let mock = dir.join("mock");
        copy_dir(&fx.join("maven2"), &repo);
        copy_dir(&fx.join("mock"), &mock);
        (repo, mock)
    }

    fn mock_llm(mock: &Path) -> MockLlm {
        MockLlm::new(MockOptions {
            dir: Some(mock.to_path_buf()),
            ..MockOptions::default()
        })
    }

    fn db(repo: &Path) -> rusqlite::Connection {
        rusqlite::Connection::open(db_path(repo)).unwrap()
    }

    /// Identifiants des références comptées non résolues.
    fn unresolved_ids(repo: &Path) -> Vec<i64> {
        let c = db(repo);
        let mut st = c
            .prepare("SELECT id FROM refs WHERE unresolved = 1 ORDER BY id")
            .unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    /// Le graphe de `repo` est celui d'une passe complète sur une copie de ce même graphe.
    fn assert_same_as_full_pass(repo: &Path, dir: &Path, what: &str) {
        let copy = dir.join("full");
        let _ = std::fs::remove_dir_all(&copy);
        copy_dir(repo, &copy);
        db(&copy)
            .execute(
                "UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'",
                [],
            )
            .unwrap();
        reset_diagnostics();
        {
            let mut store = Store::open_existing(&copy).unwrap();
            crate::resolve::resolve(&mut store).unwrap();
        }
        assert_eq!(diagnostics().full_passes(), 1, "{what} : passe de contrôle");
        let (a, b) = (
            Store::open_readonly(repo).unwrap(),
            Store::open_readonly(&copy).unwrap(),
        );
        assert_eq!(a.nodes().unwrap(), b.nodes().unwrap(), "{what} : nœuds");
        assert_eq!(a.edges().unwrap(), b.edges().unwrap(), "{what} : liens");
        assert_eq!(
            unresolved_ids(repo),
            unresolved_ids(&copy),
            "{what} : non résolues"
        );
        assert_eq!(
            a.summary().unwrap().unwrap().0,
            b.summary().unwrap().unwrap().0,
            "{what} : résumé"
        );
    }

    /// AC-068-1 : `build` avec le mock : une passe complète, une incrémentale, bornée.
    #[test]
    fn req068_ac1_build_with_llm_runs_one_full_and_one_incremental_pass() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, mock) = maven2_with_mock(dir.path());
        let llm = mock_llm(&mock);
        let lock = RepoLock::acquire(&repo).unwrap();
        reset_diagnostics();
        build::build(
            &repo,
            &IndexOptions::default(),
            Some(&llm),
            &lock,
            &Matcher::none(),
        )
        .unwrap();
        let d = diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (1, 1));

        let c = db(&repo);
        let count =
            |sql: &str| -> usize { c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap() as usize };
        let parse_refs = count("SELECT count(*) FROM refs WHERE origin = 'parse'");
        let llm_refs = count("SELECT count(*) FROM refs WHERE origin = 'llm'");
        assert!(llm_refs > 0, "le mock doit écrire des références");
        let total = count("SELECT count(*) FROM refs");
        // Clés des nœuds `concept` et `dependency` du graphe final (DEC-082).
        let store = crate::store::Store::open_readonly(&repo).unwrap();
        let keys: Vec<String> = store
            .nodes()
            .unwrap()
            .iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    crate::model::NodeKind::Concept | crate::model::NodeKind::Dependency
                )
            })
            .map(|n| crate::resolve::name_keys(&n.qualified).0)
            .collect();
        drop(store);
        let marks = vec!["?"; keys.len()].join(",");
        let sql = format!(
            "SELECT count(*) FROM refs WHERE source IN (SELECT source FROM refs WHERE file IN \
             (SELECT file FROM refs WHERE origin = 'llm') OR target_key IN ({marks}) \
             OR target_head IN ({marks}))"
        );
        let params: Vec<&String> = keys.iter().chain(keys.iter()).collect();
        let expected = c
            .query_row(&sql, rusqlite::params_from_iter(params), |r| {
                r.get::<_, i64>(0)
            })
            .unwrap() as usize;
        // La première passe relit exactement les références de la phase structure.
        let second = d.refs_reevaluated() as usize - parse_refs;
        assert_eq!(
            second, expected,
            "références réévaluées par la seconde passe"
        );
        assert!(second < total, "{second} >= {total}");
        drop(c);
        assert_same_as_full_pass(&repo, dir.path(), "build avec le mock");
    }

    /// AC-068-2 : `update` après modification d'un document : deux passes incrémentales.
    #[test]
    fn req068_ac2_update_with_llm_runs_two_incremental_passes() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, mock) = maven2_with_mock(dir.path());
        let llm = mock_llm(&mock);
        let lock = RepoLock::acquire(&repo).unwrap();
        build::build(
            &repo,
            &IndexOptions::default(),
            Some(&llm),
            &lock,
            &Matcher::none(),
        )
        .unwrap();
        let guide = repo.join("docs/guide.md");
        let mut text = std::fs::read_to_string(&guide).unwrap();
        text.push_str("\nUn paragraphe de plus.\n");
        std::fs::write(&guide, text).unwrap();
        reset_diagnostics();
        update(
            &repo,
            &IndexOptions::default(),
            Some(&llm),
            &lock,
            &Matcher::none(),
        )
        .unwrap()
        .expect("le document a changé");
        let d = diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (0, 2));
        assert_same_as_full_pass(&repo, dir.path(), "update avec le mock");
    }
}
