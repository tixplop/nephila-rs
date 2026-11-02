//! Schéma JSON envoyé selon `structured_output` (REQ-202, REQ-204, T-295).
#![allow(non_snake_case)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use serde_json::{json, Value};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const KEY_VAR: &str = "NEPHILA_T295_KEY";

fn reply(provider: &str, t: &str) -> String {
    if provider == "anthropic" {
        json!({"content":[{"type":"text","text":t}]}).to_string()
    } else {
        json!({"choices":[{"message":{"role":"assistant","content":t}}]}).to_string()
    }
}

fn file_reply(files: &[&str]) -> String {
    let sums: Vec<Value> = files
        .iter()
        .map(|f| json!({"node": format!("document:{f}"), "summary": "Résumé"}))
        .collect();
    json!({"summaries": sums, "concepts": []}).to_string()
}

fn lot_reply(files: &[&str]) -> String {
    let items: Vec<Value> = files
        .iter()
        .map(|f| {
            json!({"file": f, "summaries": [{"node": format!("document:{f}"), "summary": "Résumé"}],
                "concepts": []})
        })
        .collect();
    json!({"files": items}).to_string()
}

struct Http {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
    provider: String,
}

impl Http {
    /// `batch` : taille des lots ; deux documents `docs/d.md` et `docs/e.md`.
    fn new(provider: &str, extra: &str, batch: usize, bodies: Vec<String>) -> Http {
        let server = FakeServer::start(bodies.into_iter().map(|b| (200, b)).collect());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/d.md"), "# D\n\nTexte du document.\n").unwrap();
        fs::write(repo.join("docs/e.md"), "# E\n\nAutre document.\n").unwrap();
        fs::write(
            repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nmodel = \"claude-x\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = {batch}\n{extra}",
                server.base_url
            ),
        )
        .unwrap();
        Http {
            _tmp: tmp,
            repo,
            config,
            server,
            provider: provider.into(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env(KEY_VAR, "sk-test")
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn bodies(&self) -> Vec<Value> {
        let path = if self.provider == "anthropic" {
            "/v1/messages"
        } else {
            "/v1/chat/completions"
        };
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == path)
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect()
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Champ de schéma d'un corps, selon le fournisseur.
fn format_of(provider: &str, body: &Value) -> Option<Value> {
    if provider == "anthropic" {
        body.get("output_config")
            .and_then(|o| o.get("format"))
            .cloned()
    } else {
        body.get("response_format").cloned()
    }
}

fn expected(provider: &str, name: &str, schema: Value) -> Value {
    if provider == "anthropic" {
        json!({"type": "json_schema", "schema": schema})
    } else {
        json!({"type": "json_schema", "json_schema": {"name": name, "strict": true, "schema": schema}})
    }
}

fn schema_of(provider: &str, f: &Value) -> Value {
    if provider == "anthropic" {
        f["schema"].clone()
    } else {
        f["json_schema"]["schema"].clone()
    }
}

/// Un enrichissement (fichier seul si `batch` = 1, sinon lot) : corps reçus.
fn build_bodies(provider: &str, extra: &str, batch: usize) -> Vec<Value> {
    let r = if batch == 1 {
        file_reply(&["docs/d.md"])
    } else {
        lot_reply(&["docs/d.md", "docs/e.md"])
    };
    let r2 = file_reply(&["docs/e.md"]);
    let h = Http::new(
        provider,
        extra,
        batch,
        vec![reply(provider, &r), reply(provider, &r2)],
    );
    let o = h.run(&["build"]);
    assert!(o.status.success(), "{provider} {extra}: {}", err(&o));
    let b = h.bodies();
    assert!(!b.is_empty());
    b
}

#[test]
fn REQ202_AC5_on_envoie_le_schema_fichier_seul_et_lot() {
    for provider in ["openai", "anthropic"] {
        let bodies = build_bodies(provider, "structured_output = \"on\"\n", 1);
        assert_eq!(bodies.len(), 2, "{provider}");
        for b in &bodies {
            let f =
                format_of(provider, b).unwrap_or_else(|| panic!("{provider} sans schéma : {b}"));
            assert_eq!(
                f,
                expected(provider, "nephila_fichier", fileschema()),
                "{provider}"
            );
        }
        let bodies = build_bodies(provider, "structured_output = \"on\"\n", 2);
        assert_eq!(bodies.len(), 1, "{provider}");
        let f = format_of(provider, &bodies[0]).unwrap();
        assert_eq!(f["type"], "json_schema");
        let schema = schema_of(provider, &f);
        assert!(
            schema["properties"].get("files").is_some(),
            "{provider}: {schema}"
        );
        if provider == "openai" {
            assert_eq!(f["json_schema"]["name"], "nephila_lot");
        }
    }
}

fn fileschema() -> Value {
    let sums = json!({"type": "array", "items": {"type": "object",
        "properties": {"node": {"type": "string"}, "summary": {"type": "string"}},
        "required": ["node", "summary"], "additionalProperties": false}});
    let cons = json!({"type": "array", "items": {"type": "object",
        "properties": {"name": {"type": "string"}, "summary": {"type": ["string", "null"]},
            "sections": {"type": "array", "items": {"type": "string"}}},
        "required": ["name", "summary", "sections"], "additionalProperties": false}});
    json!({"type": "object", "properties": {"summaries": sums, "concepts": cons},
        "required": ["summaries", "concepts"], "additionalProperties": false})
}

#[test]
fn REQ202_AC5_sans_la_cle_et_off_aucun_schema_et_memes_prompts() {
    for provider in ["openai", "anthropic"] {
        for batch in [1, 2] {
            let on = build_bodies(provider, "structured_output = \"on\"\n", batch);
            let off = build_bodies(provider, "structured_output = \"off\"\n", batch);
            let auto = build_bodies(provider, "", batch);
            for set in [&off, &auto] {
                assert_eq!(set.len(), on.len());
                for b in set {
                    assert!(b.get("response_format").is_none(), "{b}");
                    assert!(
                        b.get("output_config")
                            .and_then(|o| o.get("format"))
                            .is_none(),
                        "{b}"
                    );
                }
            }
            let prompts = |set: &Vec<Value>| -> Vec<Value> {
                set.iter()
                    .map(|b| {
                        if provider == "anthropic" {
                            json!([b["system"], b["messages"]])
                        } else {
                            b["messages"].clone()
                        }
                    })
                    .collect()
            };
            assert_eq!(prompts(&on), prompts(&off), "{provider} {batch}");
            assert_eq!(prompts(&auto), prompts(&off), "{provider} {batch}");
        }
    }
}

#[test]
fn REQ202_AC6_passer_de_off_a_on_ne_refait_aucun_fichier() {
    let h = Http::new(
        "openai",
        "structured_output = \"off\"\n",
        1,
        vec![
            reply("openai", &file_reply(&["docs/d.md"])),
            reply("openai", &file_reply(&["docs/e.md"])),
        ],
    );
    let o = h.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    let before = h.server.requests().len();
    let cfg = fs::read_to_string(&h.config).unwrap();
    fs::write(&h.config, cfg.replace("\"off\"", "\"on\"")).unwrap();
    let o = h.run(&["update"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "graphe à jour");
    assert_eq!(h.server.requests().len(), before);
}

const ASK_REPLY: &str = r#"{"found":true,"answer":"réponse","nodes":["document:docs/d.md"]}"#;

fn ask_bodies(provider: &str, extra: &str) -> Vec<Value> {
    let h = Http::new(provider, extra, 1, vec![reply(provider, ASK_REPLY); 2]);
    let o = h.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", err(&o));
    let o = h.run(&["query", "ask", "Alpha"]);
    assert!(o.status.success(), "{provider}: {}", err(&o));
    let b = h.bodies();
    assert_eq!(b.len(), 1);
    b
}

#[test]
fn REQ204_AC3_ask_on_envoie_le_schema_nephila_ask() {
    for provider in ["openai", "anthropic"] {
        let b = &ask_bodies(provider, "[ask]\nstructured_output = \"on\"\n")[0];
        let f = format_of(provider, b).unwrap_or_else(|| panic!("{provider} sans schéma : {b}"));
        let schema = json!({"type": "object", "properties": {
            "found": {"type": "boolean"}, "answer": {"type": ["string", "null"]},
            "nodes": {"type": ["array", "null"], "items": {"type": "string"}}},
            "required": ["found", "answer", "nodes"], "additionalProperties": false});
        assert_eq!(f, expected(provider, "nephila_ask", schema), "{provider}");

        // Sans la clé, même avec `structured_output = "on"` au premier niveau : pas de schéma.
        let b = &ask_bodies(provider, "structured_output = \"on\"\n")[0];
        assert!(format_of(provider, b).is_none(), "{provider}: {b}");
    }
}
