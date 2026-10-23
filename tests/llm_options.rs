//! Options LLM `thinking`, `effort` et `max_output_tokens` (REQ-058, T-134) : validation,
//! erreur E-15, ligne `config`, aide.
#![allow(non_snake_case)]

use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new(config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("A.java"), "package p;\npublic class A {}\n").unwrap();
        fs::write(repo.join("README.md"), "# Titre\n\nTexte.\n").unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, config).unwrap();
        Env {
            _tmp: tmp,
            repo,
            config: config_path,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn write_config(&self, text: &str) {
        fs::write(&self.config, text).unwrap();
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn config_line(o: &Output) -> String {
    stderr(o)
        .lines()
        .find(|l| l.contains("config: "))
        .unwrap_or_else(|| panic!("ligne config absente : {}", stderr(o)))
        .to_string()
}

const MOCK: &str = "provider = \"mock\"\n";
const EFFORT: &str = "\"low\", \"medium\" ou \"high\"";
const BOOL: &str = "un booléen, true ou false";
const INT: &str = "un entier strictement positif";

/// (ligne TOML, clé, attendu, valeur réécrite)
fn invalid() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        ("effort = \"max\"", "effort", EFFORT, "\"max\""),
        ("effort = \"\"", "effort", EFFORT, "\"\""),
        ("effort = \"LOW\"", "effort", EFFORT, "\"LOW\""),
        ("effort = 1", "effort", EFFORT, "1"),
        ("thinking = \"oui\"", "thinking", BOOL, "\"oui\""),
        ("thinking = 1", "thinking", BOOL, "1"),
        ("max_output_tokens = 0", "max_output_tokens", INT, "0"),
        ("max_output_tokens = -5", "max_output_tokens", INT, "-5"),
        ("max_output_tokens = 1.5", "max_output_tokens", INT, "1.5"),
        (
            "max_output_tokens = \"16384\"",
            "max_output_tokens",
            INT,
            "\"16384\"",
        ),
    ]
}

#[test]
fn REQ058_AC2_AC14_valeur_invalide_refusee_avant_tout_parcours_et_tout_appel() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    for (line, key, expected, got) in invalid() {
        for provider in [
            "provider = \"mock\"\n".to_string(),
            format!("provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\n"),
        ] {
            let env = Env::new(&format!("{provider}{line}\n"));
            let want = format!(
                "erreur: configuration invalide ({}) : clé « {key} » doit être {expected} (reçu : {got})\n",
                env.config.display()
            );
            for args in [&["build"][..], &["update"][..], &["query", "ask", "x"][..]] {
                let o = env.run(args);
                assert!(!o.status.success(), "{line} {args:?}");
                assert!(o.stdout.is_empty(), "{line} {args:?}");
                assert_eq!(stderr(&o), want, "{line} {args:?}");
                assert!(!env.repo.join(".nephila").exists(), "{line} {args:?}");
            }
            for sub in ["build", "update"] {
                let o = env.run(&[sub, "--no-llm"]);
                assert_eq!(o.status.code(), Some(0), "{line} {sub}: {}", stderr(&o));
                assert_eq!(stderr(&o), "", "{line} {sub}");
            }
        }
    }
    assert!(
        server.accept().is_err(),
        "le faux serveur a reçu une connexion"
    );
}

#[test]
fn REQ058_AC2_configuration_invalide_laisse_le_graphe_intact() {
    let env = Env::new(MOCK);
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let db = env.repo.join(".nephila").join("graph.db");
    let before = fs::read(&db).unwrap();
    env.write_config(&format!("{MOCK}effort = \"max\"\n"));
    assert!(!env.run(&["update"]).status.success());
    assert_eq!(before, fs::read(&db).unwrap());
}

#[test]
fn REQ058_AC7_nephila_toml_refuse_les_trois_cles() {
    for line in [
        "effort = \"low\"",
        "thinking = true",
        "max_output_tokens = 100",
    ] {
        let env = Env::new(MOCK);
        fs::write(env.repo.join(".nephila.toml"), format!("{line}\n")).unwrap();
        let o = env.run(&["build"]);
        assert!(!o.status.success(), "{line}");
        let key = line.split(' ').next().unwrap();
        assert!(
            stderr(&o).contains(&format!("clé inconnue `{key}`")),
            "{line} : {}",
            stderr(&o)
        );
        assert!(stderr(&o).contains("clés permises :"), "{}", stderr(&o));
    }
}

#[test]
fn REQ058_AC10_AC13_ligne_config_mock() {
    let env = Env::new(MOCK);
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(
            " model=mock enrich_code=false thinking=false effort=low max_output_tokens=16384 concurrency=4 "
        ),
        "{l}"
    );
    assert!(l.ends_with(" batch_files=10 format_retries=2"), "{l}");

    env.write_config(&format!(
        "{MOCK}thinking = true\neffort = \"high\"\nmax_output_tokens = 4096\n"
    ));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(
            " enrich_code=false thinking=true effort=high max_output_tokens=4096 concurrency="
        ),
        "{l}"
    );

    env.write_config(&format!("{MOCK}enrich_code = true\neffort = \"medium\"\n"));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(
            " enrich_code=true thinking=false effort=medium max_output_tokens=16384 concurrency="
        ),
        "{l}"
    );

    env.write_config(MOCK);
    fs::write(env.repo.join(".nephila.toml"), "model = \"m-projet\"\n").unwrap();
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(l.ends_with(" (.nephila.toml : model)"), "{l}");
    let project = l.split("(.nephila.toml").nth(1).unwrap();
    for k in ["thinking", "effort", "max_output_tokens"] {
        assert!(!project.contains(k), "{l}");
    }
    fs::remove_file(env.repo.join(".nephila.toml")).unwrap();

    env.write_config(&format!("{MOCK}tls_insecure = true\n"));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(l.ends_with(" format_retries=2 tls_insecure"), "{l}");
    for k in [" thinking=", " effort=", " max_output_tokens="] {
        assert_eq!(l.matches(k).count(), 1, "{l}");
    }
}

#[test]
fn REQ058_AC10_ligne_config_openai() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let env = Env::new(&format!(
        "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\nthinking = true\nmax_output_tokens = 99\n"
    ));
    // Sans document à enrichir, aucun appel n'est émis : le faux serveur ne répond pas.
    fs::remove_file(env.repo.join("README.md")).unwrap();
    let o = env.run(&["build", "--verbose", "--no-llm"]);
    assert!(!stderr(&o).contains("thinking"), "{}", stderr(&o));
    let o = env.run(&["build", "--verbose"]);
    let l = config_line(&o);
    assert!(
        l.contains(" llm=openai model=m enrich_code=false thinking=true effort=low max_output_tokens=99 concurrency="),
        "{l}"
    );
    assert!(server.accept().is_err());
}

#[test]
fn REQ058_AC15_aide_sans_les_options_llm() {
    for args in [
        &["build", "--help"][..],
        &["update", "--help"][..],
        &["query", "ask", "--help"][..],
    ] {
        let o = Command::new(cargo_bin("nephila"))
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success());
        let h = String::from_utf8_lossy(&o.stdout);
        for k in ["thinking", "effort", "max_output_tokens"] {
            assert!(!h.contains(k), "{args:?} contient {k}");
        }
    }
}
