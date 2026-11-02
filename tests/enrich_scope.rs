//! Clés `enrich_include` et `enrich_exclude` (REQ-206, T-298) : lecture, erreurs, plafond, E-2.
#![allow(non_snake_case)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;

const MOCK: &str = "provider = \"mock\"\n";

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
        fs::write(repo.join("README.md"), "# Titre\n\nTexte.\n").unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, config).unwrap();
        Env {
            _tmp: tmp,
            repo,
            config: config_path,
        }
    }

    /// Lancé depuis le dépôt : le fichier de projet s'affiche `./.nephila.toml`.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .args(args)
            .current_dir(&self.repo)
            .env("NEPHILA_CONFIG", &self.config)
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn project(&self, text: &str) {
        fs::write(self.repo.join(".nephila.toml"), text).unwrap();
    }

    fn graph_dir(&self) -> PathBuf {
        self.repo.join(".nephila")
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn personal_line(env: &Env, key: &str, got: &str) -> String {
    format!(
        "erreur: configuration invalide ({}) : clé « {key} » doit être un tableau de motifs valides (reçu : {got})\n",
        env.config.display()
    )
}

#[test]
fn REQ206_AC3_AC6_erreurs_de_la_configuration_personnelle() {
    let cases = [
        ("enrich_exclude = \"a\"", "enrich_exclude", "\"a\""),
        ("enrich_exclude = [1]", "enrich_exclude", "[1]"),
        ("enrich_include = [\"a[b\"]", "enrich_include", "\"a[b\""),
        (
            "enrich_include = [\"src/**\", \"a[b\"]",
            "enrich_include",
            "\"a[b\"",
        ),
    ];
    for (extra, key, got) in cases {
        let env = Env::new(&format!("{MOCK}{extra}\n"));
        let o = env.run(&["build"]);
        assert_eq!(o.status.code(), Some(1), "{extra}");
        assert_eq!(stderr(&o), personal_line(&env, key, got), "{extra}");
        assert!(!env.graph_dir().exists(), "{extra}");
        let o = env.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{extra} : {}", stderr(&o));
        assert!(!stderr(&o).contains("erreur:"), "{extra}");
    }
}

#[test]
fn REQ206_AC4_AC7_erreurs_du_fichier_de_projet() {
    let many = |n: usize| format!("enrich_exclude = [{}]\n", "\"a\", ".repeat(n));
    let mut with_invalid = many(100);
    with_invalid = with_invalid.replace("\"a\", ]", "\"a[b\", \"a\"]");
    let cases = [
        (
            "enrich_include = \"a\"\n".to_string(),
            "erreur: ./.nephila.toml : clé `enrich_include` : tableau de chaînes attendu, ex. enrich_include = [\"src/**\"]\n".to_string(),
        ),
        (
            "enrich_exclude = \"a\"\n".to_string(),
            "erreur: ./.nephila.toml : clé `enrich_exclude` : tableau de chaînes attendu, ex. enrich_exclude = [\"legacy/\"]\n".to_string(),
        ),
        (
            "enrich_exclude = [1]\n".to_string(),
            "erreur: ./.nephila.toml : clé `enrich_exclude` : tableau de chaînes attendu, ex. enrich_exclude = [\"legacy/\"]\n".to_string(),
        ),
        (
            many(101),
            "erreur: ./.nephila.toml : clé `enrich_exclude` : trop de motifs `101` (au plus 100)\n".to_string(),
        ),
        (
            with_invalid,
            "erreur: ./.nephila.toml : clé `enrich_exclude` : trop de motifs `101` (au plus 100)\n".to_string(),
        ),
    ];
    for (text, expected) in cases {
        for args in [&["build"][..], &["build", "--no-llm"][..]] {
            let env = Env::new(MOCK);
            env.project(&text);
            let o = env.run(args);
            assert_eq!(o.status.code(), Some(1), "{args:?}");
            assert_eq!(stderr(&o), expected, "{args:?}");
            assert!(!env.graph_dir().exists());
        }
    }
    for (key, args) in [
        ("enrich_exclude", &["build"][..]),
        ("enrich_exclude", &["build", "--no-llm"][..]),
        ("enrich_include", &["build"][..]),
        ("enrich_include", &["build", "--no-llm"][..]),
    ] {
        let env = Env::new(MOCK);
        env.project(&format!("{key} = [\"a[b\"]\n"));
        let o = env.run(args);
        assert_eq!(o.status.code(), Some(1), "{key} {args:?}");
        let err = stderr(&o);
        assert!(
            err.starts_with(&format!(
                "erreur: ./.nephila.toml : clé `{key}` : motif invalide `a[b` ("
            )),
            "{err}"
        );
        assert!(
            err.ends_with(")\n") && err.matches('\n').count() == 1,
            "{err}"
        );
        assert!(!env.graph_dir().exists(), "{key} {args:?}");
    }
    // 100 motifs : accepté.
    for args in [&["build"][..], &["build", "--no-llm"][..]] {
        let env = Env::new(MOCK);
        env.project(&many(100));
        let o = env.run(args);
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    }
}

const E2_KEYS: &str = "exclude, exclude_types, exclude_defaults, enrich_include, enrich_exclude, model, summary_language, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries, ask";

#[test]
fn REQ206_AC5_AC7_liste_des_cles_permises_d_e2() {
    let env = Env::new(MOCK);
    env.project("zz = 1\n");
    let o = env.run(&["build"]);
    assert_eq!(
        stderr(&o),
        format!("erreur: ./.nephila.toml : clé inconnue `zz` (clés permises : {E2_KEYS})\n")
    );
    // Les deux clés sont permises dans le fichier de projet.
    env.project("enrich_include = [\"src/**\"]\nenrich_exclude = [\"legacy/\"]\n");
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
}

#[test]
fn REQ206_AC7_section_ask_avec_enrich_exclude_garde_ses_sept_cles() {
    let env = Env::new(MOCK);
    env.project("[ask]\nenrich_exclude = [\"a\"]\n");
    let o = env.run(&["build"]);
    assert_eq!(
        stderr(&o),
        "erreur: ./.nephila.toml : clé inconnue `ask.enrich_exclude` (clés permises dans [ask] : model, thinking, effort, max_output_tokens, max_context_chars, max_seeds, max_nodes)\n"
    );
}
