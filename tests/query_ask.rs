//! `query ask` (REQ-016, AC-002-6, AC-003-4).

#[path = "support/fake_server.rs"]
#[allow(dead_code)]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use fake_server::FakeServer;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const BILLING_SRC: &str = "package com.acme.billing;\n\npublic class BillingService {\n    String SECRET_SOURCE_BODY = \"x\";\n    private Ledger ledger;\n\n    public void charge() {\n        ledger.record();\n    }\n}\n";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            mock: tmp.path().join("mock"),
            config: tmp.path().join("config.toml"),
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
        };
        fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        env.file("billing/BillingService.java", BILLING_SRC);
        env.enrichment(
            "billing/BillingService.java",
            r#"{"version":2,"summaries":[{"node":"type:com.acme.billing.BillingService","summary":"Gère la facturation des commandes"}],"concepts":[]}"#,
        );
        env.file(
            "billing/Ledger.java",
            "package com.acme.billing;\n\npublic class Ledger {\n    public void record() {}\n}\n",
        );
        env.file(
            "other/Island.java",
            "package com.acme.other;\n\npublic class Island {\n}\n",
        );
        let out = env.cmd(&["build"]).output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        env
    }

    fn file(&self, path: &str, content: &str) {
        let p = self.repo.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    }

    /// Réponse d'enrichissement (schéma version 2) du LLM simulé pour un fichier.
    fn enrichment(&self, path: &str, json: &str) {
        let m = self.mock.join(format!("{path}.json"));
        fs::create_dir_all(m.parent().unwrap()).unwrap();
        fs::write(m, json).unwrap();
    }

    fn fixture(&self, json: &str) {
        fs::write(self.mock.join("_ask.json"), json).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("rkg"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log)
            .env_remove("RKG_MOCK_INVALID_KEY")
            .env_remove("RKG_MOCK_DELAY_MS")
            .env_remove("RKG_MOCK_FAIL_AFTER");
        c
    }

    fn ask(&self, q: &str, extra: &[&str]) -> Output {
        let mut a = vec!["query", "ask", q];
        a.extend_from_slice(extra);
        fs::write(&self.log, "").unwrap();
        self.cmd(&a).output().unwrap()
    }

    fn llm_calls(&self) -> usize {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

const Q: &str = "Quel module gère la facturation ?";

fn assert_not_found(o: &Output) {
    assert!(!o.status.success());
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    let err = text(&o.stderr);
    assert!(err.starts_with("erreur:"), "{err}");
    assert!(err.contains("pas trouvé dans le graphe"), "{err}");
}

#[test]
fn req016_ac1_answer_and_cited_nodes_on_stdout() {
    let env = Env::new();
    env.fixture(
        r#"{"found":true,"answer":"Le module billing gère la facturation.","nodes":["type:com.acme.billing.BillingService","com.acme.billing.Ledger"]}"#,
    );
    let o = env.ask(Q, &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(
        out.contains("Le module billing gère la facturation."),
        "{out}"
    );
    assert!(out.contains("com.acme.billing.BillingService"), "{out}");
    assert!(out.contains("com.acme.billing.Ledger"), "{out}");
    assert_eq!(env.llm_calls(), 1);
}

#[test]
fn req016_ac1_json_output() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"billing","nodes":["BillingService"]}"#);
    let o = env.ask(Q, &["--json"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["answer"], "billing");
    let n = &v["nodes"].as_array().unwrap()[0];
    assert_eq!(n["qualified"], "com.acme.billing.BillingService");
    assert_eq!(n["kind"], "type");
    assert_eq!(n["file"], "billing/BillingService.java");
}

#[test]
fn req016_ac2_nonexistent_cited_nodes_removed() {
    let env = Env::new();
    env.fixture(
        r#"{"found":true,"answer":"reponse","nodes":["com.acme.Ghost","type:com.acme.billing.Ledger","com.acme.billing.Ledger"]}"#,
    );
    let o = env.ask(Q, &["--json"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let names: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["qualified"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["com.acme.billing.Ledger"]);
    let o = env.ask(Q, &[]);
    assert!(!text(&o.stdout).contains("Ghost"));
}

#[test]
fn req016_ac2_no_existing_cited_node_is_not_found() {
    let env = Env::new();
    for nodes in [r#"["com.acme.Ghost"]"#, "[]"] {
        env.fixture(&format!(
            r#"{{"found":true,"answer":"inventé","nodes":{nodes}}}"#
        ));
        for extra in [vec![], vec!["--json"]] {
            let o = env.ask(Q, &extra);
            assert_not_found(&o);
            assert!(!text(&o.stderr).contains("inventé"));
        }
    }
}

#[test]
fn req016_ac3_llm_not_found_signal() {
    let env = Env::new();
    env.fixture(r#"{"found":false}"#);
    assert_not_found(&env.ask(Q, &[]));
    assert_eq!(env.llm_calls(), 1);
}

#[test]
fn req016_ac3_no_matching_node_means_no_llm_call() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"inventé","nodes":["com.acme.billing.Ledger"]}"#);
    let o = env.ask("zzzabsent qqqabsent ?", &[]);
    assert_not_found(&o);
    assert_eq!(env.llm_calls(), 0);
    assert_not_found(&env.ask("?!", &["--json"]));
    assert_eq!(env.llm_calls(), 0);
}

#[test]
fn req016_stale_graph_warns_and_still_answers() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"ok","nodes":["Ledger"]}"#);
    fs::write(env.repo.join("docs.md"), "# nouveau\n").unwrap();
    let o = env.ask(Q, &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(
        text(&o.stderr).contains("graphe pas à jour"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn req016_without_graph_asks_for_build() {
    let env = Env::new();
    fs::remove_dir_all(env.repo.join(".rkg")).unwrap();
    let o = env.ask(Q, &[]);
    assert!(!o.status.success());
    assert!(o.stdout.is_empty());
    assert!(text(&o.stderr).contains("rkg build"), "{}", text(&o.stderr));
    assert_eq!(env.llm_calls(), 0);
}

#[test]
fn req016_relative_repo_option_works() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"ok","nodes":["Ledger"]}"#);
    let o = Command::new(cargo_bin("rkg"))
        .current_dir(&env.repo)
        .args(["-C", ".", "query", "ask", Q])
        .env("RKG_CONFIG", &env.config)
        .env("RKG_MOCK_DIR", &env.mock)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
}

#[test]
fn req002_ac6_ask_requires_valid_configuration() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"ok","nodes":["Ledger"]}"#);
    let run = |env: &Env| {
        fs::write(&env.log, "").unwrap();
        env.cmd(&["query", "ask", Q]).output().unwrap()
    };
    // Fichier absent.
    let missing = env.config.with_file_name("absent.toml");
    let o = env
        .cmd(&["query", "ask", Q])
        .env("RKG_CONFIG", &missing)
        .output()
        .unwrap();
    assert!(!o.status.success() && o.stdout.is_empty());
    let err = text(&o.stderr);
    assert!(
        err.contains("absent.toml") && err.contains("provider"),
        "{err}"
    );
    // Provider inconnu.
    fs::write(&env.config, "provider = \"nope\"\nmodel = \"m\"\n").unwrap();
    let o = run(&env);
    assert!(!o.status.success() && o.stdout.is_empty());
    assert!(text(&o.stderr).contains("provider"), "{}", text(&o.stderr));
    // Variable de clé absente.
    fs::write(
        &env.config,
        "provider = \"anthropic\"\nmodel = \"m\"\napi_key_env = \"RKG_TEST_ASK_NO_SUCH_KEY\"\n",
    )
    .unwrap();
    let o = env
        .cmd(&["query", "ask", Q])
        .env_remove("RKG_TEST_ASK_NO_SUCH_KEY")
        .output()
        .unwrap();
    assert!(!o.status.success() && o.stdout.is_empty());
    assert!(
        text(&o.stderr).contains("RKG_TEST_ASK_NO_SUCH_KEY"),
        "{}",
        text(&o.stderr)
    );
    // Clé refusée.
    fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let o = env
        .cmd(&["query", "ask", Q])
        .env("RKG_MOCK_INVALID_KEY", "1")
        .output()
        .unwrap();
    assert!(!o.status.success() && o.stdout.is_empty());
}

