//! Option `--batch` de `build` et `update` (REQ-211) : portée, refus d'usage, refus propre du
//! fournisseur `mock`, non-régression de `.nephila.toml` (AC-211-1 à AC-211-5, AC-211-7,
//! AC-211-8 pour `mock`).

#![allow(dead_code)]

#[path = "support/batch_server.rs"]
mod batch_server;
#[path = "support/fake_server.rs"]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use batch_server::BatchServer;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const KEY_VAR: &str = "NEPHILA_T305_KEY";
const E31: &str =
    "erreur: ce fournisseur n'a pas de mode lot ; relancez nephila update sans --batch";

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("code de sortie")
}

fn help(args: &[&str]) -> String {
    let o = Command::new(cargo_bin("nephila"))
        .args(args)
        .arg("--help")
        .output()
        .unwrap();
    out(&o).split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

impl Env {
    /// `config` : contenu de la configuration personnelle, ou `None` pour n'en avoir aucune.
    fn new(config: Option<&str>) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&mock).unwrap();
        let config_path = tmp.path().join("config.toml");
        if let Some(c) = config {
            fs::write(&config_path, c).unwrap();
        }
        fs::write(repo.join("a.md"), "# A\n\nTexte du document.\n").unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config: config_path,
        }
    }

    fn mock() -> Env {
        Env::new(Some("provider = \"mock\"\nmodel = \"mock\"\n"))
    }

    fn server(provider: &str, url: &str) -> Env {
        Env::new(Some(&format!(
            "provider = \"{provider}\"\nmodel = \"m\"\nbase_url = \"{url}\"\napi_key_env = \"{KEY_VAR}\"\n"
        )))
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env(KEY_VAR, "sk-test")
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    fn calls(&self) -> usize {
        fs::read_to_string(&self.log)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn to_enrich(&self) -> usize {
        let o = self.run(&["status", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
        v["to_enrich"].as_u64().unwrap() as usize
    }
}

#[test]
fn req211_ac1_aide_de_build_et_update_cite_batch_et_batch_files() {
    for cmd in ["build", "update"] {
        let h = help(&[cmd]);
        assert!(h.contains("--batch Confie"), "{cmd} : {h}");
        assert!(h.contains("--batch-files <N>"), "{cmd} : {h}");
    }
    for args in [
        vec!["status", "--batch"],
        vec!["query", "ask", "--batch", "x"],
    ] {
        let o = Command::new(cargo_bin("nephila"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(code(&o), 2, "{args:?}");
        assert!(
            err(&o).contains("unexpected argument '--batch' found"),
            "{args:?} : {}",
            err(&o)
        );
    }
}

#[test]
fn req211_ac2_no_llm_et_batch_sont_incompatibles() {
    for cmd in ["build", "update"] {
        let env = Env::mock();
        let o = env.run(&[cmd, "--no-llm", "--batch"]);
        assert_eq!(code(&o), 2, "{cmd}");
        let e = err(&o);
        assert!(
            e.contains("--no-llm") && e.contains("--batch"),
            "{cmd} : {e}"
        );
        assert!(!env.repo.join(".nephila").exists(), "{cmd}");
    }
}

#[test]
fn req211_ac3_sans_configuration_batch_ne_change_rien() {
    for cmd in ["build", "update"] {
        let plain = Env::new(None);
        let batch = Env::new(None);
        let a = plain.run(&[cmd]);
        let b = batch.run(&[cmd, "--batch"]);
        assert_eq!(code(&b), 0, "{cmd} : {}", err(&b));
        assert_eq!(code(&a), 0);
        assert_eq!(out(&a), out(&b), "{cmd}");
        assert_eq!(err(&a), err(&b), "{cmd}");
        assert_eq!(batch.calls(), 0);
    }
}

#[test]
fn req211_ac4_dry_run_batch_n_envoie_rien_et_ne_change_pas_la_sortie() {
    for provider in ["anthropic", "openai"] {
        let server = BatchServer::start();
        let env = Env::server(provider, &server.base_url);
        let plain = env.run(&["build", "--dry-run"]);
        let batch = env.run(&["build", "--dry-run", "--batch"]);
        assert_eq!(code(&plain), 0, "{provider} : {}", err(&plain));
        assert_eq!(code(&batch), 0, "{provider} : {}", err(&batch));
        assert_eq!(out(&plain), out(&batch), "{provider}");
        assert_eq!(err(&plain), err(&batch), "{provider}");
        assert!(server.requests().is_empty(), "{provider}");
    }
}

#[test]
fn req211_ac5_mock_refuse_le_mode_lot_et_garde_la_structure() {
    let env = Env::mock();
    let o = env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(err(&o).lines().last(), Some(E31), "{}", err(&o));
    assert!(!env.log.exists(), "le mock ne doit pas être appelé");
    let q = env.run(&["query", "search", "A"]);
    assert_eq!(code(&q), 0);
    assert!(out(&q).contains("a.md"), "{}", out(&q));
    assert_eq!(env.to_enrich(), 1);
}

#[test]
fn req211_ac5_rien_a_enrichir_graphe_a_jour_sans_erreur() {
    let env = Env::mock();
    let b = env.run(&["build"]);
    assert_eq!(code(&b), 0, "{}", err(&b));
    let calls = env.calls();
    let o = env.run(&["update", "--batch"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("graphe à jour"), "{}", out(&o));
    assert!(!err(&o).contains("erreur:"), "{}", err(&o));
    assert_eq!(env.calls(), calls);
}

#[test]
fn req211_ac7_batch_n_est_pas_une_cle_de_projet() {
    let env = Env::mock();
    fs::write(env.repo.join(".nephila.toml"), "batch = true\n").unwrap();
    let o = env.run(&["build"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(
        err(&o).contains(".nephila.toml : clé inconnue `batch` (clés permises : "),
        "{}",
        err(&o)
    );
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req211_ac8_texte_exact_du_refus_du_mock_apres_le_bilan() {
    for cmd in ["build", "update"] {
        let env = Env::mock();
        let o = env.run(&[cmd, "--batch"]);
        assert_eq!(code(&o), 1, "{cmd}");
        let e = err(&o);
        assert_eq!(e.lines().last(), Some(E31), "{cmd} : {e}");
        assert_eq!(e.matches("erreur:").count(), 1, "{cmd} : {e}");
        assert!(out(&o).contains("terminé"), "{cmd} : {}", out(&o));
        assert!(
            out(&o).contains("1 fichiers à enrichir"),
            "{cmd} : {}",
            out(&o)
        );
    }
}
