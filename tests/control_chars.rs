//! Caractères de contrôle des données dans les sorties texte (REQ-099, DEC-103) : `\u{XXXX}`
//! en texte, `--json` inchangé.
#![cfg(unix)]
#![allow(non_snake_case)]

use assert_cmd::cargo::cargo_bin;
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Command, Output};

/// Clé feuille de `data.json` (DEC-038) ; ESC et BEL dans le nom.
const KEY: &str = "evil\u{1b}]52;c;ZWNobyBwd25lZA==\u{7}key";
const SHOWN: &str = "evil\\u{001B}]52;c;ZWNobyBwd25lZA==\\u{0007}key";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn has_byte(b: &[u8], x: u8) -> bool {
    b.contains(&x)
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            mock: tmp.path().join("mock"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
        };
        fs::create_dir_all(&env.repo).unwrap();
        fs::create_dir_all(&env.mock).unwrap();
        fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        env.file("A.java", "class A { void run() {} }\n");
        env
    }

    fn file(&self, name: &str, content: &str) {
        fs::write(self.repo.join(name), content).unwrap();
    }

    fn cmd(&self, args: &[&OsStr]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env_remove("NEPHILA_MOCK_FAIL_AFTER");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        let a: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        self.cmd(&a).output().unwrap()
    }

    /// Argument aux octets bruts (`Command::arg`).
    fn run_raw(&self, args: &[&[u8]]) -> Output {
        let a: Vec<&OsStr> = args.iter().map(|b| OsStr::from_bytes(b)).collect();
        self.cmd(&a).output().unwrap()
    }

    fn evil() -> Env {
        let env = Env::new();
        env.file("data.json", &json!({ KEY: "x" }).to_string());
        let o = env.run(&["build", "--no-llm"]);
        assert!(o.status.success(), "{}", text(&o.stderr));
        env
    }
}

fn qualified() -> String {
    format!("data.json#{KEY}")
}

/// Une chaîne lue contient le caractère U+001B lui-même.
fn has_esc(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains('\u{1b}'),
        Value::Array(a) => a.iter().any(has_esc),
        Value::Object(o) => o.iter().any(|(k, v)| k.contains('\u{1b}') || has_esc(v)),
        _ => false,
    }
}

fn assert_clean(o: &Output, what: &str) {
    assert!(o.status.success(), "{what}: {}", text(&o.stderr));
    for b in [0x1bu8, 0x07, 0x0d] {
        assert!(
            !has_byte(&o.stdout, b),
            "{what}: octet {b:#x} : {:?}",
            o.stdout
        );
    }
}

#[test]
fn req099_ac1_search_echappe_les_controles() {
    let env = Env::evil();
    let o = env.run(&["query", "search", "evil"]);
    assert_clean(&o, "search");
    assert!(text(&o.stdout).contains(SHOWN), "{}", text(&o.stdout));
}

#[test]
fn req099_ac1_neighbors_echappe_les_controles() {
    let env = Env::evil();
    let q = qualified();
    let o = env.run_raw(&[b"query", b"neighbors", q.as_bytes()]);
    assert_clean(&o, "neighbors");
    // Les voisins de la clé n'incluent pas la clé : ceux du document la montrent.
    let o = env.run(&["query", "neighbors", "data.json"]);
    assert_clean(&o, "neighbors data.json");
    assert!(text(&o.stdout).contains(SHOWN), "{}", text(&o.stdout));
}

#[test]
fn req099_ac1_path_echappe_les_controles() {
    let env = Env::evil();
    let q = qualified();
    let o = env.run_raw(&[b"query", b"path", q.as_bytes(), b"data.json"]);
    assert_clean(&o, "path");
    assert!(text(&o.stdout).contains(SHOWN), "{}", text(&o.stdout));
}

