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
        fs::write(
            &env.config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
        )
        .unwrap();
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
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env_remove("NEPHILA_MOCK_FAIL_AFTER");
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
        r#"{"found":true,"answer":"Le module billing gère la facturation.","nodes":["type:com.acme.billing.BillingService","com.acme.billing.BillingService.charge"]}"#,
    );
    let o = env.ask(Q, &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(
        out.contains("Le module billing gère la facturation."),
        "{out}"
    );
    assert!(out.contains("com.acme.billing.BillingService"), "{out}");
    assert!(
        out.contains("com.acme.billing.BillingService.charge"),
        "{out}"
    );
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
        r#"{"found":true,"answer":"reponse","nodes":["com.acme.Ghost","method:com.acme.billing.BillingService.charge","com.acme.billing.BillingService.charge"]}"#,
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
    assert_eq!(names, ["com.acme.billing.BillingService.charge"]);
    let o = env.ask(Q, &[]);
    assert!(!text(&o.stdout).contains("Ghost"));
}

const A9: &str = "avertissement: réponse non vérifiable : aucun nœud cité ne figure dans le contexte envoyé ; vérifiez-la avec nephila query search\n";

/// AC-161-4 : un nœud du graphe absent du contexte de la question n'est pas retenu.
#[test]
fn req161_ac4_cited_node_outside_the_context_is_dropped() {
    for c in [CONTEXTE_0_18_0, CONTEXTE_COMPACT] {
        assert!(!c.contains("Island"));
        assert!(!c.contains("Ledger"));
    }
    let env = Env::new();
    env.fixture(
        r#"{"found":true,"answer":"reponse","nodes":["com.acme.other.Island","com.acme.billing.Ledger","method:com.acme.billing.BillingService.charge"]}"#,
    );
    // Le nœud existe bien dans le graphe.
    let s = env.cmd(&["query", "search", "Island"]).output().unwrap();
    assert!(text(&s.stdout).contains("com.acme.other.Island"));
    let s = env.cmd(&["query", "search", "Ledger"]).output().unwrap();
    assert!(text(&s.stdout).contains("com.acme.billing.Ledger"));
    let o = env.ask(Q, &["--json"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let names: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["qualified"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["com.acme.billing.BillingService.charge"]);
    assert!(!names.contains(&"com.acme.other.Island"));
    assert!(text(&o.stderr).is_empty());
    assert_eq!(o.status.code(), Some(0));
}

#[test]
fn req161_ac5_answer_without_valid_citation_is_shown() {
    let env = Env::new();
    for nodes in [
        r#"["com.acme.Ghost"]"#,
        "[]",
        "",
        r#"["com.acme.billing.Ledger"]"#,
    ] {
        let nodes_field = if nodes.is_empty() {
            String::new()
        } else {
            format!(r#","nodes":{nodes}"#)
        };
        env.fixture(&format!(
            r#"{{"found":true,"answer":"inventé"{nodes_field}}}"#
        ));
        let o = env.ask(Q, &[]);
        assert_eq!(text(&o.stdout), "inventé\n", "{nodes}");
        assert_eq!(text(&o.stderr), A9, "{nodes}");
        assert_eq!(o.status.code(), Some(0), "{nodes}");
        assert_eq!(env.llm_calls(), 1, "{nodes}");
        let o = env.ask(Q, &["--json"]);
        assert_eq!(
            text(&o.stdout),
            "{\"answer\":\"inventé\",\"nodes\":[]}\n",
            "{nodes}"
        );
        assert_eq!(text(&o.stderr), A9, "{nodes}");
        assert_eq!(o.status.code(), Some(0), "{nodes}");
        assert_eq!(env.llm_calls(), 1, "{nodes}");
    }
    // Ordre de stderr avec --check-stale : fraîcheur, puis A-9 ; stdout inchangé.
    env.fixture(r#"{"found":true,"answer":"inventé","nodes":[]}"#);
    let plain = env.ask(Q, &[]).stdout;
    let plain_json = env.ask(Q, &["--json"]).stdout;
    fs::write(env.repo.join("docs.md"), "# nouveau\n").unwrap();
    for (extra, expected) in [
        (vec!["--check-stale"], plain),
        (vec!["--check-stale", "--json"], plain_json),
    ] {
        let o = env.ask(Q, &extra);
        assert_eq!(
            text(&o.stderr),
            format!("avertissement: graphe pas à jour : 1 fichier modifié\n{A9}")
        );
        assert_eq!(o.stdout, expected);
        assert_eq!(o.status.code(), Some(0));
    }
    // Pas d'A-9 après `found: false`, ni sans nœud de départ.
    env.fixture(r#"{"found":false}"#);
    let o = env.ask(Q, &[]);
    assert_not_found(&o);
    assert_eq!(text(&o.stderr), "erreur: pas trouvé dans le graphe\n");
    assert_eq!(o.status.code(), Some(1));
    env.fixture(r#"{"found":true,"answer":"inventé","nodes":[]}"#);
    let o = env.ask("?!", &[]);
    assert_not_found(&o);
    assert_eq!(text(&o.stderr), "erreur: pas trouvé dans le graphe\n");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(env.llm_calls(), 0);
    // Une citation retenue : pas d'A-9, ligne `Nœuds cités :` présente.
    env.fixture(
        r#"{"found":true,"answer":"ok","nodes":["com.acme.billing.BillingService.charge"]}"#,
    );
    let o = env.ask(Q, &[]);
    assert!(text(&o.stderr).is_empty(), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("Nœuds cités :\n"));
}

#[test]
fn req161_ac10_answer_without_citation_is_escaped() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"ligne 1\nligne\u001b2","nodes":[]}"#);
    let o = env.ask(Q, &[]);
    assert_eq!(text(&o.stdout), "ligne 1\nligne\\u{001B}2\n");
    assert_eq!(text(&o.stderr), A9);
    let o = env.ask(Q, &["--json"]);
    assert_eq!(
        text(&o.stdout),
        "{\"answer\":\"ligne 1\\nligne\\u001b2\",\"nodes\":[]}\n"
    );
    assert!(!o.stdout.contains(&0x1b) && !o.stderr.contains(&0x1b));
}

#[test]
fn req016_ac3_llm_not_found_signal() {
    let env = Env::new();
    env.fixture(r#"{"found":false}"#);
    assert_not_found(&env.ask(Q, &[]));
    assert_eq!(env.llm_calls(), 1);
}

/// AC-260-7 (remplace l'ancien test sans appel) : sans nœud de départ, le résumé global part au LLM.
#[test]
fn req260_ac7_no_matching_node_asks_with_the_summary() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"vue d'ensemble","nodes":["com.acme.billing.Ledger"]}"#);
    let o = env.ask("zzzabsent qqqabsent ?", &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).lines().next(), Some("vue d'ensemble"));
    assert_eq!(env.llm_calls(), 1);
    env.fixture(r#"{"found":false}"#);
    let o = env.ask("zzzabsent qqqabsent ?", &[]);
    assert_not_found(&o);
    assert_eq!(env.llm_calls(), 1);
    env.fixture(r#"{"found":true,"answer":"inventé","nodes":["com.acme.billing.Ledger"]}"#);
    assert_not_found(&env.ask("?!", &["--json"]));
    assert_eq!(env.llm_calls(), 0);
    assert_not_found(&env.ask("?!", &[]));
    assert_eq!(env.llm_calls(), 0);
}

/// AC-260-7 (corps de la requête) et AC-016-4 : le bloc part, pas le contenu des fichiers.
#[test]
fn req260_ac7_request_body_has_the_summary_block() {
    let env = Env::new();
    let reply = r#"{"found":true,"answer":"vue","nodes":[]}"#;
    let server = FakeServer::start(vec![(
        200,
        serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string(),
    )]);
    http_env(&env, &server);
    let o = env
        .cmd(&["query", "ask", "zzzabsent qqqabsent ?"])
        .env("NEPHILA_TEST_ASK_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let reqs = server.requests();
    let post = reqs.iter().find(|r| r.method == "POST").unwrap();
    assert!(post.body.contains("Résumé du graphe :"), "{}", post.body);
    assert!(!post.body.contains("SECRET_SOURCE_BODY"), "{}", post.body);
}

/// AC-016-7 : une erreur d'appel n'est pas transformée en « pas trouvé ».
#[test]
fn req016_ac7_call_error_is_not_not_found() {
    let env = Env::new();
    let server = FakeServer::start(vec![(401, "{}".to_string())]);
    http_env(&env, &server);
    let o = env
        .cmd(&["query", "ask", "zzzabsent qqqabsent ?"])
        .env("NEPHILA_TEST_ASK_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    assert_eq!(
        text(&o.stderr),
        "erreur: clé d'API refusée par anthropic (HTTP 401) ; vérifiez la variable désignée par api_key_env\n"
    );
    assert!(!text(&o.stderr).contains("pas trouvé dans le graphe"));
    assert_eq!(o.status.code(), Some(1));
    let posts = server
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(posts, 1);
}

/// AC-260-10 : citations du bloc, sortie sans le bloc.
#[test]
fn req260_ac10_cited_summary_nodes_are_shown() {
    let env = Env::new();
    env.fixture(
        r#"{"found":true,"answer":"la voici","nodes":["type:com.acme.billing.BillingService"]}"#,
    );
    let o = env.ask("zzzabsent qqqabsent ?", &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.starts_with("la voici\nNœuds cités :\n- "), "{out}");
    assert_eq!(out.lines().count(), 3, "{out}");
    assert!(text(&o.stderr).is_empty(), "{}", text(&o.stderr));
    assert!(!out.contains("Résumé du graphe"));
    let o = env.ask("zzzabsent qqqabsent ?", &["--json"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["nodes"].as_array().unwrap().len(), 1);
    // Libellé réel de communauté (celui d'un nœud existant) : aucune citation retenue, A-9.
    let sum = env.cmd(&["summary", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&sum.stdout).unwrap();
    let labels: Vec<&str> = v["communities"]["top"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"charge"), "{labels:?}");
    env.fixture(r#"{"found":true,"answer":"la voici","nodes":["charge"]}"#);
    let o = env.ask("zzzabsent qqqabsent ?", &[]);
    assert_eq!(text(&o.stdout), "la voici\n");
    assert_eq!(text(&o.stderr), A9);
    assert_eq!(o.status.code(), Some(0));
    let o = env.ask("zzzabsent qqqabsent ?", &["--json"]);
    assert_eq!(text(&o.stdout), "{\"answer\":\"la voici\",\"nodes\":[]}\n");
}

/// AC-260-11 : traces.
#[test]
fn req260_ac11_verbose_traces() {
    let env = Env::new();
    ask_ok_fixture(&env);
    let o = env.ask("zzzabsent qqqabsent ?", &["--verbose"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert_eq!(err.lines().count(), 2, "{err}");
    assert_eq!(traces(&o), [CFG_DEFAULT, CALLS_UNKNOWN]);
    let o = env.ask("?!", &["--verbose"]);
    let err = text(&o.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 2, "{err}");
    assert!(lines[0].ends_with(CFG_DEFAULT), "{err}");
    assert_eq!(lines[1], "erreur: pas trouvé dans le graphe");
    assert!(!err.contains("appels:"));
}

#[test]
fn req016_stale_graph_warns_and_still_answers() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"ok","nodes":["Ledger"]}"#);
    fs::write(env.repo.join("docs.md"), "# nouveau\n").unwrap();
    let o = env.ask(Q, &["--check-stale"]);
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
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    let o = env.ask(Q, &[]);
    assert!(!o.status.success());
    assert!(o.stdout.is_empty());
    assert!(
        text(&o.stderr).contains("nephila build"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(env.llm_calls(), 0);
}

#[test]
fn req016_relative_repo_option_works() {
    let env = Env::new();
    env.fixture(r#"{"found":true,"answer":"ok","nodes":["Ledger"]}"#);
    let o = Command::new(cargo_bin("nephila"))
        .current_dir(&env.repo)
        .args(["-C", ".", "query", "ask", Q])
        .env("NEPHILA_CONFIG", &env.config)
        .env("NEPHILA_MOCK_DIR", &env.mock)
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
        .env("NEPHILA_CONFIG", &missing)
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
        "provider = \"anthropic\"\nenrich_code = true\nmodel = \"m\"\napi_key_env = \"NEPHILA_TEST_ASK_NO_SUCH_KEY\"\n",
    )
    .unwrap();
    let o = env
        .cmd(&["query", "ask", Q])
        .env_remove("NEPHILA_TEST_ASK_NO_SUCH_KEY")
        .output()
        .unwrap();
    assert!(!o.status.success() && o.stdout.is_empty());
    assert!(
        text(&o.stderr).contains("NEPHILA_TEST_ASK_NO_SUCH_KEY"),
        "{}",
        text(&o.stderr)
    );
    // Clé refusée.
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
    )
    .unwrap();
    let o = env
        .cmd(&["query", "ask", Q])
        .env("NEPHILA_MOCK_INVALID_KEY", "1")
        .output()
        .unwrap();
    assert!(!o.status.success() && o.stdout.is_empty());
}

fn http_env(env: &Env, server: &FakeServer) {
    fs::write(
        &env.config,
        format!(
            "provider = \"anthropic\"\nenrich_code = true\nmodel = \"claude-x\"\napi_key_env = \"NEPHILA_TEST_ASK_KEY\"\nbase_url = \"{}\"\n",
            server.base_url
        ),
    )
    .unwrap();
}

#[test]
fn req016_ac4_request_contains_only_graph_data() {
    let env = Env::new();
    let reply = r#"{"found":true,"answer":"billing","nodes":["BillingService"]}"#;
    let server = FakeServer::start(vec![(
        200,
        serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string(),
    )]);
    http_env(&env, &server);
    let o = env
        .cmd(&["query", "ask", Q])
        .env("NEPHILA_TEST_ASK_KEY", "sk-test")
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
    let server = FakeServer::start(vec![(
        200,
        serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string(),
    )]);
    http_env(&env, &server);
    let o = env
        .cmd(&["query", "ask", Q])
        .env("NEPHILA_TEST_ASK_KEY", "sk-test")
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

// ---- REQ-038 : nouveaux essais de `query ask` ----

const ASK_OK: &str = r#"{"found":true,"answer":"Le module billing gère la facturation.","nodes":["type:com.acme.billing.BillingService"]}"#;

impl Env {
    fn ask_malformed(&self, k: u32, extra: &[&str]) -> Output {
        let mut a = vec!["query", "ask", Q];
        a.extend_from_slice(extra);
        fs::write(&self.log, "").unwrap();
        self.cmd(&a)
            .env("NEPHILA_MOCK_MALFORMED", format!("_ask:{k}"))
            .output()
            .unwrap()
    }
}

#[test]
fn req038_ac11_ask_retried_until_conforming() {
    let env = Env::new();
    env.fixture(ASK_OK);
    let o = env.ask_malformed(2, &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("Le module billing gère la facturation."));
    assert!(text(&o.stderr).is_empty(), "{}", text(&o.stderr));
    assert_eq!(env.llm_calls(), 3);
    // REQ-016 AC-5 : même sortie que sans réponse mal formée.
    let clean = env.ask(Q, &[]);
    assert!(clean.status.success(), "{}", text(&clean.stderr));
    assert_eq!(text(&o.stdout), text(&clean.stdout));

    let o = env.ask_malformed(2, &["--json"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["answer"], "Le module billing gère la facturation.");
    assert!(text(&o.stderr).is_empty());
    let clean = env.ask(Q, &["--json"]);
    assert!(clean.status.success(), "{}", text(&clean.stderr));
    assert_eq!(text(&o.stdout), text(&clean.stdout));
}

#[test]
fn req038_ac11_ask_still_malformed_after_last_try() {
    let env = Env::new();
    env.fixture(ASK_OK);
    let o = env.ask_malformed(3, &[]);
    assert!(!o.status.success());
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    let err = text(&o.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(
        err.starts_with("erreur: réponse LLM non conforme")
            && err.trim_end().ends_with(" (3 essais sur 3)"),
        "{err}"
    );
    assert_eq!(env.llm_calls(), 3);

    let o = env.ask_malformed(3, &["--format-retries", "3"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("Le module billing gère la facturation."));
    assert!(text(&o.stderr).is_empty(), "{}", text(&o.stderr));
    assert_eq!(env.llm_calls(), 4);

    // format_retries = 0 : message de la 0.12.0, sans suffixe.
    let o = env.ask_malformed(1, &["--format-retries", "0"]);
    assert!(!o.status.success());
    assert!(
        !text(&o.stderr).contains("essais sur"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(env.llm_calls(), 1);

    let o = env.ask_malformed(3, &["--format-retries", "1"]);
    assert!(!o.status.success());
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    assert!(text(&o.stderr).trim_end().ends_with(" (2 essais sur 2)"));
    assert_eq!(env.llm_calls(), 2);
}

#[test]
fn req016_ac5_ac6_ask_option_priority_over_config() {
    let env = Env::new();
    env.fixture(ASK_OK);
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nformat_retries = 0\n",
    )
    .unwrap();
    let o = env.ask_malformed(1, &[]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("erreur: "), "{err}");
    assert!(!err.contains("essais sur"), "{err}");
    assert_eq!(env.llm_calls(), 1);
    let o = env.ask_malformed(2, &["--format-retries", "3"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
}

/// Contexte de la requête `ask` de la question d'AC-016-1, tel que la 0.18.0 l'écrivait.
const CONTEXTE_0_18_0: &str = "Nœuds :\n- type:com.acme.billing.BillingService : com.acme.billing.BillingService (class, billing/BillingService.java) - Gère la facturation des commandes\n- package:com.acme.billing : com.acme.billing (package, billing/BillingService.java)\n- method:com.acme.billing.BillingService.charge : com.acme.billing.BillingService.charge (method, billing/BillingService.java)\nLiens :\n- package:com.acme.billing -contains-> type:com.acme.billing.BillingService\n- type:com.acme.billing.BillingService -contains-> method:com.acme.billing.BillingService.charge\n";

/// Contexte compact de la même question (REQ-181, DEC-145) : mêmes nœuds et mêmes liens,
/// lignes de nœuds sans nom qualifié ni genre répétés.
const CONTEXTE_COMPACT: &str = "Nœuds :\n- type:com.acme.billing.BillingService (class, billing/BillingService.java) - Gère la facturation des commandes\n- package:com.acme.billing (billing/BillingService.java)\n- method:com.acme.billing.BillingService.charge (billing/BillingService.java)\nLiens :\n- package:com.acme.billing -contains-> type:com.acme.billing.BillingService\n- type:com.acme.billing.BillingService -contains-> method:com.acme.billing.BillingService.charge\n";

/// Identifiants des lignes de nœuds : texte après `- ` jusqu'au séparateur donné.
fn node_ids<'a>(ctx: &'a str, sep: &str) -> Vec<&'a str> {
    let body = ctx.strip_prefix("Nœuds :\n").unwrap();
    let (nodes, _) = body.split_once("Liens :\n").unwrap();
    nodes
        .lines()
        .map(|l| l.strip_prefix("- ").unwrap().split(sep).next().unwrap())
        .collect()
}

/// AC-181-5 : le corps de la requête `ask` de la question d'AC-016-1 est identique octet pour
/// octet à `tests/fixtures/ask_body_ac016_1.json`, et son contexte est le contexte compact.
#[test]
fn req181_ac5_ask_request_body_compact_context() {
    let env = Env::new();
    let reply = r#"{"found":true,"answer":"billing","nodes":["BillingService"]}"#;
    let server = FakeServer::start(vec![(
        200,
        serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string(),
    )]);
    http_env(&env, &server);
    let mut cfg = fs::read_to_string(&env.config).unwrap();
    cfg.push_str("max_output_tokens = 8192\neffort = \"default\"\n");
    fs::write(&env.config, cfg).unwrap();
    let o = env
        .cmd(&["query", "ask", Q])
        .env("NEPHILA_TEST_ASK_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let reqs = server.requests();
    let post = reqs
        .iter()
        .find(|r| r.method == "POST")
        .expect("requête ask");
    assert_eq!(
        post.body,
        include_str!("fixtures/ask_body_ac016_1.json").trim_end_matches('\n')
    );
    let v: Value = serde_json::from_str(&post.body).unwrap();
    assert_eq!(
        v["messages"][0]["content"].as_str().unwrap(),
        format!("Contexte :\n{CONTEXTE_COMPACT}\nQuestion : {Q}")
    );
    // Même sélection qu'en 0.18.0, forme seule changée.
    assert_eq!(CONTEXTE_COMPACT.chars().count(), 436);
    let links = |c: &'static str| c.split_once("Liens :\n").unwrap().1;
    assert_eq!(links(CONTEXTE_COMPACT), links(CONTEXTE_0_18_0));
    assert_eq!(
        node_ids(CONTEXTE_COMPACT, " "),
        node_ids(CONTEXTE_0_18_0, " : ")
    );
}

/// Texte des messages de la requête `ask` reçue (JSON décodé : les `\n` sont de vrais retours).
fn ask_user_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap();
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Dépôt Java sur le modèle de javapoet : le contexte de la question dépasse 12 000 caractères
/// avant le lien `JavaFile.writeTo -calls-> CodeWriter.suggestedImports` avant REQ-142.
fn javapoet_like(env: &Env) {
    let pad = "WithALongDescriptiveNameSoThatTheGraphContextGrowsQuickly";
    let mut cw = String::from("package com.squareup.javapoet;\n\nclass CodeWriter {\n");
    for i in 0..35 {
        cw.push_str(&format!(
            "    void emit{pad}{i}() {{ emit{pad}{}(); }}\n",
            (i + 1) % 35
        ));
    }
    cw.push_str("    void suggestedImports() {}\n}\n");
    env.file("com/squareup/javapoet/CodeWriter.java", &cw);
    let mut ms = String::from("package com.squareup.javapoet;\n\nclass MethodSpec {\n");
    for i in 0..30 {
        ms.push_str(&format!("    void method{pad}{i}() {{}}\n"));
    }
    ms.push_str("}\n");
    env.file("com/squareup/javapoet/MethodSpec.java", &ms);
    env.file(
        "com/squareup/javapoet/JavaFile.java",
        "package com.squareup.javapoet;\n\nclass JavaFile {\n    void writeTo() {\n        CodeWriter w = new CodeWriter();\n        w.suggestedImports();\n    }\n}\n",
    );
    let o = env.cmd(&["build", "--no-llm"]).output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
}

#[test]
fn req142_ac6_late_link_reaches_the_context_for_the_three_questions() {
    const LINK: &str = "- method:com.squareup.javapoet.JavaFile.writeTo -calls-> method:com.squareup.javapoet.CodeWriter.suggestedImports";
    let env = Env::new();
    javapoet_like(&env);
    // Le graphe contient bien le lien : le rouge ne vient pas de la fixture.
    let o = env
        .cmd(&["query", "neighbors", "CodeWriter.suggestedImports"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(
        text(&o.stdout).contains("calls com.squareup.javapoet.JavaFile.writeTo"),
        "{}",
        text(&o.stdout)
    );
    let mut missing = Vec::new();
    for q in [
        "Which method calls CodeWriter.suggestedImports()?",
        "Which calls CodeWriter.suggestedImports()?",
        "Who calls suggestedImports in CodeWriter?",
    ] {
        let reply = r#"{"found":true,"answer":"a","nodes":["JavaFile.writeTo"]}"#;
        let server = FakeServer::start(vec![(
            200,
            serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string(),
        )]);
        http_env(&env, &server);
        let o = env
            .cmd(&["query", "ask", q])
            .env("NEPHILA_TEST_ASK_KEY", "sk-test")
            .output()
            .unwrap();
        assert!(o.status.success(), "{q} : {}", text(&o.stderr));
        let reqs = server.requests();
        let post = reqs.iter().find(|r| r.method == "POST").unwrap();
        let msg = ask_user_message(&post.body);
        if !msg.lines().any(|l| l == LINK) {
            missing.push(format!("{q} ({} caractères)", msg.chars().count()));
        }
    }
    assert!(missing.is_empty(), "lien absent pour : {missing:?}");
}

// ---- REQ-162, REQ-163 : ligne `config` d'`ask`, `--context-chars` ----

const MOCK_CFG: &str = "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n";
const CFG_DEFAULT: &str = "config: llm=mock model=mock thinking=false effort=low max_output_tokens=16384 max_context_chars=12000 max_seeds=20 max_nodes=60 timeout_secs=60 format_retries=2";

const CALLS_UNKNOWN: &str =
    "appels: 1 appels, 0 nouveaux essais de format, 1 envois HTTP, jetons : inconnus";

/// Lignes `trace:` de stderr, horodatage retiré.
fn traces(o: &Output) -> Vec<String> {
    text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("trace:"))
        .map(|l| l[l.find("] ").unwrap() + 2..].to_string())
        .collect()
}

fn ask_ok_fixture(env: &Env) {
    env.fixture(ASK_OK);
}

#[test]
fn req162_ac10_ac13_config_line_template_marks_and_stdout() {
    let env = Env::new();
    ask_ok_fixture(&env);
    let plain = env.ask(Q, &[]);
    assert!(traces(&plain).is_empty() && text(&plain.stderr).is_empty());
    // (a) gabarit exact, stdout identique, `-v` équivaut à `--verbose`.
    for flag in ["--verbose", "-v"] {
        let o = env.ask(Q, &[flag]);
        assert_eq!(traces(&o), [CFG_DEFAULT, CALLS_UNKNOWN], "{flag}");
        assert_eq!(text(&o.stderr).lines().count(), 2);
        assert_eq!(o.stdout, plain.stdout);
        assert_eq!(o.status.code(), Some(0));
    }
    let json = env.ask(Q, &["--json"]);
    let o = env.ask(Q, &["--json", "--verbose"]);
    assert_eq!(o.stdout, json.stdout);
    assert_eq!(traces(&o), [CFG_DEFAULT, CALLS_UNKNOWN]);
    for bad in ["enrich_code=", "concurrency=", "batch_files="] {
        assert!(!CFG_DEFAULT.contains(bad));
    }
    // AC-162-10 : `[ask] model` du fichier de projet, puis `model` du premier niveau.
    let proj = env.repo.join(".nephila.toml");
    fs::write(&proj, "[ask]\nmodel = \"m-ask-projet\"\n").unwrap();
    let o = env.ask(Q, &["-v"]);
    let l = &traces(&o)[0];
    assert!(l.contains(" model=m-ask-projet "), "{l}");
    assert!(l.ends_with(" (.nephila.toml : ask.model)"), "{l}");
    fs::write(&proj, "model = \"m-projet\"\n").unwrap();
    let l = traces(&env.ask(Q, &["-v"]))[0].clone();
    assert!(l.contains(" model=m-projet "), "{l}");
    assert!(l.ends_with(" (.nephila.toml : model)"), "{l}");
    // --format-retries retire format_retries de la marque.
    fs::write(&proj, "format_retries = 3\n").unwrap();
    let l = traces(&env.ask(Q, &["-v"]))[0].clone();
    assert!(l.contains(" format_retries=3"), "{l}");
    assert!(l.ends_with("(.nephila.toml : format_retries)"), "{l}");
    let l = traces(&env.ask(Q, &["-v", "--format-retries", "1"]))[0].clone();
    assert!(l.contains(" format_retries=1"), "{l}");
    assert!(!l.contains(".nephila.toml"), "{l}");
    // (b) `[ask] model` personnel l'emporte sur `model` de .nephila.toml.
    fs::write(&proj, "model = \"m-projet\"\n").unwrap();
    fs::write(&env.config, format!("{MOCK_CFG}[ask]\nmodel = \"m-ask\"\n")).unwrap();
    let l = traces(&env.ask(Q, &["-v"]))[0].clone();
    assert!(l.contains(" model=m-ask "), "{l}");
    assert!(!l.contains(" (.nephila.toml :"), "{l}");
    // (c) mélange, option `--context-chars`.
    fs::write(&env.config, MOCK_CFG).unwrap();
    fs::write(
        &proj,
        "timeout_secs = 600\n[ask]\nmodel = \"m-ask-projet\"\nmax_context_chars = 3000\nmax_nodes = 30\n",
    )
    .unwrap();
    let l = traces(&env.ask(Q, &["-v", "--context-chars", "2000"]))[0].clone();
    for want in [
        " model=m-ask-projet ",
        " max_context_chars=2000 ",
        " max_nodes=30 ",
        " timeout_secs=600 ",
    ] {
        assert!(l.contains(want), "{want} : {l}");
    }
    assert!(
        l.ends_with(" (.nephila.toml : ask.model,ask.max_nodes,timeout_secs)"),
        "{l}"
    );
    // (d) thinking et effort.
    fs::write(&proj, "[ask]\nthinking = true\neffort = \"high\"\n").unwrap();
    let l = traces(&env.ask(Q, &["-v"]))[0].clone();
    assert!(l.contains(" model=mock thinking=true effort=high "), "{l}");
    assert!(
        l.ends_with(" (.nephila.toml : ask.thinking,ask.effort)"),
        "{l}"
    );
}

#[test]
fn req162_ac14_config_line_place_in_stderr() {
    let env = Env::new();
    ask_ok_fixture(&env);
    // (a) --check-stale : fraîcheur, puis config.
    fs::write(env.repo.join("docs.md"), "# nouveau\n").unwrap();
    let o = env.ask(Q, &["--verbose", "--check-stale"]);
    let err = text(&o.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 3, "{err}");
    assert_eq!(
        lines[0],
        "avertissement: graphe pas à jour : 1 fichier modifié"
    );
    assert!(
        lines[1].starts_with("trace: [") && lines[1].ends_with(CFG_DEFAULT),
        "{err}"
    );
    assert!(lines[2].ends_with(CALLS_UNKNOWN), "{err}");
    let o = env.ask(Q, &["--check-stale"]);
    assert!(!text(&o.stderr).contains("trace:"));
    // (b) aucun nœud de départ : config puis erreur.
    let o = env.ask("?!", &["--verbose"]);
    let err = text(&o.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 2, "{err}");
    assert!(lines[0].ends_with(CFG_DEFAULT), "{err}");
    assert_eq!(lines[1], "erreur: pas trouvé dans le graphe");
    assert_eq!(o.status.code(), Some(1));
    // (c) sans citation valide : config puis A-9.
    env.fixture(r#"{"found":true,"answer":"inventé","nodes":[]}"#);
    let o = env.ask(Q, &["--verbose"]);
    let err = text(&o.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 3, "{err}");
    assert!(lines[0].ends_with(CFG_DEFAULT), "{err}");
    assert!(lines[1].ends_with(CALLS_UNKNOWN), "{err}");
    assert_eq!(format!("{}\n", lines[2]), A9);
    assert_eq!(o.status.code(), Some(0));
    let plain = env.ask(Q, &[]);
    assert_eq!(text(&plain.stderr), A9);
    // AC-166-7 : stdout identique, texte et `--json`.
    assert_eq!(o.stdout, plain.stdout);
    let j_plain = env.ask(Q, &["--json"]);
    let j = env.ask(Q, &["--json", "--verbose"]);
    assert_eq!(j.stdout, j_plain.stdout);
    // (d) erreur de configuration : seule la ligne E-15, aucune trace.
    fs::write(&env.config, format!("{MOCK_CFG}[ask]\neffort = \"max\"\n")).unwrap();
    let o = env.ask(Q, &["--verbose"]);
    let err = text(&o.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("erreur: configuration invalide"), "{err}");
    assert!(o.stdout.is_empty());
    assert_eq!(env.llm_calls(), 0);
}

fn openai_reply(t: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":t}}]}).to_string()
}

/// Dépôt au contexte volumineux (2000 concepts), configuration `openai` sur le faux serveur ;
/// rend le contexte du message utilisateur de chaque `query ask`.
fn context_sizes(personal_ask: &str, project: Option<&str>, runs: &[&[&str]]) -> Vec<usize> {
    let env = Env::new();
    javapoet_like(&env);
    let reply = openai_reply(r#"{"found":true,"answer":"a","nodes":["BillingService"]}"#);
    let server = FakeServer::start(runs.iter().map(|_| (200, reply.clone())).collect());
    fs::write(
        &env.config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\napi_key_env = \"NEPHILA_TEST_ASK_KEY\"\nbase_url = \"{}\"\n{personal_ask}",
            server.base_url
        ),
    )
    .unwrap();
    if let Some(p) = project {
        fs::write(env.repo.join(".nephila.toml"), p).unwrap();
    }
    let mut sizes = Vec::new();
    for extra in runs {
        let mut args = vec!["query", "ask", "Who calls suggestedImports in CodeWriter?"];
        args.extend_from_slice(extra);
        let o = env
            .cmd(&args)
            .env("NEPHILA_TEST_ASK_KEY", "sk-test")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", text(&o.stderr));
        let reqs = server.requests();
        let msg = ask_user_message(&reqs[sizes.len()].body);
        let from = msg.find("Contexte :\n").unwrap() + "Contexte :\n".len();
        let to = msg.rfind("\nQuestion : ").unwrap();
        sizes.push(msg[from..to].chars().count());
    }
    sizes
}

#[test]
fn req163_ac4_context_chars_option_and_ask_keys_bound_the_context() {
    let none: &[&str] = &[];
    let sizes = context_sizes("", None, &[none, &["--context-chars", "3000"]]);
    assert!(sizes[0] > 3000, "{sizes:?}");
    assert!(sizes[1] <= 3000 && sizes[1] > 1500, "{sizes:?}");
    let personal = "[ask]\nmax_context_chars = 3000\n";
    let sizes = context_sizes(personal, None, &[none, &["--context-chars", "20000"]]);
    assert!(sizes[0] <= 3000, "{sizes:?}");
    assert!(sizes[1] > 3000, "{sizes:?}");
    let sizes = context_sizes(
        "[ask]\nmax_context_chars = 5000\n",
        Some("[ask]\nmax_context_chars = 2000\n"),
        &[none],
    );
    assert!(sizes[0] <= 2000 && sizes[0] > 1000, "{sizes:?}");
}

#[test]
fn req195_ac8_trace_du_modele_du_depot_ignore_dans_ask() {
    let env = Env::new();
    ask_ok_fixture(&env);
    let proj = env.repo.join(".nephila.toml");
    let deny = format!("{MOCK_CFG}model_override = \"deny\"\n");
    fs::write(&env.config, &deny).unwrap();
    // [ask].model du dépôt prime sur model ; la trace suit la ligne config, avant appels.
    fs::write(
        &proj,
        "model = \"m-projet\"\n[ask]\nmodel = \"m-ask-projet\"\n",
    )
    .unwrap();
    let t = traces(&env.ask(Q, &["-v"]));
    assert_eq!(t.len(), 3, "{t:?}");
    assert_eq!(t[0], CFG_DEFAULT);
    assert_eq!(
        t[1],
        "modèle: m-ask-projet de .nephila.toml ignoré (model_override = \"deny\")"
    );
    assert_eq!(t[2], CALLS_UNKNOWN);
    // Sans [ask].model : model du premier niveau.
    fs::write(&proj, "model = \"m-projet\"\n").unwrap();
    let t = traces(&env.ask(Q, &["-v"]));
    assert_eq!(
        t[1],
        "modèle: m-projet de .nephila.toml ignoré (model_override = \"deny\")"
    );
    // Sans -v : stderr vide ; sans modèle du dépôt : aucune trace.
    let o = env.ask(Q, &[]);
    assert!(text(&o.stderr).is_empty(), "{}", text(&o.stderr));
    fs::write(&proj, "").unwrap();
    assert_eq!(traces(&env.ask(Q, &["-v"])), [CFG_DEFAULT, CALLS_UNKNOWN]);
    // allow et warn : aucune trace.
    fs::write(&proj, "model = \"m-projet\"\n").unwrap();
    for p in ["allow", "warn"] {
        fs::write(&env.config, format!("{MOCK_CFG}model_override = \"{p}\"\n")).unwrap();
        let t = traces(&env.ask(Q, &["-v"]));
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(!t.iter().any(|l| l.contains("ignoré")));
    }
}
