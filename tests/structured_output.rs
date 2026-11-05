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
        "properties": {"n": {"type": "integer"}, "summary": {"type": "string"}},
        "required": ["n", "summary"], "additionalProperties": false}});
    let cons = json!({"type": "array", "items": {"type": "object",
        "properties": {"name": {"type": "string"}, "summary": {"type": ["string", "null"]},
            "sections": {"type": "array", "items": {"type": "string"}}},
        "required": ["name", "summary", "sections"], "additionalProperties": false}});
    json!({"type": "object", "properties": {"summaries": sums, "concepts": cons},
        "required": ["summaries", "concepts"], "additionalProperties": false})
}

#[test]
fn REQ236_AC7_requete_avec_liste_numerotee_et_schema_a_numeros() {
    for provider in ["openai", "anthropic"] {
        for mode in ["on", "off"] {
            let extra = format!("structured_output = \"{mode}\"\n");
            let one = build_bodies(provider, &extra, 1);
            assert_eq!(one.len(), 2, "{provider} {mode}");
            for (b, f) in one.iter().zip(["d", "e"]) {
                let text = b.to_string();
                assert!(
                    text.contains(&format!("[1] {f}.md — document:docs/{f}.md")),
                    "{provider} {mode}: {text}"
                );
                let up = f.to_uppercase();
                assert!(
                    text.contains(&format!("[2] {f} — section:docs/{f}.md#{f}")),
                    "{provider} {mode}: {up} {text}"
                );
                if mode == "on" {
                    let fmt = format_of(provider, b).unwrap();
                    assert_eq!(fmt, expected(provider, "nephila_fichier", fileschema()));
                }
            }
            let lot = build_bodies(provider, &extra, 2);
            let text = lot[0].to_string();
            for want in [
                "=== FICHIER [1] : docs/d.md ===",
                "=== FICHIER [2] : docs/e.md ===",
                "[1] d.md — document:docs/d.md",
                "[2] d — section:docs/d.md#d",
                "[3] e.md — document:docs/e.md",
                "[4] e — section:docs/e.md#e",
            ] {
                assert!(text.contains(want), "{provider} {mode}: {want}");
            }
            if mode == "on" {
                let schema = schema_of(provider, &format_of(provider, &lot[0]).unwrap());
                let item = &schema["properties"]["files"]["items"];
                assert_eq!(
                    item["properties"]["summaries"],
                    fileschema()["properties"]["summaries"]
                );
                assert_eq!(item["properties"]["f"], json!({"type": "integer"}));
                assert_eq!(item["required"], json!(["f", "summaries", "concepts"]));
            }
        }
    }
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

// ---------------------------------------------------------------- T-297 (AC UX)

fn mock_repo(project: Option<&str>, personal: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("a.md"), "# a\n").unwrap();
    if let Some(p) = project {
        fs::write(repo.join(".nephila.toml"), p).unwrap();
    }
    let config = tmp.path().join("config.toml");
    fs::write(&config, personal).unwrap();
    (tmp, repo, config)
}

fn nephila(repo: &PathBuf, config: &PathBuf, args: &[&str]) -> Output {
    Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("NEPHILA_CONFIG", config)
        .env_remove("NEPHILA_MOCK_DIR")
        .env_remove("NEPHILA_MOCK_LOG")
        .output()
        .unwrap()
}