fn http_env(env: &Env, server: &FakeServer) {
    fs::write(
        &env.config,
        format!(
            "provider = \"anthropic\"\nmodel = \"claude-x\"\napi_key_env = \"RKG_TEST_ASK_KEY\"\nbase_url = \"{}\"\n",
            server.base_url
        ),
    )
    .unwrap();
}

#[test]
fn req016_ac4_request_contains_only_graph_data() {
    let env = Env::new();
    let reply = r#"{"found":true,"answer":"billing","nodes":["BillingService"]}"#;
    let server = FakeServer::start(vec![
        (200, "{}".into()),
        (
            200,
            serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string(),
        ),
    ]);
    http_env(&env, &server);
    let o = env
        .cmd(&["query", "ask", Q])
        .env("RKG_TEST_ASK_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("billing"));
    let reqs = server.requests();
    let post = reqs
        .iter()
        .find(|r| r.method == "POST")
        .expect("requête ask");
    assert_eq!(post.path, "/v1/messages");
    let body = &post.body;
    // Données du graphe : nœuds trouvés, résumé, voisins à profondeur 1.
    assert!(body.contains("com.acme.billing.BillingService"), "{body}");
    assert!(body.contains("Gère la facturation des commandes"), "{body}");
    assert!(
        body.contains("com.acme.billing.BillingService.charge"),
        "{body}"
    );
    // Profondeur 1 seulement : Ledger.record est à profondeur 2.
    assert!(!body.contains("Ledger"), "{body}");
    assert!(body.contains(Q));
    // Pas de contenu source, ni de nœud sans rapport.
    assert!(!body.contains("SECRET_SOURCE_BODY"), "{body}");
    assert!(!body.contains("Island"), "{body}");
}

#[test]
fn req016_ac4_context_is_bounded() {
    let env = Env::new();
    let many: Vec<String> = (0..2000)
        .map(|i| {
            format!(r#"{{"name":"facturation{i:04}","sections":["section:docs/big.md#big"]}}"#)
        })
        .collect();
    env.file("docs/big.md", "# big\n");
    env.enrichment(
        "docs/big.md",
        &format!(
            r#"{{"version":2,"summaries":[],"concepts":[{}]}}"#,
            many.join(",")
        ),
    );
    let o = env.cmd(&["build"]).output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let reply = r#"{"found":true,"answer":"a","nodes":["BillingService"]}"#;
    let server = FakeServer::start(vec![
        (200, "{}".into()),
        (
            200,
            serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string(),
        ),
    ]);
    http_env(&env, &server);
    let o = env
        .cmd(&["query", "ask", Q])
        .env("RKG_TEST_ASK_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let reqs = server.requests();
    let post = reqs.iter().find(|r| r.method == "POST").unwrap();
    assert!(post.body.len() < 40_000, "{}", post.body.len());
}

#[test]
fn req003_ac4_ask_uses_mock_fixture_without_fixture_is_not_found() {
    let env = Env::new();
    assert_not_found(&env.ask(Q, &[]));
    assert_eq!(env.llm_calls(), 1);
}
