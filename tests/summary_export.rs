//! Commandes `summary` et `export` (REQ-019, REQ-018, AC-001-4).

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven3");
        let repo = tmp.path().join("repo");
        copy_dir(&fx.join("repo"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("mock"), &mock);
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            mock,
            log,
        }
    }

    fn built() -> Env {
        let env = Env::new();
        env.build();
        env
    }

    fn build(&self) {
        let mut c = Command::new(cargo_bin("rkg"));
        c.arg("-C")
            .arg(&self.repo)
            .arg("build")
            .env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log);
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    /// Commande de lecture : aucune configuration LLM, aucune fixture.
    fn read(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("rkg"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("RKG_CONFIG", self.repo.join("absent.toml"))
            .env_remove("RKG_MOCK_DIR")
            .env_remove("RKG_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.read(args);
        assert!(out.status.success(), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("JSON valide sur stdout")
    }

    fn computed_at(&self) -> i64 {
        let c = rusqlite::Connection::open(self.repo.join(".rkg/graph.db")).unwrap();
        c.query_row("SELECT computed_at FROM summary WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn req019_ac1_summary_lists_modules_dependencies_nodes_and_themes() {
    let env = Env::built();
    let s = env.json(&["summary", "--json"]);
    let modules = s["modules"].as_array().unwrap();
    let dep = |name: &str| -> Vec<String> {
        let m = modules
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("module {name} absent : {s}"));
        strings(&m["depends_on"])
    };
    assert_eq!(modules.len(), 3, "{s}");
    assert_eq!(dep("api"), vec!["module:core"]);
    assert_eq!(dep("store"), vec!["module:core"]);
    assert_eq!(dep("core"), vec!["dependency:org.slf4j:slf4j-api"]);

    // Les 10 nœuds de plus haut degré, vérifiés contre l'export.
    let export = env.json(&["export"]);
    let mut degree = std::collections::HashMap::<String, u64>::new();
    for e in export["edges"].as_array().unwrap() {
        *degree
            .entry(e["source"].as_str().unwrap().into())
            .or_default() += 1;
        *degree
            .entry(e["target"].as_str().unwrap().into())
            .or_default() += 1;
    }
    let mut all: Vec<u64> = degree.values().copied().collect();
    all.sort_by(|a, b| b.cmp(a));
    let top = s["top_nodes"].as_array().unwrap();
    assert_eq!(top.len(), 10, "{s}");
    let got: Vec<u64> = top.iter().map(|n| n["degree"].as_u64().unwrap()).collect();
    assert_eq!(got, all[..10].to_vec());
    for n in top {
        assert_eq!(
            degree[n["id"].as_str().unwrap()],
            n["degree"].as_u64().unwrap()
        );
    }

    // Thèmes : concepts les plus mentionnés.
    let themes = s["themes"].as_array().unwrap();
    assert_eq!(themes[0]["concept"], "concept:idempotence", "{s}");
    assert_eq!(themes[0]["mentions"], 2);
    assert_eq!(themes[1]["concept"], "concept:persistance");
    assert_eq!(themes[1]["mentions"], 1);
}

#[test]
fn req019_ac1_summary_text_output() {
    let env = Env::built();
    let out = env.read(&["summary"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = text(&out.stdout);
    for needle in [
        "api",
        "store",
        "core",
        "slf4j-api",
        "idempotence",
        "persistance",
    ] {
        assert!(t.contains(needle), "{needle} absent de :\n{t}");
    }
}

#[test]
fn req019_ac2_build_stores_timestamped_summary_and_rebuild_refreshes_it() {
    let env = Env::built();
    let first = env.computed_at();
    assert!(first > 0);
    let j = env.json(&["summary", "--json"]);
    assert_eq!(j["computed_at"].as_i64().unwrap(), first);
    std::thread::sleep(std::time::Duration::from_millis(20));
    env.build();
    assert!(env.computed_at() > first);
}

#[test]
fn req019_ac4_summary_without_graph_invites_to_build() {
    let env = Env::new();
    let out = env.read(&["summary"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("rkg build"),
        "{}",
        text(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req018_ac1_export_contains_all_nodes_and_edges() {
    let env = Env::built();
    let v = env.json(&["export"]);
    assert_eq!(v["version"], 1);
    let nodes = v["nodes"].as_array().unwrap();
    let ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    for id in [
        "module:api",
        "module:store",
        "type:com.acme.core.OrderService",
        "method:com.acme.core.OrderService.place",
        "concept:idempotence",
        "dependency:org.slf4j:slf4j-api",
    ] {
        assert!(ids.contains(&id), "{id} absent");
    }
    let svc = nodes
        .iter()
        .find(|n| n["id"] == "type:com.acme.core.OrderService")
        .unwrap();
    assert_eq!(svc["kind"], "type");
    assert_eq!(svc["subkind"], "class");
    assert_eq!(svc["short"], "OrderService");
    assert_eq!(svc["qualified"], "com.acme.core.OrderService");
    assert!(svc["file"].as_str().unwrap().ends_with("OrderService.java"));
    let edges = v["edges"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["source"] == "module:api"
        && e["target"] == "module:core"
        && e["kind"] == "depends_on"));
    assert!(edges.iter().any(|e| e["kind"] == "mentions"));
    assert!(v["unresolved"].is_array());
}

#[test]
fn req018_ac2_output_file_has_same_content_and_stdout_is_empty() {
    let env = Env::built();
    let expected = env.json(&["export"]);
    let target = env.repo.join("out.json");
    let out = env.read(&["export", "--output", target.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    let got: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(got, expected);
}

#[test]
fn req018_ac2_unwritable_output_is_an_error() {
    let env = Env::built();
    let target = env.repo.join("absent/dir/out.json");
    let out = env.read(&["export", "--output", target.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).starts_with("erreur:"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn req018_ac3_unresolved_references_are_exported() {
    let env = Env::new();
    let f = env
        .mock
        .join("core/src/main/java/com/acme/core/OrderService.java.json");
    let s = fs::read_to_string(&f).unwrap();
    let s = s.replace(
        r#""references":["#,
        r#""references":[{"source":"method:com.acme.core.OrderService.place","kind":"calls","target":"Ghost.run"},"#,
    );
    fs::write(&f, s).unwrap();
    env.build();
    let v = env.json(&["export"]);
    let un = v["unresolved"].as_array().unwrap();
    assert_eq!(un.len(), 1, "{un:?}");
    assert_eq!(un[0]["source"], "method:com.acme.core.OrderService.place");
    assert_eq!(un[0]["kind"], "calls");
    assert_eq!(un[0]["target"], "Ghost.run");
}

#[test]
fn req018_ac4_export_without_graph_invites_to_build() {
    let env = Env::new();
    let target = env.repo.join("out.json");
    for args in [
        vec!["export"],
        vec!["export", "--output", target.to_str().unwrap()],
    ] {
        let out = env.read(&args);
        assert!(!out.status.success());
        assert!(
            text(&out.stderr).contains("rkg build"),
            "{}",
            text(&out.stderr)
        );
        assert!(out.stdout.is_empty());
    }
    assert!(!target.exists());
}

#[test]
fn req001_ac4_summary_and_export_make_no_llm_call() {
    let env = Env::built();
    let before = fs::read_to_string(&env.log).unwrap();
    assert!(!before.is_empty());
    assert!(env.read(&["summary"]).status.success());
    assert!(env.read(&["summary", "--json"]).status.success());
    assert!(env.read(&["export"]).status.success());
    // Même avec un journal actif et une configuration invalide, aucun appel n'est fait.
    let out = Command::new(cargo_bin("rkg"))
        .arg("-C")
        .arg(&env.repo)
        .arg("summary")
        .env("RKG_CONFIG", env.repo.join("absent.toml"))
        .env("RKG_MOCK_DIR", &env.mock)
        .env("RKG_MOCK_LOG", &env.log)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(&env.log).unwrap(), before);
}

#[test]
fn build_on_schema_v1_database_recreates_it() {
    let env = Env::new();
    let dir = env.repo.join(".rkg");
    fs::create_dir_all(&dir).unwrap();
    let c = rusqlite::Connection::open(dir.join("graph.db")).unwrap();
    c.execute_batch(
        "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL); PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(c);
    env.build();
    assert!(env.computed_at() > 0);
}
