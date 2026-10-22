//! Enrichissements conservés entre deux builds (REQ-049, DEC-062) : AC-049-1, 2, 5, 8 et 9.

use assert_cmd::Command;
use nephila::model::{EdgeKind, NodeKind};
use nephila::store::{Store, SCHEMA_VERSION};
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

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
            "provider = \"mock\"\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\n",
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
fn req049_ac1_second_build_without_change_sends_nothing_and_graph_is_identical() {
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
    assert_eq!(out, text(&first.stdout));
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
    assert_eq!(SCHEMA_VERSION, 5);
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
