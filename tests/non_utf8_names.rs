//! Fichiers aux noms non UTF-8 (REQ-004, DEC-070) : lus par leur chemin réel à l'analyse et à
//! l'enrichissement, jamais en attente. Unix seulement ; non applicable si le système de fichiers
//! refuse la création (APFS, règle d'AC-004-8).
#![cfg(unix)]
#![allow(non_snake_case)]

use assert_cmd::cargo::cargo_bin;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

impl Env {
    /// `None` si le système de fichiers refuse les noms non UTF-8.
    fn new(config: &str) -> Option<Env> {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            mock: tmp.path().join("mock"),
            log: tmp.path().join("mock.log"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
        };
        fs::create_dir_all(&env.repo).unwrap();
        fs::create_dir_all(&env.mock).unwrap();
        fs::write(&env.config, config).unwrap();
        let name = OsStr::from_bytes(b"A\xff.java");
        if fs::write(env.repo.join(name), "class A { void run() {} }\n").is_err() {
            return None;
        }
        fs::write(env.repo.join("B.java"), "class B {}\n").unwrap();
        Some(env)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .output()
            .unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn first_line(o: &Output) -> String {
    out(o).lines().next().unwrap_or("").to_string()
}

#[test]
fn REQ004_AC9_sans_llm_le_fichier_n_est_pas_en_attente() {
    let Some(env) = Env::new("provider = \"mock\"\n") else {
        return;
    };
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!err(&o).contains("avertissement:"), "{}", err(&o));
    assert!(out(&o).contains(", 0 fichiers en attente,"), "{}", out(&o));
    let o = env.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    let o = env.run(&["query", "search", "run"]);
    assert!(out(&o).contains("A.run"), "{}", out(&o));
    assert!(out(&o).contains("A\u{FFFD}.java"), "{}", out(&o));
    let o = env.run(&["update", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = env.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    let o = env.run(&["query", "search", "A", "--check-stale"]);
    assert_eq!(err(&o), "");
}

#[test]
fn REQ004_AC9_avec_llm_simule_le_fichier_est_enrichi() {
    let Some(env) = Env::new("provider = \"mock\"\nenrich_code = true\n") else {
        return;
    };
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let log = fs::read_to_string(&env.log).unwrap_or_default();
    assert!(
        log.lines().any(|l| l.contains("A\u{FFFD}.java")),
        "journal : {log:?}"
    );
    let o = env.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    assert!(!out(&o).contains("à enrichir"), "{}", out(&o));
    assert!(!out(&o).contains("en attente"), "{}", out(&o));
    let _: &Path = &env.mock;
}
