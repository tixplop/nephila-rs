//! Enrichissements conservés entre deux builds (REQ-049, DEC-062) : AC-049-1, 2, 5, 8 et 9.

use assert_cmd::Command;
use nephila::model::{EdgeKind, NodeKind};
use nephila::store::{Store, SCHEMA_VERSION};
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;

const SVC: &str = "core/src/main/java/com/acme/core/OrderService.java";
const ORDER: &str = "core/src/main/java/com/acme/domain/Order.java";

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    /// Dépôt exemple, configuration `mock` avec `batch_files = 1` (une ligne de journal par fichier).
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("maven2"), &repo);
        copy_dir(&fx.join("mock"), &mock);
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\n",
        )
        .unwrap();
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            mock,
            log,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .arg("-C")
            .arg(&self.repo);
        c
    }

    /// Exécute la commande avec un journal du mock vide au départ.
    fn run(&self, args: &[&str]) -> Output {
        let _ = fs::remove_file(&self.log);
        self.cmd().args(args).output().unwrap()
    }

    /// Comme `run`, sans fichier de configuration LLM.
    fn run_plain(&self, args: &[&str]) -> Output {
        let _ = fs::remove_file(&self.log);
        self.cmd()
            .env("NEPHILA_CONFIG", self.config.with_file_name("absent.toml"))
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(o.status.success(), "{}", text(&o.stderr));
        o
    }

    fn logged(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn logged_paths(&self) -> BTreeSet<String> {
        self.logged()
            .iter()
            .map(|l| l.split('\t').next().unwrap().to_string())
            .collect()
    }

    fn store(&self) -> Store {
        Store::open_readonly(&self.repo).unwrap()
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Nœuds (tous attributs, résumés compris), liens et références du graphe.
type Snapshot = (
    Vec<nephila::model::Node>,
    Vec<nephila::model::Edge>,
    Vec<String>,
);

fn snapshot(store: &Store) -> Snapshot {
    let mut nodes = store.nodes().unwrap();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let edges = store.edges().unwrap();
    let mut refs: Vec<String> = store
        .refs_with_origin()
        .unwrap()
        .iter()
        .map(|(r, o)| format!("{} {:?} {} {:?}", r.source, r.kind, r.target_name, o))
        .collect();
    refs.sort();
    (nodes, edges, refs)
}

fn summary_of(env: &Env, id: &str) -> Option<String> {
    env.store()
        .nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("nœud absent : {id}"))
        .summary
}

#[test]
fn req049_ac1_req235_ac16_second_build_without_change_sends_nothing_and_graph_is_identical() {
    let env = Env::new();
    let first = env.ok(&["build"]);
    let sent = env.logged();
    assert_eq!(sent.len(), env.store().files().unwrap().len(), "{sent:?}");
    let before = snapshot(&env.store());
    assert!(before.0.iter().any(|n| n.summary.is_some()));
    assert!(before.0.iter().any(|n| n.kind == NodeKind::Concept));
    assert!(before.1.iter().any(|e| e.kind == EdgeKind::Mentions));

    let second = env.ok(&["build"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    let out = text(&second.stdout);
    assert!(out.contains("0 fichiers à enrichir"), "{out}");
    // REQ-235 (AC-235-16) : le second build n'écrit aucun enrichissement, donc pas de segment.
    let first_out = text(&first.stdout);
    let (head, segment) = first_out.trim_end().rsplit_once(", ").unwrap();
    assert!(segment.ends_with(" nœuds résumés"), "{first_out}");
    assert!(!out.contains("nœuds résumés"), "{out}");
    assert_eq!(out.trim_end(), head);
    assert_eq!(snapshot(&env.store()), before);

    let status = env.ok(&["status"]);
    assert!(text(&status.stdout).contains("graphe à jour"));
    assert!(!text(&status.stdout).contains("à enrichir"));
    let json = env.ok(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["to_enrich"], 0);
}

#[test]
fn req049_ac2_only_modified_and_added_files_are_sent() {
    let env = Env::new();
    env.ok(&["build"]);
    let readme = summary_of(&env, "document:README.md");
    assert!(readme.is_some());
    assert!(env.store().enrichment_entry(ORDER).unwrap().is_some());

    let mut svc = fs::read_to_string(env.repo.join(SVC)).unwrap();
    svc.push_str("\n// modifié\n");
    env.write(SVC, &svc);
    env.write("docs/nouveau.md", "# Nouveau\n\nTexte.\n");
    fs::remove_file(env.repo.join(ORDER)).unwrap();

    env.ok(&["build"]);
    assert_eq!(
        env.logged_paths(),
        BTreeSet::from([SVC.to_string(), "docs/nouveau.md".to_string()])
    );
    assert_eq!(env.logged().len(), 2);
    assert_eq!(summary_of(&env, "document:README.md"), readme);
    let store = env.store();
    assert!(!store
        .nodes()
        .unwrap()
        .iter()
        .any(|n| n.id == "type:com.acme.domain.Order"));
    assert_eq!(store.enrichment_entry(ORDER).unwrap(), None);
    let svc_entry = store.enrichment_entry(SVC).unwrap().unwrap();
    let hash = store
        .files()
        .unwrap()
        .into_iter()
        .find(|f| f.path == SVC)
        .unwrap()
        .hash;
    assert_eq!(svc_entry.content_hash, hash);
    assert_eq!(store.count_files_to_enrich().unwrap(), 0);
}

#[test]
fn req049_ac5_enrichments_survive_no_llm_and_missing_config() {
    let env = Env::new();
    env.ok(&["build"]);
    let before = snapshot(&env.store());

    let o = env.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(env.logged().is_empty());
    assert_eq!(snapshot(&env.store()), before);

    let o = env.run_plain(&["build"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(snapshot(&env.store()), before);
    assert!(env.logged().is_empty());
    let search = env.ok(&["query", "search", "OrderService"]);
    assert!(text(&search.stdout).contains("OrderService"));
    assert!(
        text(&search.stdout).contains("Place les commandes."),
        "{}",
        text(&search.stdout)
    );
    assert!(env
        .store()
        .nodes()
        .unwrap()
        .iter()
        .any(|n| n.id == "concept:facturation"));

    let o = env.ok(&["update"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert_eq!(text(&o.stdout), "graphe à jour\n");
}

/// Fabrique un graphe de schéma 4 (format de la 0.17.6).
fn make_schema4(env: &Env) {
    let dir = env.repo.join(".nephila");
    fs::create_dir_all(&dir).unwrap();
    let c = rusqlite::Connection::open(dir.join("graph.db")).unwrap();
    c.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE files (path TEXT PRIMARY KEY, hash TEXT NOT NULL, size INTEGER NOT NULL,
            mtime INTEGER NOT NULL, recorded_at INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL,
            enriched_hash TEXT);
         INSERT INTO meta(key, value) VALUES ('schema_version', '4');
         PRAGMA user_version = 4;",
    )
    .unwrap();
}

#[test]
fn req049_ac8_schema4_graph_is_replaced_by_build_and_refused_elsewhere() {
    let env = Env::new();
    make_schema4(&env);
    for args in [
        vec!["status"],
        vec!["query", "search", "x"],
        vec!["export"],
        vec!["summary"],
        vec!["update"],
    ] {
        let o = env.run(&args);
        assert!(!o.status.success(), "{args:?}");
        assert!(text(&o.stderr).contains("nephila build"), "{args:?}");
    }
    let n_files = {
        env.ok(&["build"]);
        env.store().files().unwrap().len()
    };
    assert_eq!(env.store().schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 14);
    assert_eq!(env.logged().len(), n_files, "{:?}", env.logged());
}

#[test]
fn req049_ac9_unreadable_then_readable_file_is_reapplied_without_call() {
    let env = Env::new();
    // Mtime ancien : l'empreinte connue est réutilisée sans relire le fichier.
    let path = env.repo.join(SVC);
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    env.ok(&["build"]);
    let summary = summary_of(&env, "type:com.acme.core.OrderService");
    assert!(summary.is_some());

    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        return; // exécuté en administrateur : les droits ne protègent rien
    }
    let o = env.run(&["build"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!o.status.success(), "fichier en attente");
    assert!(!env
        .store()
        .nodes()
        .unwrap()
        .iter()
        .any(|n| n.id == "type:com.acme.core.OrderService"));
    assert!(env.store().enrichment_entry(SVC).unwrap().is_some());

    let o = env.ok(&["update"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert!(text(&o.stdout).contains("0 fichiers à enrichir"));
    assert_eq!(summary_of(&env, "type:com.acme.core.OrderService"), summary);
}

fn all_paths(env: &Env) -> BTreeSet<String> {
    env.store()
        .files()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect()
}

fn set_personal_model(env: &Env, model: &str) {
    fs::write(
        &env.config,
        format!("provider = \"mock\"\nenrich_code = true\nmodel = \"{model}\"\nbatch_files = 1\nconcurrency = 1\n"),
    )
    .unwrap();
}

/// Change le modèle (`personal` : configuration personnelle, sinon `.nephila.toml`).
fn change_model(env: &Env, personal: bool) {
    if personal {
        set_personal_model(env, "autre");
    } else {
        env.write(".nephila.toml", "model = \"autre\"\n");
    }
}

#[test]
fn req049_ac3_model_change_resends_each_file_once_by_build_and_by_update() {
    for personal in [true, false] {
        for verb in ["build", "update"] {
            let env = Env::new();
            env.ok(&["build"]);
            let files = all_paths(&env);
            change_model(&env, personal);

            if verb == "update" {
                let st = env.ok(&["status"]);
                assert!(
                    text(&st.stdout).contains("graphe à jour"),
                    "{}",
                    text(&st.stdout)
                );
            }
            env.ok(&[verb]);
            let sent = env.logged();
            assert_eq!(sent.len(), files.len(), "{verb} {personal} : {sent:?}");
            assert_eq!(env.logged_paths(), files, "{verb} {personal}");
            assert_eq!(env.store().count_files_to_enrich().unwrap(), 0);
            for f in &files {
                assert_eq!(
                    env.store().enrichment_entry(f).unwrap().unwrap().model,
                    "autre"
                );
            }

            let again = env.ok(&[verb]);
            assert!(
                env.logged().is_empty(),
                "{verb} {personal} : {:?}",
                env.logged()
            );
            if verb == "update" {
                assert_eq!(text(&again.stdout), "graphe à jour\n");
            }
            env.ok(&["build"]);
            assert!(env.logged().is_empty(), "{:?}", env.logged());
        }
    }
}

#[test]
fn req049_ac3_other_settings_send_nothing() {
    let env = Env::new();
    env.ok(&["build"]);
    env.write(
        ".nephila.toml",
        "max_chunk_chars = 4000\nbatch_files = 3\nconcurrency = 2\nformat_retries = 3\ntimeout_secs = 100\n",
    );
    env.ok(&["build"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    let o = env.ok(&["update"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert_eq!(text(&o.stdout), "graphe à jour\n");
}

#[test]
fn req049_ac3_model_survives_build_no_llm() {
    let env = Env::new();
    env.ok(&["build"]);
    env.ok(&["build", "--no-llm"]);
    let st = env.ok(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&st.stdout).unwrap();
    assert_eq!(v["to_enrich"], 0);
    set_personal_model(&env, "autre");
    env.ok(&["update"]);
    assert_eq!(env.logged_paths(), all_paths(&env));
}

fn set_personal_language(env: &Env, language: &str) {
    fs::write(
        &env.config,
        format!("provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\nsummary_language = \"{language}\"\n"),
    )
    .unwrap();
}

/// Langues des entrées appliquées (`enrichment_entry`) des fichiers indexés (REQ-258).
fn enrichment_languages(env: &Env) -> BTreeSet<String> {
    let store = nephila::store::Store::open_readonly(&env.repo).unwrap();
    store
        .files()
        .unwrap()
        .iter()
        .filter_map(|f| store.enrichment_entry(&f.path).unwrap())
        .map(|e| e.language)
        .collect()
}

#[test]
fn req173_ac2_language_change_resends_each_file_once_by_build_and_by_update() {
    for personal in [true, false] {
        for verb in ["build", "update"] {
            let env = Env::new();
            env.ok(&["build"]);
            let files = all_paths(&env);
            assert_eq!(env.logged().len(), files.len());
            if personal {
                set_personal_language(&env, "anglais");
            } else {
                env.write(".nephila.toml", "summary_language = \"anglais\"\n");
            }
            env.ok(&[verb]);
            let sent = env.logged();
            assert_eq!(sent.len(), files.len(), "{verb} {personal} : {sent:?}");
            assert_eq!(env.logged_paths(), files, "{verb} {personal}");
            assert_eq!(
                enrichment_languages(&env),
                BTreeSet::from(["anglais".to_string()])
            );
            env.ok(&[verb]);
            assert!(
                env.logged().is_empty(),
                "{verb} {personal} : {:?}",
                env.logged()
            );
            env.ok(&["build"]);
            assert!(env.logged().is_empty(), "{:?}", env.logged());
        }
    }
}

#[test]
fn req173_ac2_default_language_written_explicitly_sends_nothing() {
    let env = Env::new();
    env.ok(&["build"]);
    set_personal_language(&env, "français");
    env.ok(&["build"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    env.ok(&["update"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
}

#[test]
fn req173_ac3_language_change_without_llm_command_keeps_state_until_update() {
    let env = Env::new();
    env.ok(&["build"]);
    set_personal_language(&env, "anglais");
    env.ok(&["build"]);
    let files = all_paths(&env);
    assert_eq!(env.logged_paths(), files);

    set_personal_language(&env, "espagnol");
    assert_eq!(to_enrich(&env), 0);
    let before = summary_of(&env, "type:com.acme.core.OrderService");
    assert!(before.is_some());
    env.ok(&["build", "--no-llm"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert_eq!(summary_of(&env, "type:com.acme.core.OrderService"), before);
    assert_eq!(to_enrich(&env), 0);

    env.ok(&["update"]);
    assert_eq!(env.logged().len(), files.len(), "{:?}", env.logged());
    assert_eq!(env.logged_paths(), files);
    assert_eq!(
        enrichment_languages(&env),
        BTreeSet::from(["espagnol".to_string()])
    );
}

#[test]
fn req049_ac6_resume_sends_only_files_not_enriched_before_failure() {
    let env = Env::new();
    for i in 0..8 {
        env.write(
            &format!("docs/extra{i}.md"),
            &format!("# Extra {i}\n\nTexte.\n"),
        );
    }
    let o = env.run(&["build"]);
    assert!(o.status.success());
    let _ = o;
    // Graphe neuf : le premier build ci-dessous est celui qui échoue.
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    let o = env
        .cmd()
        .env("NEPHILA_MOCK_FAIL_AFTER", "2")
        .arg("build")
        .output()
        .unwrap();
    assert!(!o.status.success(), "{}", text(&o.stdout));
    let store = env.store();
    let files: Vec<String> = store.files().unwrap().into_iter().map(|f| f.path).collect();
    assert!(files.iter().filter(|f| f.ends_with(".md")).count() >= 8);
    let enriched: BTreeSet<String> = files
        .iter()
        .filter(|f| store.enrichment_entry(f).unwrap().is_some())
        .cloned()
        .collect();
    assert_eq!(enriched.len(), 2, "{enriched:?}");
    drop(store);

    env.ok(&["build"]);
    let expected: BTreeSet<String> = files
        .into_iter()
        .filter(|f| !enriched.contains(f))
        .collect();
    assert_eq!(env.logged_paths(), expected);
    assert_eq!(env.logged().len(), expected.len());
}

#[test]
fn req049_ac7_killed_build_keeps_enriched_files() {
    let env = Env::new();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
        .env_remove("NEPHILA_MOCK_FAIL_AFTER")
        .env("NEPHILA_MOCK_DELAY_MS", "300")
        .env("NEPHILA_CONFIG", &env.config)
        .env("NEPHILA_MOCK_DIR", &env.mock)
        .env("NEPHILA_MOCK_LOG", &env.log)
        .arg("-C")
        .arg(&env.repo)
        .arg("build")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed().as_secs() < 30,
            "aucun enrichissement enregistré"
        );
        if let Ok(store) = Store::open_readonly(&env.repo) {
            let files = store.files().unwrap_or_default();
            if files
                .iter()
                .any(|f| store.enrichment_entry(&f.path).ok().flatten().is_some())
            {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    let store = env.store();
    let files: Vec<String> = store.files().unwrap().into_iter().map(|f| f.path).collect();
    let enriched: BTreeSet<String> = files
        .iter()
        .filter(|f| store.enrichment_entry(f).unwrap().is_some())
        .cloned()
        .collect();
    drop(store);
    assert!(!enriched.is_empty() && enriched.len() < files.len());
    env.ok(&["build"]);
    let sent = env.logged_paths();
    assert!(sent.is_disjoint(&enriched), "{sent:?} / {enriched:?}");
    assert_eq!(sent.len() + enriched.len(), files.len());
}

fn trace_lines(o: &Output) -> Vec<String> {
    text(&o.stderr).lines().map(str::to_string).collect()
}

fn structure_end(o: &Output) -> String {
    trace_lines(o)
        .into_iter()
        .find(|l| l.contains("structure: fin"))
        .unwrap_or_else(|| panic!("pas de ligne structure: fin : {}", text(&o.stderr)))
}

fn processed(o: &Output) -> usize {
    let out = text(&o.stdout);
    let rest = out.split("terminé : ").nth(1).unwrap();
    rest.split(' ').next().unwrap().parse().unwrap()
}

#[test]
fn req049_ac10_retained_count_in_structure_end_line() {
    let env = Env::new();
    let first = env.ok(&["build", "--verbose"]);
    assert!(
        structure_end(&first).ends_with(", 0 en attente, 0 enrichissements conservés"),
        "{}",
        structure_end(&first)
    );
    let second = env.ok(&["build", "--verbose"]);
    let n = processed(&second);
    assert!(n > 0);
    assert!(
        structure_end(&second).ends_with(&format!(", 0 en attente, {n} enrichissements conservés")),
        "{}",
        structure_end(&second)
    );
    assert!(!text(&second.stderr).contains("llm enrich"));
    // REQ-235 (AC-235-16) : égal au premier, moins le segment de couverture.
    let first_out = text(&first.stdout);
    let (head, segment) = first_out.trim_end().rsplit_once(", ").unwrap();
    assert!(segment.ends_with(" nœuds résumés"), "{first_out}");
    assert_eq!(text(&second.stdout).trim_end(), head);

    let third = env.ok(&["build", "--no-llm", "--verbose"]);
    assert!(
        structure_end(&third).ends_with(&format!(", 0 en attente, {n} enrichissements conservés")),
        "{}",
        structure_end(&third)
    );

    let fourth = env.ok(&["build", "--show-progress"]);
    assert!(
        !trace_lines(&fourth)
            .iter()
            .any(|l| l.starts_with("progression: enrichissement")),
        "{}",
        text(&fourth.stderr)
    );
    assert!(trace_lines(&fourth)
        .iter()
        .any(|l| l.starts_with("progression: extraction")));
}

#[test]
fn req049_ac10_update_of_reapplied_pending_file_counts_one() {
    let env = Env::new();
    let path = env.repo.join(SVC);
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    env.ok(&["build"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        return;
    }
    let o = env.run(&["build"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!o.status.success());
    let o = env.ok(&["update", "--verbose"]);
    assert!(
        structure_end(&o).ends_with(", 0 en attente, 1 enrichissements conservés"),
        "{}",
        structure_end(&o)
    );
    assert!(env.logged().is_empty());
}

/// Remplace la charge utile de l'enrichissement conservé de `path` par `{` (illisible).
fn corrupt_payload(env: &Env, path: &str) {
    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let n = conn
        .execute(
            "UPDATE enrichments SET payload = '{' WHERE path = ?1",
            [path],
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn req131_ac4_unreadable_entry_is_counted_on_structure_end_line() {
    let env = Env::new();
    env.ok(&["build"]);
    let second = env.ok(&["build", "--verbose"]);
    let n = processed(&second);
    assert!(n > 1);
    assert!(!text(&second.stderr).contains("illisibles"));
    let baseline = text(&env.ok(&["build"]).stderr);

    corrupt_payload(&env, SVC);
    let o = env.ok(&["build", "--verbose"]);
    assert!(
        structure_end(&o).ends_with(&format!(
            ", 0 en attente, {} enrichissements conservés, 1 enrichissements illisibles écartés",
            n - 1
        )),
        "{}",
        structure_end(&o)
    );
    assert_eq!(env.logged_paths(), BTreeSet::from([SVC.to_string()]));

    corrupt_payload(&env, SVC);
    let quiet = env.ok(&["build"]);
    assert_eq!(text(&quiet.stderr), baseline);
    assert!(!text(&quiet.stderr).contains("illisibles"));
}

#[test]
fn req131_ac7_segment_text_and_silence_elsewhere() {
    let env = Env::new();
    env.ok(&["build"]);
    corrupt_payload(&env, SVC);
    let db = env.repo.join(".nephila/graph.db");
    let saved = fs::read(&db).unwrap();

    let o = env.ok(&["build", "--no-llm", "--verbose"]);
    assert!(
        structure_end(&o)
            .ends_with("enrichissements conservés, 1 enrichissements illisibles écartés"),
        "{}",
        structure_end(&o)
    );
    let others = trace_lines(&o)
        .into_iter()
        .filter(|l| l.contains("illisibles"))
        .count();
    assert_eq!(others, 1, "{}", text(&o.stderr));

    fs::write(&db, saved).unwrap();
    let p = env.ok(&["build", "--show-progress"]);
    let err = text(&p.stderr);
    assert!(!err.contains("illisibles"), "{err}");
    assert!(!err.contains("trace:"), "{err}");
}

#[test]
fn req131_ac8_payloads_without_v_are_reused_without_reenrichment() {
    let env = Env::new();
    env.ok(&["build"]);
    let second = env.ok(&["build", "--verbose"]);
    let n = processed(&second);
    assert!(n > 0);
    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let changed = conn
        .execute(
            "UPDATE enrichments SET payload = json_remove(payload, '$.v')",
            [],
        )
        .unwrap();
    assert!(changed > 0);
    let left: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM enrichments WHERE payload LIKE '%\"v\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    drop(conn);

    let o = env.ok(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert!(!err.contains("llm enrich"), "{err}");
    assert!(!err.contains("illisibles"), "{err}");
    assert!(
        structure_end(&o).ends_with(&format!(", 0 en attente, {n} enrichissements conservés")),
        "{}",
        structure_end(&o)
    );
    assert!(env.logged().is_empty());
}

/// Dépôt réduit aux fichiers donnés (chemin, contenu), sans fixture de mock préexistante.
fn tiny_env(files: &[(&str, &str)]) -> Env {
    let env = Env::new();
    fs::remove_dir_all(&env.repo).unwrap();
    fs::remove_dir_all(&env.mock).unwrap();
    fs::create_dir_all(&env.mock).unwrap();
    for (p, c) in files {
        env.write(p, c);
    }
    env
}

fn to_enrich(env: &Env) -> u64 {
    let json = env.ok(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    v["to_enrich"].as_u64().unwrap()
}

/// Graphe enrichi, ramené à la version `old` du prompt, puis réenrichi une seule fois.
fn reenriched_once(old: i64) {
    let env = tiny_env(&[("a.md", "# Un\n\ntexte\n"), ("b.md", "# Deux\n\ntexte\n")]);
    for (f, id) in [("a.md", "document:a.md"), ("b.md", "document:b.md")] {
        fs::write(
            env.mock.join(format!("{f}.json")),
            format!(r#"{{"version":2,"summaries":[{{"node":"{id}","summary":"Résumé."}}],"concepts":[]}}"#),
        )
        .unwrap();
    }
    env.ok(&["build"]);
    assert_eq!(to_enrich(&env), 0);

    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let n = conn
        .execute(
            &format!("UPDATE enrichments SET prompt_version = {old}"),
            [],
        )
        .unwrap();
    assert_eq!(n, 2);
    drop(conn);
    assert_eq!(to_enrich(&env), 2);

    env.ok(&["update"]);
    assert_eq!(env.logged().len(), 2, "{:?}", env.logged());
    assert_eq!(env.logged_paths().len(), 2);
    assert_eq!(to_enrich(&env), 0);

    let again = env.ok(&["update"]);
    assert_eq!(text(&again.stdout), "graphe à jour\n");
    assert!(env.logged().is_empty(), "{:?}", env.logged());

    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let versions: Vec<i64> = conn
        .prepare("SELECT DISTINCT prompt_version FROM (SELECT prompt_version, ROW_NUMBER() OVER (PARTITION BY path ORDER BY used_at DESC) AS rn FROM enrichments) WHERE rn = 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        versions,
        vec![i64::from(nephila::llm::enrich::MARKDOWN_PROMPT_VERSION)]
    );
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM enrichments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 4, "deux entrées par chemin");
}

#[test]
fn req167_ac4_prompt_version_1_graph_is_reenriched_once() {
    reenriched_once(1);
}

#[test]
fn req169_ac8_prompt_version_2_graph_is_reenriched_once() {
    reenriched_once(2);
}

#[test]
fn req171_ac6_prompt_version_3_graph_is_reenriched_once() {
    reenriched_once(3);
}

#[test]
fn req172_ac6_prompt_version_4_graph_is_reenriched_once() {
    reenriched_once(4);
}

#[test]
fn req175_ac4_prompt_version_5_graph_is_reenriched_once() {
    reenriched_once(5);
}

#[test]
fn req177_ac6_prompt_version_6_graph_is_reenriched_once() {
    reenriched_once(6);
}

#[test]
fn req236_ac8_prompt_version_8_graph_is_reenriched_once() {
    reenriched_once(8);
}

#[test]
fn req267_ac5_previous_prompt_versions_are_reenriched_once() {
    use nephila::llm::enrich::{
        BUILD_PROMPT_VERSION, CODE_PROMPT_VERSION, DATA_PROMPT_VERSION, MARKDOWN_PROMPT_VERSION,
    };
    assert_eq!(
        [
            CODE_PROMPT_VERSION,
            BUILD_PROMPT_VERSION,
            DATA_PROMPT_VERSION,
            MARKDOWN_PROMPT_VERSION
        ],
        [10, 11, 12, 10]
    );
    let env = redact_env();
    env.ok(&["build"]);
    set_prompt_version(&env, "A.java", 9);
    set_prompt_version(&env, "a.md", 9);
    set_prompt_version(&env, "app.yml", 11);
    set_prompt_version(&env, "package.json", 10);
    set_prompt_version(&env, "pom.xml", 10);
    assert_eq!(to_enrich(&env), 5);
    env.ok(&["update"]);
    assert_eq!(env.logged().len(), 5);
    assert_eq!(to_enrich(&env), 0);
    let again = env.ok(&["update"]);
    assert_eq!(text(&again.stdout), "graphe à jour\n");
}

#[test]
fn req167_ac5_omitted_trivial_accessor_is_accepted() {
    let env = tiny_env(&[(
        "src/A.java",
        "package p;\npublic class A {\n  private int x;\n  public int getX() { return x; }\n  public void run() { System.out.println(x); }\n}\n",
    )]);
    let id_of = |env: &Env, q: &str| {
        env.store()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.qualified == q)
            .unwrap_or_else(|| panic!("nœud absent : {q}"))
            .id
    };
    env.ok(&["build", "--no-llm"]);
    let class = id_of(&env, "p.A");
    let run = id_of(&env, "p.A.run");
    let get = id_of(&env, "p.A.getX");
    fs::create_dir_all(env.mock.join("src")).unwrap();
    fs::write(
        env.mock.join("src/A.java.json"),
        serde_json::json!({"version":2,"summaries":[
            {"node": class, "summary": "Porte un compteur et le lance."},
            {"node": run, "summary": "Affiche la valeur du champ."}
        ],"concepts":[]})
        .to_string(),
    )
    .unwrap();

    let b = env.ok(&["build"]);
    assert_eq!(env.logged().len(), 1, "{:?}", env.logged());
    assert!(
        !text(&b.stderr).contains("avertissement:"),
        "{}",
        text(&b.stderr)
    );
    assert!(!text(&b.stdout).contains("avertissement:"));
    assert_eq!(to_enrich(&env), 0);
    env.ok(&["update"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());

    let s = env.ok(&["query", "search", "run"]);
    assert!(
        text(&s.stdout).contains("Affiche la valeur du champ."),
        "{}",
        text(&s.stdout)
    );
    assert_eq!(summary_of(&env, &get).as_deref(), Some("return x;"));
    assert!(summary_of(&env, &run).is_some());
}

fn set_prompt_version(env: &Env, path: &str, v: i64) {
    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let n = conn
        .execute(
            "UPDATE enrichments SET prompt_version = ?1 WHERE path = ?2",
            rusqlite::params![v, path],
        )
        .unwrap();
    assert_eq!(n, 1, "{path}");
}

#[test]
fn req193_ac5_only_the_family_with_an_older_version_is_reenriched() {
    use nephila::llm::enrich::{
        BUILD_PROMPT_VERSION, CODE_PROMPT_VERSION, DATA_PROMPT_VERSION, MARKDOWN_PROMPT_VERSION,
    };
    let env = tiny_env(&[
        ("a.md", "# Un\n\ntexte\n"),
        (
            "src/A.java",
            "package p;\npublic class A {\n  public void run() {}\n}\n",
        ),
        (
            "pom.xml",
            "<project><modelVersion>4.0.0</modelVersion><groupId>g</groupId><artifactId>x</artifactId><version>1</version><dependencies><dependency><groupId>d</groupId><artifactId>e</artifactId><version>1</version></dependency></dependencies></project>\n",
        ),
        ("app.yml", "a: 1\nb: 2\n"),
    ]);
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 4\nconcurrency = 1\n",
    )
    .unwrap();
    env.ok(&["build"]);
    assert_eq!(to_enrich(&env), 0);
    let versions = || -> Vec<(String, i64)> {
        let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
        let mut st = conn
            .prepare("SELECT path, prompt_version FROM enrichments ORDER BY path")
            .unwrap();
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows
    };
    let seven = |p: &str, v: u32| (p.to_string(), i64::from(v));
    assert_eq!(
        versions(),
        vec![
            seven("a.md", MARKDOWN_PROMPT_VERSION),
            seven("app.yml", DATA_PROMPT_VERSION),
            seven("pom.xml", BUILD_PROMPT_VERSION),
            seven("src/A.java", CODE_PROMPT_VERSION),
        ]
    );
    for path in ["a.md", "src/A.java"] {
        set_prompt_version(&env, path, 6);
        assert_eq!(to_enrich(&env), 1, "{path}");
        env.ok(&["update"]);
        let logged = env.logged();
        assert_eq!(logged.len(), 1, "{path}: {logged:?}");
        assert_eq!(
            env.logged_paths(),
            BTreeSet::from([path.to_string()]),
            "{logged:?}"
        );
        assert_eq!(to_enrich(&env), 0);
        let again = env.ok(&["update"]);
        assert_eq!(text(&again.stdout), "graphe à jour\n");
        assert!(env.logged().is_empty(), "{:?}", env.logged());
    }
}

// ---- Appels conservés d'un gros fichier (REQ-226, REQ-227, DEC-173) ----

const BIG: &str = "docs/big.md";

fn big_doc(sections: usize) -> String {
    (1..=sections)
        .map(|i| format!("# Section{i}\n\n{}\n\n", "mot ".repeat(40)))
        .collect()
}

/// Configuration personnelle `mock` sans nouvel essai, `max_chunk_chars` = 250 par défaut.
fn write_config(env: &Env, model: &str, extra: &str) {
    let settings = if extra.contains("max_chunk_chars") {
        extra.to_string()
    } else {
        format!("max_chunk_chars = 250\n{extra}")
    };
    fs::write(
        &env.config,
        format!("provider = \"mock\"\nenrich_code = true\nmodel = \"{model}\"\nbatch_files = 1\nconcurrency = 1\nformat_retries = 0\n{settings}"),
    )
    .unwrap();
}

fn call_fixture(env: &Env, k: usize, rejected: bool) {
    let extra = if rejected && k == 1 {
        r#"{"node":"type:Absent","summary":"hors liste"},"#
    } else {
        ""
    };
    fs::create_dir_all(env.mock.join("docs")).unwrap();
    fs::write(
        env.mock.join(format!("{BIG}.{k}.json")),
        format!(
            r#"{{"version":2,"summaries":[{extra}{{"node":"document:{BIG}","summary":"resume{k}"}}],"concepts":[{{"name":"Concept{k}","summary":"c{k}","sections":["section:{BIG}#section{k}"]}}]}}"#
        ),
    )
    .unwrap();
}

/// Dépôt d'un seul gros fichier de `sections` appels, fixtures conformes.
fn big_env(sections: usize) -> Env {
    let env = tiny_env(&[(BIG, &big_doc(sections))]);
    write_config(&env, "mock", "");
    for k in 1..=sections {
        call_fixture(&env, k, false);
    }
    env
}

fn break_fixture(env: &Env, k: usize) {
    fs::write(env.mock.join(format!("{BIG}.{k}.json")), "{pas du json").unwrap();
}

/// Gros fichier de 4 appels dont le troisième est non conforme : `build` échoue après les appels
/// 1, 2 et 3. Le troisième est ensuite corrigé.
fn failed_after_three() -> Env {
    let env = big_env(4);
    break_fixture(&env, 3);
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert_eq!(env.logged(), calls(&[1, 2, 3]));
    call_fixture(&env, 3, false);
    env
}

fn calls(ks: &[usize]) -> Vec<String> {
    ks.iter().map(|k| format!("{BIG}\t{k}")).collect()
}

fn kept_rows(env: &Env) -> usize {
    env.store().call_results().unwrap().len()
}

fn export_json(env: &Env) -> String {
    text(
        &env.ok(&["export", "--format", "json", "--output", "-"])
            .stdout,
    )
}

#[test]
fn req227_ac1_reprise_n_envoie_que_les_appels_manquants() {
    let env = big_env(4);
    break_fixture(&env, 3);
    let o = env.run(&["build"]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(env.logged(), calls(&[1, 2, 3]));
    assert_eq!(to_enrich(&env), 1);
    assert!(env
        .store()
        .nodes()
        .unwrap()
        .iter()
        .all(|n| n.summary.is_none()));
    let warns: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect();
    assert_eq!(warns.len(), 1, "{err}");
    assert!(warns[0].contains(BIG), "{err}");
    assert_eq!(kept_rows(&env), 2);

    call_fixture(&env, 3, false);
    let o = env.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(env.logged(), calls(&[3, 4]));
    assert_eq!(kept_rows(&env), 0);
    assert_eq!(to_enrich(&env), 0);

    let clean = big_env(4);
    clean.ok(&["build"]);
    assert_eq!(export_json(&env), export_json(&clean));
}

#[test]
fn req227_ac3_morceau_modifie_ou_nouveau_nom_renvoient_ce_qui_change() {
    // Morceau 2 modifié sans déplacer de ligne ni changer de nœud : l'appel 1 ne repart pas.
    let env = failed_after_three();
    let doc = big_doc(4);
    let target = format!("# Section2\n\n{}", "mot ".repeat(40));
    let changed = doc.replace(&target, &format!("# Section2\n\n{}", "mon ".repeat(40)));
    assert_ne!(doc, changed);
    env.write(BIG, &changed);
    env.ok(&["update"]);
    assert_eq!(env.logged(), calls(&[2, 3, 4]));

    // Ligne insérée en tête : tous les appels partent.
    let env = failed_after_three();
    env.write(BIG, &format!("intro\n\n{}", big_doc(4)));
    env.ok(&["update"]);
    assert_eq!(env.logged(), calls(&[1, 2, 3, 4]));

    // Nœud ajouté dans le dernier morceau : la liste des nœuds connus change, tout repart.
    let env = failed_after_three();
    env.write(BIG, &format!("{}# Extra\n\nx\n", big_doc(4)));
    env.ok(&["update"]);
    assert_eq!(env.logged(), calls(&[1, 2, 3, 4]));
}

#[test]
fn req227_ac4_modele_langue_et_taille_de_morceau() {
    // Modèle changé : tous les appels partent.
    let env = failed_after_three();
    write_config(&env, "autre", "");
    env.ok(&["update"]);
    assert_eq!(env.logged(), calls(&[1, 2, 3, 4]));

    // Langue changée : de même.
    let env = failed_after_three();
    write_config(&env, "mock", "summary_language = \"english\"\n");
    env.ok(&["update"]);
    assert_eq!(env.logged(), calls(&[1, 2, 3, 4]));

    // Morceaux inchangés malgré une autre taille maximale : seuls les appels manquants partent.
    let env = failed_after_three();
    write_config(&env, "mock", "max_chunk_chars = 260\n");
    env.ok(&["update"]);
    assert_eq!(env.logged(), calls(&[3, 4]));

    // Morceaux regroupés (le plan change) : les appels du nouveau plan partent.
    let env = failed_after_three();
    write_config(&env, "mock", "max_chunk_chars = 400\n");
    env.ok(&["update"]);
    assert_eq!(env.logged(), calls(&[1, 2]));

    // Concurrence et nouveaux essais : seuls les appels manquants partent.
    let env = failed_after_three();
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 3\nformat_retries = 2\nmax_chunk_chars = 250\n",
    )
    .unwrap();
    env.ok(&["update"]);
    let mut sent = env.logged();
    sent.sort();
    assert_eq!(sent, calls(&[3, 4]));
}

#[test]
fn req227_ac6_journal_verbose_et_progression_de_la_reprise() {
    let env = failed_after_three();
    let o = env.ok(&["update", "--verbose"]);
    let err = text(&o.stderr);
    let enrich: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("llm enrich") && l.contains(BIG))
        .collect();
    assert!(
        enrich
            .iter()
            .all(|l| l.contains("[3/4]") || l.contains("[4/4]")),
        "{err}"
    );
    assert_eq!(enrich.len(), 4, "départ et fin de deux appels : {err}");
    let summary: Vec<&str> = err.lines().filter(|l| l.contains(" appels: ")).collect();
    assert_eq!(summary.len(), 1, "{err}");
    assert!(summary[0].contains("appels: 2 appels,"), "{err}");

    let env = failed_after_three();
    let o = env.ok(&["update", "--show-progress"]);
    let progress: Vec<String> = text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        progress,
        [
            "progression: enrichissement 0 % (0/1 fichiers)",
            "progression: enrichissement 100 % (1/1 fichiers)"
        ]
    );
}

/// Fabrique l'état « tous les appels conservés » d'un gros fichier de `sections` appels
/// (REQ-227 : un échec réel ne le produit pas) : un déclencheur empêche la purge des lignes à la
/// réussite, puis le fichier redevient « à enrichir ».
fn keep_all_calls(env: &Env, sections: usize) {
    // Sans concept : un concept déclaré par le fichier changerait la liste des nœuds connus.
    for k in 1..=sections {
        fs::write(
            env.mock.join(format!("{BIG}.{k}.json")),
            format!(r#"{{"version":2,"summaries":[{{"node":"section:{BIG}#section{k}","summary":"s{k}"}}],"concepts":[]}}"#),
        )
        .unwrap();
    }
    env.ok(&["build", "--no-llm"]);
    let db = env.repo.join(".nephila/graph.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER keep BEFORE DELETE ON enrich_calls BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    drop(conn);
    env.ok(&["update"]);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP TRIGGER keep;").unwrap();
    conn.execute("DELETE FROM enrichments WHERE path = ?1", [BIG])
        .unwrap();
}

#[test]
fn req227_ac6_tous_les_appels_conserves_aucun_appel_fichier_enrichi() {
    let env = big_env(4);
    keep_all_calls(&env, 4);
    assert_eq!(kept_rows(&env), 4);
    assert_eq!(to_enrich(&env), 1);
    let o = env.ok(&["update"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert_eq!(to_enrich(&env), 0);
    assert_eq!(kept_rows(&env), 0);
    // Les appels conservés sont écrits par cette commande : le segment suit (REQ-235).
    assert!(without_coverage(&text(&o.stdout)).ends_with(", 0 fichiers à enrichir"));
}

#[test]
fn req227_ac8_element_ecarte_signale_une_fois_a_la_reussite() {
    let env = big_env(4);
    call_fixture(&env, 1, true);
    break_fixture(&env, 3);
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        !text(&o.stderr).contains("écarté(s)"),
        "{}",
        text(&o.stderr)
    );
    call_fixture(&env, 3, false);
    let o = env.ok(&["update"]);
    let err = text(&o.stderr);
    let warns: Vec<&str> = err.lines().filter(|l| l.contains("écarté(s)")).collect();
    assert_eq!(warns.len(), 1, "{err}");

    let clean = big_env(4);
    call_fixture(&clean, 1, true);
    let o = clean.ok(&["build"]);
    let expected: Vec<String> = text(&o.stderr)
        .lines()
        .filter(|l| l.contains("écarté(s)"))
        .map(str::to_string)
        .collect();
    assert_eq!(warns, expected);
}

#[test]
fn req227_ac9_lot_et_fichier_d_un_seul_appel_n_ecrivent_rien() {
    // Lot en échec : deux petits documents envoyés ensemble.
    let env = tiny_env(&[("a.md", "# Un\n\ntexte\n"), ("b.md", "# Deux\n\ntexte\n")]);
    fs::write(
        &env.config,
        "provider = \"mock\"\nmodel = \"mock\"\nbatch_files = 2\nconcurrency = 1\nformat_retries = 0\n",
    )
    .unwrap();
    let _ = fs::remove_file(&env.log);
    let o = env
        .cmd()
        .env("NEPHILA_MOCK_FAIL_AFTER", "0")
        .arg("build")
        .output()
        .unwrap();
    assert!(!o.status.success(), "{}", text(&o.stderr));
    assert_eq!(kept_rows(&env), 0);
    let o = env.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(env.logged(), vec!["a.md;b.md\t1".to_string()]);

    // Fichier d'un seul appel en échec.
    let env = tiny_env(&[("a.md", "# Un\n\ntexte\n")]);
    write_config(&env, "mock", "");
    fs::write(env.mock.join("a.md.1.json"), "{pas du json").unwrap();
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(kept_rows(&env), 0);
    fs::remove_file(env.mock.join("a.md.1.json")).unwrap();
    env.ok(&["update"]);
    assert_eq!(env.logged(), vec!["a.md\t1".to_string()]);
}

#[test]
fn req227_ac11_reprise_silencieuse_sans_option() {
    let env = failed_after_three();
    let o = env.ok(&["update"]);
    assert_eq!(text(&o.stderr), "");
    let out = text(&o.stdout);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(
        without_coverage(&out).ends_with(", 0 fichiers à enrichir"),
        "{out}"
    );

    let env = failed_after_three();
    let o = env.ok(&["update", "--verbose"]);
    for line in text(&o.stderr).lines() {
        let without_tokens = line.split(" (jetons").next().unwrap();
        for word in [
            "appels conservés",
            "appel conservé",
            "réponse gardée",
            "cache",
            "[1/2]",
            "[2/2]",
        ] {
            assert!(
                !without_tokens.contains(word) || line.starts_with("appels:"),
                "{word} : {line}"
            );
        }
        assert!(
            !line.contains("[1/4]") && !line.contains("[2/4]") || !line.contains("llm enrich"),
            "{line}"
        );
    }
}

#[test]
fn req226_ac6_build_garde_les_lignes_et_la_suppression_du_fichier_les_purge() {
    let env = failed_after_three();
    assert_eq!(kept_rows(&env), 2);
    env.ok(&["build", "--no-llm"]);
    assert_eq!(
        kept_rows(&env),
        2,
        "build réinitialise le graphe sans toucher aux lignes"
    );
    fs::remove_file(env.repo.join(BIG)).unwrap();
    env.ok(&["update"]);
    assert_eq!(kept_rows(&env), 0);
}

#[test]
fn req227_ac7_apres_kill_9_les_appels_des_groupes_valides_ne_repartent_pas() {
    let env = big_env(4);
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
        .env_remove("NEPHILA_MOCK_FAIL_AFTER")
        .env("NEPHILA_MOCK_DELAY_MS", "300")
        .env("NEPHILA_CONFIG", &env.config)
        .env("NEPHILA_MOCK_DIR", &env.mock)
        .env("NEPHILA_MOCK_LOG", &env.log)
        .arg("-C")
        .arg(&env.repo)
        .arg("build")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    loop {
        assert!(start.elapsed().as_secs() < 30, "aucun appel conservé");
        if Store::open_readonly(&env.repo)
            .is_ok_and(|s| s.call_results().is_ok_and(|r| !r.is_empty()))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let kept = kept_rows(&env);
    assert!((1..4).contains(&kept), "{kept}");
    assert_eq!(to_enrich(&env), 1);
    env.ok(&["update"]);
    assert_eq!(env.logged().len(), 4 - kept, "{:?}", env.logged());
    assert_eq!(to_enrich(&env), 0);
}

// ---- AC-232-13 : appel tronqué récupéré et appels conservés ----

fn openai_body(text: &str, finish: &str) -> String {
    serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": finish}]
    })
    .to_string()
}

struct Served {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: fake_server::FakeServer,
}

impl Served {
    /// `A.java` de 60 méthodes en 2 appels ; les réponses successives du faux serveur.
    fn new(text: &str, max_chunk_chars: usize, replies: Vec<String>) -> Served {
        let server =
            fake_server::FakeServer::start(replies.into_iter().map(|b| (200, b)).collect());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("A.java"), text).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T322_KEY\"\nconcurrency = 1\nformat_retries = 0\nmax_chunk_chars = {max_chunk_chars}\n",
                server.base_url
            ),
        )
        .unwrap();
        Served {
            _tmp: tmp,
            repo,
            config,
            server,
        }
    }

    fn run(&self, cmd: &str) -> Output {
        std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .arg(cmd)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T322_KEY", "sk-test")
            .output()
            .unwrap()
    }

    fn posts(&self) -> usize {
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == "/v1/chat/completions")
            .count()
    }

    fn kept(&self) -> usize {
        Store::open_readonly(&self.repo)
            .unwrap()
            .call_results()
            .unwrap()
            .len()
    }
}

/// Texte de `A.java` (60 méthodes), borne de morceau donnant 2 morceaux, identifiants demandés
/// à chaque appel (lus dans les requêtes d'un premier essai).
fn two_calls() -> (String, usize, [Vec<String>; 2]) {
    let mut text = String::from("public class A {\n");
    for i in 0..60 {
        text.push_str(&format!("    void m{i}() {{}}\n"));
    }
    text.push_str("}\n");
    let max = text.len() / 2 + 40;
    assert_eq!(nephila::chunk::split(&text, max).len(), 2);
    let empty = openai_body(r#"{"version": 2, "summaries": [], "concepts": []}"#, "stop");
    let probe = Served::new(&text, max, vec![empty.clone(), empty]);
    probe.run("build");
    let bodies: Vec<String> = probe
        .server
        .requests()
        .into_iter()
        .map(|r| r.body)
        .collect();
    assert_eq!(bodies.len(), 2);
    let ids = |b: &str| -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(b).unwrap();
        let user = v["messages"][1]["content"].as_str().unwrap().to_string();
        user.split_once("Nœuds du fichier :")
            .unwrap()
            .1
            .lines()
            .filter_map(|l| {
                l.strip_prefix('[')
                    .and_then(|r| r.split_once(" — "))
                    .map(|(_, id)| id)
            })
            .map(String::from)
            .collect()
    };
    let lists = [ids(&bodies[0]), ids(&bodies[1])];
    (text, max, lists)
}

fn summaries_text(ids: &[String], cut: Option<&str>) -> String {
    let items: Vec<String> = ids
        .iter()
        .map(|n| serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")}).to_string())
        .collect();
    match cut {
        Some(c) => format!(
            r#"{{"version": 2, "summaries": [{}{}{c}"#,
            items.join(","),
            if items.is_empty() { "" } else { "," }
        ),
        None => serde_json::json!({
            "version": 2,
            "summaries": items.iter().map(|i| serde_json::from_str::<serde_json::Value>(i).unwrap()).collect::<Vec<_>>(),
            "concepts": []
        })
        .to_string(),
    }
}

#[test]
fn req232_ac13_sous_appel_non_conforme_ne_garde_que_l_appel_un() {
    let (java, max, lists) = two_calls();
    let good = openai_body(&summaries_text(&lists[0], None), "stop");
    let partial = openai_body(
        &summaries_text(&lists[1][..1], Some(r#"{"node": "x", "summ"#)),
        "length",
    );
    let bad = openai_body("pas du JSON", "stop");
    let full2 = openai_body(&summaries_text(&lists[1], None), "stop");
    let s = Served::new(&java, max, vec![good, partial, bad, full2]);
    let o = s.run("build");
    let err = text(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert_eq!(s.posts(), 3, "{err}");
    assert_eq!(err.matches("avertissement:").count(), 1, "{err}");
    assert!(err.contains("non conforme"), "{err}");
    assert_eq!(s.kept(), 1, "seul l'appel 1 est conservé");
    // `update` : l'appel 2 répond en entier, l'appel 1 est relu de la table.
    let o = s.run("update");
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(s.posts(), 4);
    assert_eq!(s.kept(), 0);
}

#[test]
fn req232_ac13_appel_un_recupere_en_entier_est_conserve_quand_l_appel_deux_echoue() {
    let (java, max, lists) = two_calls();
    // Appel 1 tronqué après tous ses résumés : cas (1), sa clé est écrite.
    let cut = format!(
        "{}, \"concepts\": [{{\"name\": \"x\", \"su",
        summaries_text(&lists[0], Some(""))
            .trim_end_matches([',', ' '])
            .to_string()
            + "]"
    );
    let all_cut = openai_body(&cut, "length");
    let bad = openai_body("pas du JSON", "stop");
    let s = Served::new(&java, max, vec![all_cut, bad]);
    let o = s.run("build");
    let err = text(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert_eq!(s.posts(), 2, "{err}");
    assert_eq!(s.kept(), 1, "{err}");
}

/// Bilan sans son segment de couverture (REQ-235), qui doit être présent.
fn without_coverage(out: &str) -> String {
    let (head, tail) = out.trim_end().rsplit_once(", ").unwrap();
    assert!(tail.ends_with(" nœuds résumés"), "{out}");
    head.to_string()
}

/// Réponse à numéros : le nœud `k` de la liste reçoit `Résumé de <id>.`
fn numbered_text(ids: &[String]) -> String {
    let items: Vec<serde_json::Value> = ids
        .iter()
        .enumerate()
        .map(|(i, n)| serde_json::json!({"n": i + 1, "summary": format!("Résumé de {n}.")}))
        .collect();
    serde_json::json!({"summaries": items, "concepts": []}).to_string()
}

#[test]
#[allow(non_snake_case)]
fn REQ237_AC8_resumes_retraduits_d_un_appel_conserve_restent_sur_leurs_noeuds() {
    let (java, max, lists) = two_calls();
    let first = openai_body(&numbered_text(&lists[0]), "stop");
    let bad = openai_body("pas du JSON", "stop");
    let second = openai_body(&numbered_text(&lists[1]), "stop");
    let s = Served::new(&java, max, vec![first, bad, second]);
    let o = s.run("build");
    let err = text(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert_eq!(s.posts(), 2, "{err}");
    assert_eq!(s.kept(), 1, "{err}");
    let o = s.run("update");
    assert!(o.status.success(), "{}", text(&o.stderr));
    // L'appel 1 n'est pas renvoyé : une seule requête de plus, pour l'appel 2.
    assert_eq!(s.posts(), 3);
    assert_eq!(s.kept(), 0);
    let nodes = Store::open_readonly(&s.repo).unwrap().nodes().unwrap();
    for id in lists[0].iter().chain(&lists[1]) {
        let n = nodes.iter().find(|n| &n.id == id).expect(id);
        assert_eq!(
            n.summary.as_deref(),
            Some(format!("Résumé de {id}.").as_str()),
            "{id}"
        );
    }
}

#[test]
fn req241_ac6_data_family_previous_version_graph_reenriches_only_data_files() {
    let env = tiny_env(&[
        ("a.md", "# Un\n\ntexte\n"),
        ("A.java", "public class A {\n  void run() {}\n}\n"),
        ("app.yml", "port: 80\n"),
    ]);
    env.ok(&["build"]);
    assert_eq!(to_enrich(&env), 0);

    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let n = conn
        .execute(
            "UPDATE enrichments SET prompt_version = CASE WHEN path = 'app.yml' THEN 11 ELSE 10 END",
            [],
        )
        .unwrap();
    assert_eq!(n, 3);
    drop(conn);
    assert_eq!(to_enrich(&env), 1);

    env.ok(&["update"]);
    assert_eq!(env.logged_paths(), BTreeSet::from(["app.yml".to_string()]));
    assert_eq!(to_enrich(&env), 0);

    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let version = |p: &str| -> i64 {
        conn.query_row(
            "SELECT prompt_version FROM enrichments WHERE path = ?1 ORDER BY used_at DESC LIMIT 1",
            [p],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(version("app.yml"), 12);
    assert_eq!(version("a.md"), 10);
    assert_eq!(version("A.java"), 10);
    drop(conn);

    let again = env.ok(&["update"]);
    assert_eq!(text(&again.stdout), "graphe à jour\n");
    assert!(env.logged().is_empty(), "{:?}", env.logged());
}

const MARK: &str = "marque-origine";
const SVC_TYPE: &str = "type:com.acme.core.OrderService";

/// Marque en SQL le résumé du type de `path` dans la charge utile de ses entrées du modèle `model`.
fn mark_payload(env: &Env, path: &str, model: &str) {
    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    let (rowid, json): (i64, String) = conn
        .query_row(
            "SELECT rowid, payload FROM enrichments WHERE path = ?1 AND model = ?2",
            [path, model],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let pair = v["summaries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|p| p[0] == SVC_TYPE)
        .expect("résumé du type");
    pair[1] = MARK.into();
    conn.execute(
        "UPDATE enrichments SET payload = ?2 WHERE rowid = ?1",
        rusqlite::params![rowid, v.to_string()],
    )
    .unwrap();
}

#[test]
fn req258_ac4_returning_to_original_content_reapplies_its_entry_without_call() {
    for verb in ["update", "build"] {
        let env = Env::new();
        env.ok(&["build"]);
        mark_payload(&env, SVC, "mock");
        let original = fs::read_to_string(env.repo.join(SVC)).unwrap();
        env.write(SVC, &format!("{original}\n// autre branche\n"));
        env.ok(&["update"]);
        assert_eq!(
            env.logged_paths(),
            BTreeSet::from([SVC.to_string()]),
            "{verb}"
        );
        assert_ne!(summary_of(&env, SVC_TYPE).as_deref(), Some(MARK));

        env.write(SVC, &original);
        env.ok(&[verb]);
        assert!(env.logged().is_empty(), "{verb} : {:?}", env.logged());
        assert_eq!(to_enrich(&env), 0, "{verb}");
        assert_eq!(summary_of(&env, SVC_TYPE).as_deref(), Some(MARK), "{verb}");
    }
}

#[test]
fn req258_ac5_returning_to_previous_model_or_language_reapplies_entry_without_call() {
    for verb in ["update", "build"] {
        for language in [false, true] {
            let env = Env::new();
            env.ok(&["build"]);
            mark_payload(&env, SVC, "mock");
            let files = all_paths(&env);
            if language {
                set_personal_language(&env, "anglais");
            } else {
                set_personal_model(&env, "autre");
            }
            env.ok(&["update"]);
            assert_eq!(env.logged_paths(), files, "{verb} {language}");
            assert_eq!(env.logged().len(), files.len(), "{verb} {language}");
            assert_ne!(summary_of(&env, SVC_TYPE).as_deref(), Some(MARK));

            set_personal_model(&env, "mock");
            env.ok(&[verb]);
            assert!(
                env.logged().is_empty(),
                "{verb} {language} : {:?}",
                env.logged()
            );
            assert_eq!(to_enrich(&env), 0, "{verb} {language}");
            assert_eq!(summary_of(&env, SVC_TYPE).as_deref(), Some(MARK));

            let again = env.ok(&["update"]);
            assert_eq!(text(&again.stdout), "graphe à jour\n", "{verb} {language}");
            assert!(env.logged().is_empty());
        }
    }
}

// ---- REQ-265 : réglage enrich_redact et version effective (T-358) ----

const POM: &str = "<project><modelVersion>4.0.0</modelVersion><groupId>g</groupId><artifactId>x</artifactId><version>1</version><dependencies><dependency><groupId>d</groupId><artifactId>e</artifactId><version>1</version></dependency></dependencies></project>\n";

fn redact_env() -> Env {
    let env = tiny_env(&[
        ("a.md", "# Un\n\ntexte\n"),
        ("A.java", "public class A {\n  void run() {}\n}\n"),
        ("pom.xml", POM),
        ("app.yml", "port: 80\n"),
        ("package.json", "{\"name\":\"x\",\"version\":\"1.0.0\"}\n"),
    ]);
    set_redact(&env, true);
    env
}

fn set_redact(env: &Env, on: bool) {
    fs::write(
        &env.config,
        format!("provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\nenrich_redact = {on}\n"),
    )
    .unwrap();
}

fn targeted() -> BTreeSet<String> {
    BTreeSet::from(["app.yml".to_string(), "package.json".to_string()])
}

fn bascule(cmd: &str) {
    let env = redact_env();
    env.ok(&["build"]);
    assert_eq!(to_enrich(&env), 0);
    set_redact(&env, false);
    env.ok(&[cmd]);
    assert_eq!(env.logged_paths(), targeted(), "{:?}", env.logged());
    assert_eq!(env.logged().len(), 2);
    assert_eq!(to_enrich(&env), 0);
    let again = env.ok(&["update"]);
    assert_eq!(text(&again.stdout), "graphe à jour\n");
    assert!(env.logged().is_empty());
    set_redact(&env, true);
    let back = env.ok(&[cmd]);
    assert_eq!(back.status.code(), Some(0));
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert_eq!(to_enrich(&env), 0);
}

#[test]
fn req265_ac3_switching_redact_resends_only_targeted_files_by_update() {
    bascule("update");
}

#[test]
fn req265_ac3_switching_redact_resends_only_targeted_files_by_build() {
    bascule("build");
}

#[test]
fn req265_ac4_dry_run_estimates_targeted_files_and_records_nothing() {
    let env = redact_env();
    env.ok(&["build"]);
    set_redact(&env, false);
    let meta = || -> String {
        let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'enrich_redact'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(meta(), "true");
    for cmd in ["update", "build"] {
        let o = env.ok(&[cmd, "--dry-run"]);
        let out = text(&o.stdout);
        assert!(
            out.lines()
                .any(|l| l.starts_with("estimation : 2 fichiers à enrichir")),
            "{cmd}: {out}"
        );
        assert!(env.logged().is_empty());
        assert_eq!(meta(), "true");
    }
    set_redact(&env, true);
    let o = env.ok(&["update", "--dry-run"]);
    assert!(
        !text(&o.stdout).contains("estimation : 2 fichiers"),
        "{}",
        text(&o.stdout)
    );
}

#[test]
fn req265_ac5_versions_and_old_graph_reenriches_data_and_build_families_once() {
    use nephila::llm::enrich::{
        BUILD_PROMPT_VERSION, CODE_PROMPT_VERSION, DATA_PROMPT_VERSION, MARKDOWN_PROMPT_VERSION,
    };
    assert_eq!(DATA_PROMPT_VERSION, 12);
    assert_eq!(BUILD_PROMPT_VERSION, 11);
    assert_eq!(CODE_PROMPT_VERSION, 10);
    assert_eq!(MARKDOWN_PROMPT_VERSION, 10);
    assert_eq!(SCHEMA_VERSION, 14);
    let env = redact_env();
    env.ok(&["build"]);
    set_prompt_version(&env, "app.yml", 11);
    set_prompt_version(&env, "package.json", 10);
    set_prompt_version(&env, "pom.xml", 10);
    assert_eq!(to_enrich(&env), 3);
    env.ok(&["update"]);
    assert_eq!(
        env.logged_paths(),
        BTreeSet::from([
            "app.yml".to_string(),
            "package.json".to_string(),
            "pom.xml".to_string()
        ])
    );
    assert_eq!(env.logged().len(), 3);
    assert_eq!(to_enrich(&env), 0);
    let again = env.ok(&["update"]);
    assert_eq!(text(&again.stdout), "graphe à jour\n");
    assert!(env.logged().is_empty());
}

#[test]
fn req265_ac4_dry_run_keeps_the_applied_summary_when_both_entries_exist() {
    let env = tiny_env(&[("app.yml", "port: 80\n")]);
    set_redact(&env, true);
    env.ok(&["build", "--no-llm"]);
    let id = env
        .store()
        .nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.file.as_deref() == Some("app.yml"))
        .expect("nœud de app.yml")
        .id;
    let mock = |summary: &str| {
        fs::write(
            env.mock.join("app.yml.json"),
            format!(
                "{{\"version\":2,\"summaries\":[{{\"node\":\"{id}\",\"summary\":\"{summary}\"}}],\"concepts\":[]}}"
            ),
        )
        .unwrap();
    };
    mock("MASQUE");
    env.ok(&["build"]);
    assert_eq!(summary_of(&env, &id).as_deref(), Some("MASQUE"));
    set_redact(&env, false);
    mock("NON_MASQUE");
    env.ok(&["update"]);
    assert_eq!(summary_of(&env, &id).as_deref(), Some("NON_MASQUE"));
    set_redact(&env, true);
    env.ok(&["update"]);
    assert_eq!(summary_of(&env, &id).as_deref(), Some("MASQUE"));
    set_redact(&env, false);
    for cmd in ["build", "update"] {
        env.ok(&[cmd, "--dry-run"]);
        assert_eq!(summary_of(&env, &id).as_deref(), Some("MASQUE"), "{cmd}");
        assert!(env.logged().is_empty());
    }
}

// ---- REQ-264 : masquage à la lecture (T-359) ----

#[allow(dead_code)]
#[path = "support/batch_server.rs"]
mod batch_server;

const SECRET: &str = "SECRET_VAL_43";
const PEM_BODY: &str = "PEMBODY_43";

fn secret_files() -> Vec<(&'static str, String)> {
    vec![
        ("config/app.yml", format!("password: {SECRET}\nname: app\n")),
        (
            "db/a.sql",
            format!("INSERT INTO u (id, api_key, nom) VALUES (1, '{SECRET}', 'a');\n"),
        ),
        (
            "conf/b.xml",
            format!("<conf><password>{SECRET}</password></conf>\n"),
        ),
        ("c.json", format!("{{\"token\": \"{SECRET}\", \"n\": 1}}\n")),
        (
            "package.json",
            format!("{{\"name\": \"p\", \"secret\": \"{SECRET}\"}}\n"),
        ),
        (
            "k.yml",
            format!("cert:\n-----BEGIN CERT-----\n{PEM_BODY}\n-----END CERT-----\n"),
        ),
    ]
}

struct Redacted {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
}

impl Redacted {
    fn new(files: &[(&str, String)], batch_files: usize, extra: &str) -> (Redacted, String) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        for (p, c) in files {
            let path = repo.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, c).unwrap();
        }
        let config = tmp.path().join("config.toml");
        (
            Redacted {
                _tmp: tmp,
                repo,
                config,
            },
            format!(
                "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\napi_key_env = \"NEPHILA_T359_KEY\"\nconcurrency = 1\nformat_retries = 0\nbatch_files = {batch_files}\n{extra}"
            ),
        )
    }

    fn run(&self, base_url: &str, cfg: &str, args: &[&str]) -> Output {
        fs::write(&self.config, format!("{cfg}base_url = \"{base_url}\"\n")).unwrap();
        std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T359_KEY", "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "1")
            .env("NEPHILA_BATCH_POLL_MS", "5")
            .output()
            .unwrap()
    }
}

/// Corps des requêtes reçues par un faux serveur `openai` après `build` (réponses vides).
fn sent_bodies(files: &[(&str, String)], batch_files: usize, extra: &str) -> Vec<String> {
    let (env, cfg) = Redacted::new(files, batch_files, extra);
    let empty = openai_body(r#"{"version": 2, "summaries": [], "concepts": []}"#, "stop");
    let server = fake_server::FakeServer::start((0..60).map(|_| (200, empty.clone())).collect());
    env.run(&server.base_url, &cfg, &["build"]);
    server
        .requests()
        .into_iter()
        .filter(|r| r.path == "/v1/chat/completions")
        .map(|r| r.body)
        .collect()
}

/// Chaînes des requêtes qui portent des fichiers (celles qui contiennent `Contenu :`).
fn contents_of(values: &[serde_json::Value]) -> Vec<String> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) if s.contains("Contenu :") => out.push(s.clone()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    values.iter().for_each(|v| walk(v, &mut out));
    out
}

fn contents_of_bodies(bodies: &[String]) -> Vec<String> {
    let values: Vec<serde_json::Value> = bodies
        .iter()
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    contents_of(&values)
}

/// Partie d'une requête qui porte `path` : le message d'un fichier seul (`Fichier : <chemin>`)
/// ou la section du fichier dans un lot.
fn section_of(contents: &[String], path: &str) -> Option<String> {
    for c in contents {
        let head = c.lines().next().unwrap_or("");
        if head == format!("Fichier : {path}") || head.starts_with(&format!("Fichier : {path} (")) {
            return Some(c.clone());
        }
        let open = format!(" : {path} ===\n");
        let close = format!("=== FIN DU FICHIER : {path} ===");
        if let (Some(i), Some(j)) = (c.find(&open), c.find(&close)) {
            if i < j {
                return Some(c[i..j].to_string());
            }
        }
    }
    None
}

/// Clé qui doit rester visible dans le texte envoyé de chaque fichier.
const KEYS: [(&str, &str); 6] = [
    ("config/app.yml", "password"),
    ("db/a.sql", "api_key"),
    ("conf/b.xml", "password"),
    ("c.json", "token"),
    ("package.json", "secret"),
    ("k.yml", "BEGIN CERT"),
];

fn assert_masked(contents: &[String]) {
    assert!(!contents.is_empty());
    let all = contents.join("\n");
    assert!(!all.contains(SECRET), "valeur envoyée : {all}");
    assert!(!all.contains(PEM_BODY), "bloc PEM envoyé");
    for (path, key) in KEYS {
        let part = section_of(contents, path).unwrap_or_else(|| panic!("{path} non envoyé"));
        assert!(part.contains(key), "clé {key} absente de {path} : {part}");
        assert!(part.contains("***"), "marque absente de {path} : {part}");
    }
}

#[test]
fn req264_ac9_batch_files_1_and_2_send_no_secret() {
    for n in [1, 2] {
        assert_masked(&contents_of_bodies(&sent_bodies(&secret_files(), n, "")));
    }
}

#[test]
fn req264_ac9_redact_false_sends_the_marker() {
    let bodies = sent_bodies(&secret_files(), 1, "enrich_redact = false\n");
    assert!(bodies.join("\n").contains(SECRET));
}

#[test]
fn req264_ac9_batch_mode_requests_carry_no_secret() {
    use batch_server::{BatchServer, Outcome};
    let (env, cfg) = Redacted::new(&secret_files(), 2, "");
    let server = BatchServer::start();
    server.simulate("openai", 0, |_| {
        Outcome::Text(r#"{"version": 2, "summaries": [], "concepts": []}"#.into())
    });
    env.run(&server.base_url, &cfg, &["build", "--batch"]);
    let lots = server.lots();
    assert!(!lots.is_empty(), "aucun lot créé");
    let flat: Vec<serde_json::Value> = lots.into_iter().flatten().collect();
    assert_masked(&contents_of(&flat));
}

#[test]
fn req264_ac10_excerpt_after_masking_keeps_key_skeleton_and_drops_value() {
    use nephila::excerpt::for_llm;
    use nephila::llm::redact::redact;
    let filler: String = (0..800).map(|i| format!("  k{i}: v{i}\n")).collect();
    let yaml = format!("root:\n  password: {SECRET}\n{filler}");
    let filler_json: String = (0..800).map(|i| format!("\"k{i}\": \"v{i}\", ")).collect();
    let json = format!("{{\"token\": \"{SECRET}\", {filler_json}\"z\": 1}}");
    for (path, text, key) in [("a.yml", yaml, "password"), ("a.json", json, "token")] {
        assert!(text.chars().count() > 7000);
        let (masked, n) = redact(path, &text);
        assert_eq!(n, 1, "{path}");
        let sent = for_llm(path, &masked, 3000);
        assert!(sent.contains("extrait tronqué"), "{path}");
        assert!(!sent.contains(SECRET), "{path}");
        assert!(sent.contains(key), "{path}");
        assert!(sent.contains("[squelette des clés]"), "{path}");
    }
    let sql = format!(
        "CREATE TABLE u (id int, api_key text);\nINSERT INTO u VALUES (1, 'a');\nINSERT INTO u (id, api_key) VALUES (2, '{SECRET}');\n{}",
        (0..300)
            .map(|i| format!("INSERT INTO u (id, api_key) VALUES ({i}, 'zzzzzzzzzz{i}');\n"))
            .collect::<String>()
    );
    let (masked, n) = redact("a.sql", &sql);
    assert!(n >= 1);
    let sent = for_llm("a.sql", &masked, 60000);
    assert!(sent.contains("instructions INSERT ou COPY retirées"));
    assert!(sent.contains("'***'"));
    assert!(sent.contains("api_key"));
    assert!(!sent.contains(SECRET));
}

#[test]
fn req264_ac11_graph_identical_with_and_without_masking_and_lines_kept() {
    let files = secret_files();
    let build = |on: bool| {
        let (env, _) = Redacted::new(&files, 1, "");
        let cfg = format!("provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\nenrich_redact = {on}\n");
        fs::write(&env.config, cfg).unwrap();
        let o = std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
            .arg("-C")
            .arg(&env.repo)
            .arg("build")
            .env("NEPHILA_CONFIG", &env.config)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", text(&o.stderr));
        let st = Store::open_readonly(&env.repo).unwrap();
        let (nodes, edges, refs) = snapshot(&st);
        let ids: Vec<String> = nodes.into_iter().map(|n| n.id).collect();
        (ids, edges, refs)
    };
    assert_eq!(build(true), build(false));
    // Le masquage ne change jamais le nombre de lignes : les numéros d'un extrait restent valides.
    let sql =
        format!("-- c\nCREATE TABLE t (pwd text);\nINSERT INTO t (pwd) VALUES ('{SECRET}');\n");
    let (masked, n) = nephila::llm::redact::redact("big.sql", &sql);
    assert_eq!(n, 1);
    assert_eq!(masked.matches('\n').count(), sql.matches('\n').count());
    let before: Vec<usize> = nephila::chunk::split(&sql, 40)
        .iter()
        .map(|c| c.matches('\n').count())
        .collect();
    let after: Vec<usize> = nephila::chunk::split(&masked, 40)
        .iter()
        .map(|c| c.matches('\n').count())
        .collect();
    assert_eq!(before.iter().sum::<usize>(), after.iter().sum::<usize>());
}

#[test]
fn req264_ac12_file_without_value_is_sent_unchanged() {
    let files = vec![("plain.yml", "port: 80\nname: app\n".to_string())];
    let with = sent_bodies(&files, 1, "");
    let without = sent_bodies(&files, 1, "enrich_redact = false\n");
    assert!(!with.is_empty());
    assert_eq!(with, without);
}

#[test]
fn req264_ac11_sql_chunks_keep_original_line_numbers() {
    let original: Vec<String> = (0..60)
        .map(|i| {
            if i % 10 == 0 {
                format!("ALTER USER u{i} WITH PASSWORD '{SECRET}';")
            } else {
                format!("CREATE TABLE t{i} (id int);")
            }
        })
        .collect();
    let file = format!("{}\n", original.join("\n"));
    let bodies = sent_bodies(&[("big.sql", file)], 1, "max_chunk_chars = 500\n");
    let contents = contents_of_bodies(&bodies);
    assert!(contents.len() >= 2, "un seul morceau");
    let mut covered = 0;
    for c in &contents {
        assert_eq!(c.lines().next().unwrap(), "Fichier : big.sql");
        let head = c
            .lines()
            .find(|l| l.starts_with("(extrait "))
            .expect("indice d'extrait");
        let range = head.split("lignes ").nth(1).unwrap().trim_end_matches(')');
        let (a, b) = range.split_once('–').unwrap();
        let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
        let sent = c
            .split("Contenu :\n")
            .nth(1)
            .unwrap()
            .split("\n\nNœuds du fichier :")
            .next()
            .unwrap()
            .trim_end();
        let expected: Vec<String> = original[a - 1..b]
            .iter()
            .map(|l| l.replace(&format!("'{SECRET}'"), "'***'"))
            .collect();
        assert_eq!(sent, expected.join("\n"), "lignes {a}–{b}");
        covered += b - a + 1;
    }
    assert_eq!(covered, 60);
}