fn enriched() -> Env {
    let env = Env::new();
    env.file("data.json", &json!({ KEY: "x" }).to_string());
    env.file("README.md", "# Titre\n\ntexte\n");
    fs::write(
        env.mock.join("data.json.json"),
        json!({"version":2,"summaries":[{"node":"document:data.json","summary":"evil r\u{1b}[2J"}],"concepts":[]})
            .to_string(),
    )
    .unwrap();
    fs::write(
        env.mock.join("README.md.json"),
        json!({"version":2,"summaries":[],"concepts":[{"name":"c\u{1b}]0;t\u{7}","summary":"s","sections":["section:README.md#titre"]}]})
            .to_string(),
    )
    .unwrap();
    fs::write(
        env.mock.join("_ask.json"),
        json!({"found":true,"answer":"ligne 1\u{1b}[31m\nligne 2\r","nodes":[qualified()]})
            .to_string(),
    )
    .unwrap();
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
    )
    .unwrap();
    let o = env.run(&["build"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    env
}

#[test]
fn req099_ac2_summary_et_search_echappent_resume_et_concept() {
    let env = enriched();
    let s = env.run(&["summary"]);
    assert_clean(&s, "summary");
    assert!(
        text(&s.stdout).contains("c\\u{001B}]0;t\\u{0007}"),
        "{}",
        text(&s.stdout)
    );
    let q = env.run(&["query", "search", "evil"]);
    assert_clean(&q, "search");
    assert!(
        text(&q.stdout).contains("\\u{001B}[2J"),
        "{}",
        text(&q.stdout)
    );
}

#[test]
fn req099_ac2_ask_garde_les_sauts_de_ligne_de_la_reponse() {
    let env = enriched();
    let o = env.run(&["query", "ask", "evil ?"]);
    assert_clean(&o, "ask");
    let out = text(&o.stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "ligne 1\\u{001B}[31m", "{out}");
    assert_eq!(lines[1], "ligne 2\\u{000D}", "{out}");
    assert_eq!(lines[2], "Nœuds cités :", "{out}");
    assert!(lines[3].starts_with("- "), "{out}");
    assert!(lines[3].contains("evil\\u{001B}]52;c;"), "{out}");
}

#[test]
fn req099_ac3_json_inchange_pour_les_cinq_commandes() {
    let env = enriched();
    let q = qualified();
    let runs: Vec<(&str, Output)> = vec![
        ("search", env.run(&["query", "search", "evil", "--json"])),
        (
            "neighbors",
            env.run_raw(&[b"query", b"neighbors", q.as_bytes(), b"--json"]),
        ),
        (
            "path",
            env.run_raw(&[b"query", b"path", q.as_bytes(), b"data.json", b"--json"]),
        ),
        ("ask", env.run(&["query", "ask", "evil ?", "--json"])),
        ("summary", env.run(&["summary", "--json"])),
    ];
    for (what, o) in runs {
        assert!(o.status.success(), "{what}: {}", text(&o.stderr));
        let out = text(&o.stdout);
        assert!(!out.contains("\\u{"), "{what}: {out}");
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(has_esc(&v), "{what}: {out}");
        assert_eq!(out, format!("{v}\n"), "{what}");
        assert!(out.contains("\\u001b"), "{what}: {out}");
    }
}

#[test]
fn req099_ac4_avertissement_echappe_le_nom_du_fichier() {
    let env = Env::new();
    env.file("bad\u{1b}[1m.json", "{");
    let o = env.run(&["build", "--no-llm"]);
    assert!(!has_byte(&o.stderr, 0x1b), "{:?}", o.stderr);
    let err = text(&o.stderr);
    let l = err
        .lines()
        .find(|l| l.starts_with("avertissement:") && l.contains("bad"))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(l.contains("bad\\u{001B}[1m.json"), "{l}");
}

#[test]
fn req099_ac4_candidats_ambigus_echappes_une_ligne_chacun() {
    let env = Env::new();
    env.file(
        "data.json",
        &json!({"x\u{1b}": {"dup": 1}, "y\u{1b}": {"dup": 2}}).to_string(),
    );
    let o = env.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let o = env.run(&["query", "neighbors", "dup"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(!has_byte(&o.stderr, 0x1b), "{:?}", o.stderr);
    let err = text(&o.stderr);
    let cands: Vec<&str> = err.lines().filter(|l| l.starts_with("  ")).collect();
    assert_eq!(cands.len(), 2, "{err}");
    for c in cands {
        assert!(c.contains("\\u{001B}"), "{err}");
    }
}

#[test]
fn req099_ac7_une_ligne_par_resultat() {
    let plain = Env::new();
    plain.file("data.json", r#"{"nlkey": 1, "crkey": 2, "tabkey": 3}"#);
    assert!(plain.run(&["build", "--no-llm"]).status.success());
    let reference = plain.run(&["query", "search", "key"]);
    let env = Env::new();
    env.file(
        "data.json",
        &json!({"nl\nkey": 1, "cr\rkey": 2, "tab\tkey": 3}).to_string(),
    );
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let o = env.run(&["query", "search", "key"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(!has_byte(&o.stdout, 0x0d), "{:?}", o.stdout);
    let out = text(&o.stdout);
    assert_eq!(out.lines().count(), text(&reference.stdout).lines().count());
    assert!(out.lines().any(|l| l.contains("nl\\u{000A}key")), "{out}");
    assert!(out.lines().any(|l| l.contains("cr\\u{000D}key")), "{out}");
    assert!(out.lines().any(|l| l.contains("tab\tkey")), "{out}");
}

#[test]
fn req099_ac8_avertissement_sur_une_ligne() {
    let env = Env::new();
    env.file("bad\nx.json", "{");
    let o = env.run(&["build", "--no-llm"]);
    let err = text(&o.stderr);
    assert!(
        err.lines()
            .any(|l| l.starts_with("avertissement: ") && l.contains("bad\\u{000A}x.json")),
        "{err}"
    );
    for l in err.lines() {
        assert!(
            l.starts_with("avertissement: ") || l.starts_with("erreur: "),
            "{l:?}"
        );
    }
}