fn lines_of(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn REQ202_AC8_valeur_invalide_une_seule_ligne_e15() {
    for (value, shown) in [("\"ON\"", "\"ON\""), ("true", "true")] {
        let (_t, repo, config) = mock_repo(
            None,
            &format!("provider = \"mock\"\nstructured_output = {value}\n"),
        );
        let o = nephila(&repo, &config, &["build"]);
        assert_eq!(o.status.code(), Some(1), "{value}");
        assert_eq!(
            err(&o),
            format!(
                "erreur: configuration invalide ({}) : clé « structured_output » doit être \"auto\", \"on\" ou \"off\" (reçu : {shown})\n",
                config.display()
            ),
            "{value}"
        );
        assert!(!repo.join(".nephila").exists(), "{value}");

        let o = nephila(&repo, &config, &["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{value}: {}", err(&o));
        assert!(
            !lines_of(&o.stderr).iter().any(|l| l.contains("erreur:")),
            "{value}"
        );
    }
}

#[test]
fn REQ202_AC8_cle_au_premier_niveau_du_fichier_de_projet() {
    let (_t, repo, config) = mock_repo(
        Some("structured_output = \"on\"\n"),
        "provider = \"mock\"\n",
    );
    let o = nephila(&repo, &config, &["build"]);
    assert_eq!(o.status.code(), Some(1));
    let lines = lines_of(&o.stderr);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let l = &lines[0];
    assert!(l.starts_with("erreur: "), "{l}");
    let (_, list) = l
        .split_once("clé inconnue `structured_output` (clés permises : ")
        .unwrap_or_else(|| panic!("{l}"));
    assert!(!list.contains("structured_output"), "{l}");
    assert!(!l.contains("invalid type") && !l.contains("TOML"), "{l}");
}

#[test]
fn REQ204_AC4_ask_structured_output_booleen_refuse_par_ask_et_build() {
    let (_t, repo, config) = mock_repo(
        None,
        "provider = \"mock\"\n[ask]\nstructured_output = true\n",
    );
    assert_eq!(
        nephila(&repo, &config, &["build", "--no-llm"])
            .status
            .code(),
        Some(0)
    );
    let expected = format!(
        "erreur: configuration invalide ({}) : clé « ask.structured_output » doit être \"auto\", \"on\" ou \"off\" (reçu : true)\n",
        config.display()
    );
    for args in [&["query", "ask", "x"][..], &["build"][..]] {
        let o = nephila(&repo, &config, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert_eq!(err(&o), expected, "{args:?}");
        assert!(o.stdout.is_empty(), "{args:?}");
    }
}

/// Trois documents, faux serveur `openai` qui accepte tout corps : stdout, stderr.
fn three_docs(extra: &str, args: &[&str]) -> (String, String) {
    let names = ["docs/d.md", "docs/e.md", "docs/f.md"];
    let replies = names
        .iter()
        .map(|n| reply("openai", &file_reply(&[n])))
        .collect();
    let h = Http::new("openai", extra, 1, replies);
    fs::write(h.repo.join("docs/f.md"), "# F\n\nTroisième.\n").unwrap();
    let mut a = vec!["build"];
    a.extend_from_slice(args);
    let o = h.run(&a);
    assert!(o.status.success(), "{extra}: {}", err(&o));
    assert_eq!(h.bodies().len(), 3);
    (String::from_utf8_lossy(&o.stdout).into_owned(), err(&o))
}

/// Retire le préfixe `trace: [<s>.<mmm>s] ` et remplace les millisecondes par `<ms>`.
fn normalized(stderr: &str) -> Vec<String> {
    let prefix = |l: &str| -> String {
        match l.strip_prefix("trace: [").and_then(|r| r.split_once("] ")) {
            Some((_, rest)) => rest.to_string(),
            None => l.to_string(),
        }
    };
    stderr
        .lines()
        .map(|l| {
            let l = prefix(l);
            let words: Vec<&str> = l.split(' ').collect();
            let mut out: Vec<&str> = Vec::new();
            for (i, w) in words.iter().enumerate() {
                let is_ms = words
                    .get(i + 1)
                    .is_some_and(|n| n.trim_end_matches(',') == "ms")
                    && !w.is_empty()
                    && w.chars().all(|c| c.is_ascii_digit());
                out.push(if is_ms { "<ms>" } else { w });
            }
            out.join(" ")
        })
        .collect()
}

#[test]
fn REQ202_AC9_on_et_off_donnent_la_meme_sortie() {
    let (_, on) = three_docs("structured_output = \"on\"\n", &["--verbose"]);
    let (_, off) = three_docs("structured_output = \"off\"\n", &["--verbose"]);
    let (on, off) = (normalized(&on), normalized(&off));
    assert!(on.len() > 3, "{on:?}");
    assert!(on.iter().any(|l| l.contains("<ms>")), "{on:?}");
    assert_eq!(on, off);
    for l in on.iter().chain(&off) {
        assert!(
            !l.contains("sortie structurée")
                && !l.contains("structured_output")
                && !l.contains("response_format"),
            "{l}"
        );
    }
    let a = three_docs("structured_output = \"on\"\n", &[]);
    let b = three_docs("structured_output = \"off\"\n", &[]);
    assert_eq!(a, b);
}

/// `query ask` sur le faux serveur `openai` : la réponse du modèle, la clé `[ask]` éventuelle.
fn ask_once(extra: &str, model_reply: &str, args: &[&str]) -> Output {
    let h = Http::new("openai", extra, 1, vec![reply("openai", model_reply)]);
    let o = h.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", err(&o));
    let mut a = vec!["query", "ask"];
    a.extend_from_slice(args);
    a.push("Alpha");
    let o = h.run(&a);
    assert_eq!(h.bodies().len(), 1);
    o
}

fn config_line(o: &Output) -> String {
    normalized(&err(o))
        .into_iter()
        .find(|l| l.starts_with("config: "))
        .unwrap_or_else(|| panic!("pas de ligne config : {}", err(o)))
}

#[test]
fn REQ204_AC5_champs_null_du_schema_equivalent_aux_champs_absents() {
    let on = "[ask]\nstructured_output = \"on\"\n";
    let cases = [
        (
            r#"{"found": false, "answer": null, "nodes": null}"#,
            r#"{"found": false}"#,
        ),
        (
            r#"{"found": true, "answer": "x", "nodes": null}"#,
            r#"{"found": true, "answer": "x"}"#,
        ),
    ];
    for (with_null, without) in cases {
        for flags in [&[][..], &["--json"][..]] {
            let a = ask_once(on, with_null, flags);
            let b = ask_once("", without, flags);
            assert_eq!(a.stdout, b.stdout, "{with_null} {flags:?}");
            assert_eq!(a.stderr, b.stderr, "{with_null} {flags:?}");
            assert_eq!(a.status.code(), b.status.code(), "{with_null} {flags:?}");
            assert!(
                !a.stdout.is_empty() || !a.stderr.is_empty(),
                "{with_null} {flags:?}"
            );
        }
        let a = ask_once(on, with_null, &["--verbose"]);
        let b = ask_once("", without, &["--verbose"]);
        assert_eq!(config_line(&a), config_line(&b), "{with_null}");
    }
}
