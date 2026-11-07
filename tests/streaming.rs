//! Réponses LLM demandées et reçues en flux (REQ-249, T-339) : corps envoyé, assemblage d'un
//! flux `anthropic` et `openai` (jetons identiques à la réponse JSON), mode lot sans `stream`.
#![allow(non_snake_case)]

#[allow(dead_code)]
#[path = "support/batch_server.rs"]
mod batch_server;
#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use batch_server::{BatchServer, Outcome};
use fake_server::{FakeServer, Reply};
use serde_json::{json, Value};

const KEY_VAR: &str = "NEPHILA_T339_KEY";
const SUMMARY: &str = "Résumé du document";
const ASK_REPLY: &str = r#"{"found":true,"answer":"réponse-ask","nodes":["Alpha"]}"#;
const PROVIDERS: [&str; 2] = ["anthropic", "openai"];

fn enrichment() -> String {
    json!({
        "version": 2,
        "summaries": [{"node": "document:docs/d.md", "summary": SUMMARY}],
        "concepts": []
    })
    .to_string()
}

fn sse(provider: &str, text: &str) -> Reply {
    let chunks = if provider == "anthropic" {
        support::anthropic_sse(text, 4)
    } else {
        support::openai_sse(text, 4)
    };
    Reply::Sse { chunks, cut: false }
}

/// La même réponse, servie en JSON, avec les mêmes jetons (10 entrées, 7 sorties).
fn json_reply(provider: &str, text: &str) -> Reply {
    let body = if provider == "anthropic" {
        json!({"content":[{"type":"text","text":text}],"stop_reason":"end_turn",
            "usage":{"input_tokens":10,"output_tokens":7}})
    } else {
        json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":7}})
    };
    Reply::status(200, &body.to_string())
}

fn path_of(provider: &str) -> &'static str {
    if provider == "anthropic" {
        "/v1/messages"
    } else {
        "/v1/chat/completions"
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
    provider: &'static str,
}

impl Fx {
    fn new(provider: &'static str, replies: Vec<Reply>) -> Fx {
        let server = FakeServer::start_replies(replies);
        Fx::with_url(provider, server)
    }

    fn with_url(provider: &'static str, server: FakeServer) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/d.md"), "# D\n\nTexte du document.\n").unwrap();
        fs::write(
            repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\n",
                server.base_url
            ),
        )
        .unwrap();
        Fx {
            _tmp: tmp,
            repo,
            config,
            server,
            provider,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "5")
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    /// Corps JSON des requêtes de génération.
    fn bodies(&self) -> Vec<Value> {
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == path_of(self.provider))
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect()
    }

    fn to_enrich(&self) -> u64 {
        let o = self.run(&["status", "--json"]);
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        v["to_enrich"].as_u64().unwrap()
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn check_stream_fields(provider: &str, b: &Value) {
    assert_eq!(b["stream"], json!(true), "{provider} : {b}");
    if provider == "openai" {
        assert_eq!(b["stream_options"], json!({"include_usage": true}), "{b}");
    } else {
        assert!(b.get("stream_options").is_none(), "{b}");
    }
}

#[test]
fn REQ249_AC1_enrichissement_envoie_stream_true() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, vec![sse(provider, &enrichment())]);
        let o = fx.run(&["build"]);
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        let bodies = fx.bodies();
        assert_eq!(bodies.len(), 1, "{provider}");
        check_stream_fields(provider, &bodies[0]);
    }
}

#[test]
fn REQ249_AC1_query_ask_envoie_stream_true() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, vec![sse(provider, ASK_REPLY)]);
        let o = fx.run(&["build", "--no-llm"]);
        assert!(o.status.success(), "{}", err(&o));
        let o = fx.run(&["query", "ask", "Alpha"]);
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        let bodies = fx.bodies();
        assert_eq!(bodies.len(), 1, "{provider}");
        check_stream_fields(provider, &bodies[0]);
    }
}

#[test]
fn REQ249_AC1_mode_lot_sans_stream() {
    for provider in PROVIDERS {
        let server = BatchServer::start();
        let text = enrichment();
        server.simulate(provider, 0, move |_| Outcome::Text(text.clone()));
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/d.md"), "# D\n\nTexte du document.\n").unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\n",
                server.base_url
            ),
        )
        .unwrap();
        let o = Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&repo)
            .args(["build", "--batch"])
            .env("NEPHILA_CONFIG", &config)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_BATCH_POLL_MS", "5")
            .env("NEPHILA_RETRY_BASE_MS", "1")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        let lots = server.lots();
        assert_eq!(lots.len(), 1, "{provider}");
        assert!(!lots[0].is_empty());
        for body in lots.iter().flatten() {
            assert!(body.get("stream").is_none(), "{provider} : {body}");
            assert!(body.get("stream_options").is_none(), "{provider} : {body}");
        }
        for r in server.requests() {
            assert!(!r.body.contains("\"stream\""), "{provider} : {}", r.body);
        }
    }
}

/// Fin de la ligne `llm enrich fin`, à partir de ` (jetons : `.
fn tokens_tail(o: &Output) -> String {
    let e = err(o);
    let fins: Vec<&str> = e.lines().filter(|l| l.contains("llm enrich fin")).collect();
    assert_eq!(fins.len(), 1, "{e}");
    let at = fins[0]
        .find(" (jetons : ")
        .unwrap_or_else(|| panic!("{}", fins[0]));
    fins[0][at..].to_string()
}

fn same_tokens_as_json(provider: &'static str) {
    let streamed = Fx::new(provider, vec![sse(provider, &enrichment())]);
    let o = streamed.run(&["build", "--verbose"]);
    assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
    assert_eq!(streamed.to_enrich(), 0, "{provider}");
    let plain = Fx::new(provider, vec![json_reply(provider, &enrichment())]);
    let p = plain.run(&["build", "--verbose"]);
    assert_eq!(p.status.code(), Some(0), "{provider} : {}", err(&p));
    let tail = tokens_tail(&o);
    assert_eq!(tail, tokens_tail(&p), "{provider}");
    assert!(tail.contains("entrée 10"), "{tail}");
    assert!(tail.contains("sortie 7"), "{tail}");
}

#[test]
fn REQ249_AC2_anthropic_flux_donne_les_memes_jetons_que_le_json() {
    same_tokens_as_json("anthropic");
}

#[test]
fn REQ249_AC3_openai_flux_donne_les_memes_jetons_que_le_json() {
    same_tokens_as_json("openai");
}

#[test]
fn REQ249_AC3_query_ask_sur_un_flux_affiche_la_reponse() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, vec![sse(provider, ASK_REPLY)]);
        assert!(fx.run(&["build", "--no-llm"]).status.success());
        let o = fx.run(&["query", "ask", "Alpha"]);
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        assert!(out(&o).contains("réponse-ask"), "{provider} : {}", out(&o));
    }
}
