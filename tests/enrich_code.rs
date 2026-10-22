//! Clé `enrich_code` et option `--enrich-code` (REQ-050, T-117) : validation, aide, trace.
#![allow(non_snake_case)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;

const ENTRY: &str = "--enrich-code Enrichit aussi le code source et les fichiers de build et de données, pas seulement les documents Markdown (remplace `enrich_code` de la configuration ; défaut : documents seulement)";

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

    fn graph_dir(&self) -> PathBuf {
        self.repo.join(".nephila")
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

fn help(args: &[&str]) -> String {
    let o = Command::new(cargo_bin("nephila"))
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn expected_error(config: &Path, received: &str) -> String {
    format!(
        "erreur: configuration invalide ({}) : clé « enrich_code » doit être un booléen, true ou false (reçu : {received})\n",
        config.display()
    )
}

const MOCK: &str = "provider = \"mock\"\n";

#[test]
fn REQ050_AC5_valeur_non_booleenne_refusee_avant_tout_traitement() {
    for bad in ["\"oui\"", "1", "\"true\""] {
        let env = Env::new(&format!("{MOCK}enrich_code = {bad}\n"));
        for args in [
            &["build"][..],
            &["update"][..],
            &["build", "--enrich-code"][..],
        ] {
            let o = env.run(args);
            assert!(!o.status.success(), "{bad} {args:?}");
            assert!(stderr(&o).contains("enrich_code"), "{bad} {args:?}");
            assert!(!env.graph_dir().exists(), "{bad} {args:?}");
        }
        let o = env.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{bad} : {}", stderr(&o));
    }
}

#[test]
fn REQ050_AC5_configuration_invalide_laisse_le_graphe_intact() {
    let env = Env::new(MOCK);
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let db = env.graph_dir().join("graph.db");
    let before = fs::read(&db).unwrap();
    env.write_config(&format!("{MOCK}enrich_code = \"oui\"\n"));
    let o = env.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(before, fs::read(&db).unwrap());
}

#[test]
fn REQ050_AC6_option_sans_effet_sans_llm_ou_sans_configuration() {
    let env = Env::new(MOCK);
    let o = env.run(&["build", "--no-llm", "--enrich-code"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let env = Env::new(MOCK);
    fs::remove_file(&env.config).unwrap();
    let o = env.run(&["build", "--enrich-code"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let o = env.run(&["update", "--enrich-code"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
}

#[test]
fn REQ050_AC6_nephila_toml_refuse_la_cle() {
    let env = Env::new(MOCK);
    fs::write(env.repo.join(".nephila.toml"), "enrich_code = true\n").unwrap();
    let o = env.run(&["build"]);
    assert!(!o.status.success());
    assert!(
        stderr(&o).contains("clé inconnue `enrich_code`"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn REQ050_AC6_option_refusee_par_status_et_ask() {
    let env = Env::new(MOCK);
    assert!(env.run(&["build", "--no-llm"]).status.success());
    for args in [
        &["status", "--enrich-code"][..],
        &["query", "ask", "x", "--enrich-code"][..],
        &["summary", "--enrich-code"][..],
    ] {
        let o = env.run(args);
        assert!(!o.status.success(), "{args:?}");
        assert_eq!(o.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn REQ050_AC6_option_sans_valeur_ni_forme_courte() {
    let env = Env::new(MOCK);
    for args in [
        &["build", "--enrich-code=true"][..],
        &["build", "--enrich-code", "true"][..],
    ] {
        let o = env.run(args);
        assert!(!o.status.success(), "{args:?}");
        assert!(!env.graph_dir().exists(), "{args:?}");
    }
}

#[test]
fn REQ050_AC7_texte_exact_de_l_option() {
    for sub in ["build", "update"] {
        let h = help(&[sub, "--help"]);
        assert!(h.contains(ENTRY), "{sub}\n{h}");
        let from = h.find("--enrich-code").unwrap();
        let rest = &h[from..];
        let entry = &rest[..rest[2..].find(" -").map_or(rest.len(), |i| i + 2)];
        assert!(!entry.contains("[default:"), "{sub} {entry}");
    }
}

#[test]
fn REQ050_AC10_place_dans_l_aide_et_paragraphe_final() {
    for sub in ["build", "update"] {
        let h = help(&[sub, "--help"]);
        let p = |s: &str| h.find(s).unwrap();
        assert!(p("--no-llm") < p("--enrich-code"), "{sub}");
        assert!(p("--enrich-code") < p("-v, --verbose"), "{sub}");
        assert!(!h.contains("<VALEUR>") && !h.contains("[possible values:"));
        let from = h.find("--enrich-code").unwrap();
        let until = h[from..].find("-v, --verbose").unwrap();
        assert!(!h[from..from + until].contains('<'), "{sub}");
        let tail = &h[h.find("Filtres : --exclude").unwrap()..];
        assert!(!tail.contains("enrich_code"), "{sub}");
        assert!(tail.contains("Réglages LLM du dépôt"), "{sub}");
    }
    assert!(!help(&["query", "ask", "--help"]).contains("--enrich-code"));
}

#[test]
fn REQ050_AC8_ligne_config_donne_la_valeur_effective() {
    let env = Env::new(MOCK);
    let o = env.run(&["build", "--verbose"]);
    let l = config_line(&o);
    assert!(
        l.contains("config: llm=mock model=mock enrich_code=false concurrency="),
        "{l}"
    );
    let o = env.run(&["build", "--verbose", "--enrich-code"]);
    assert!(config_line(&o).contains(" model=mock enrich_code=true concurrency="));
    let o = env.run(&["update", "--verbose", "--enrich-code"]);
    assert!(config_line(&o).contains(" enrich_code=true "));
    env.write_config(&format!("{MOCK}enrich_code = true\n"));
    let o = env.run(&["build", "--verbose"]);
    assert!(config_line(&o).contains(" model=mock enrich_code=true concurrency="));
    env.write_config(&format!("{MOCK}enrich_code = false\n"));
    let o = env.run(&["build", "--verbose"]);
    assert!(config_line(&o).contains(" enrich_code=false "));
    assert!(!stderr(&o).contains("verify_key"));
}

#[test]
fn REQ050_AC8_fin_de_ligne_et_fichier_de_projet() {
    let env = Env::new(MOCK);
    let o = env.run(&["build", "--verbose"]);
    assert!(config_line(&o).contains(" batch_files="));
    assert!(config_line(&o).ends_with(" format_retries=2"));
    fs::write(env.repo.join(".nephila.toml"), "model = \"m-projet\"\n").unwrap();
    let o = env.run(&["build", "--verbose", "--enrich-code"]);
    let l = config_line(&o);
    assert!(
        l.contains(" model=m-projet enrich_code=true concurrency="),
        "{l}"
    );
    assert!(l.ends_with(" (.nephila.toml : model)"), "{l}");
}

#[test]
fn REQ050_AC8_sans_llm_ligne_inchangee() {
    let env = Env::new(MOCK);
    let o = env.run(&["build", "--no-llm", "--verbose", "--enrich-code"]);
    assert!(stderr(&o)
        .lines()
        .any(|l| l.ends_with("config: llm inactif (--no-llm)")));
    assert!(!stderr(&o).contains("enrich_code"));
}

#[test]
fn REQ050_AC11_message_exact_de_l_erreur() {
    for (written, shown) in [("\"oui\"", "\"oui\""), ("1", "1"), ("\"true\"", "\"true\"")] {
        let env = Env::new(&format!("{MOCK}enrich_code = {written}\n"));
        for args in [&["build"][..], &["build", "--enrich-code"][..]] {
            let o = env.run(args);
            assert_eq!(stderr(&o), expected_error(&env.config, shown), "{args:?}");
        }
    }
}

#[test]
fn REQ050_AC11_nephila_toml_erreur_e2_exacte() {
    let env = Env::new(MOCK);
    fs::write(env.repo.join(".nephila.toml"), "enrich_code = true\n").unwrap();
    let o = Command::new(cargo_bin("nephila"))
        .arg("build")
        .current_dir(&env.repo)
        .env("NEPHILA_CONFIG", &env.config)
        .output()
        .unwrap();
    assert_eq!(
        stderr(&o),
        "erreur: ./.nephila.toml : clé inconnue `enrich_code` (clés permises : exclude, exclude_types, model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries)\n"
    );
}
