//! Réglages LLM du fichier de projet `.nephila.toml` (REQ-042), partie lecture et validation.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Liste triée (chemin relatif, contenu) de tous les fichiers sous `dir`.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().display().to_string();
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    /// Dépôt `maven2`, configuration `mock`.
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let repo = tmp.path().join("repo");
        copy_dir(&fx.join("maven2"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("mock"), &mock);
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            config,
            mock,
        }
    }

    fn project(&self, content: &str) {
        fs::write(self.repo.join(".nephila.toml"), content).unwrap();
    }

    fn command(&self) -> Command {
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env_remove("NEPHILA_MOCK_MALFORMED");
        c
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.command()
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn shown(&self) -> String {
        format!("{}/.nephila.toml", self.repo.display())
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const COMMANDS: [&[&str]; 5] = [
    &["build"],
    &["update"],
    &["status"],
    &["query", "search", "x"],
    &["query", "ask", "x"],
];

const CONNECTION: [(&str, &str, &str); 5] = [
    ("provider", "provider = \"openai\"", "openai"),
    ("base_url", "base_url = \"http://127.0.0.1:9\"", "127.0.0.1"),
    ("api_key_env", "api_key_env = \"AUTRE\"", "AUTRE"),
    ("tls_ca_cert", "tls_ca_cert = \"ca.pem\"", "ca.pem"),
    ("tls_insecure", "tls_insecure = true", "true"),
];

#[test]
fn req042_ac5_ac14_cle_de_connexion_refusee_sans_effet() {
    let e = Env::new();
    let server = FakeServer::start(vec![]);
    fs::write(
        &e.config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{}\"\n",
            server.base_url
        ),
    )
    .unwrap();
    for (key, line, value) in CONNECTION {
        e.project(&format!("{line}\n"));
        for args in COMMANDS {
            let o = e.run(args);
            assert!(!o.status.success(), "{key} {args:?}");
            assert!(o.stdout.is_empty(), "{key} {args:?}");
            assert_eq!(
                text(&o.stderr),
                format!(
                    "erreur: {} : clé `{key}` permise seulement dans la configuration personnelle ({})\n",
                    e.shown(),
                    e.config.display()
                ),
                "{args:?}"
            );
            assert!(!text(&o.stderr).contains("clé inconnue"));
            if value != key && value != "true" {
                let line = text(&o.stderr);
                let tail = line.split("erreur: ").nth(1).unwrap();
                let tail = tail
                    .replace(&e.shown(), "")
                    .replace(&e.config.display().to_string(), "");
                assert!(!tail.contains(value) || key == "provider", "{tail}");
            }
        }
        assert!(!e.repo.join(".nephila").exists(), "{key}");
    }
    assert!(server.requests().is_empty());
}

#[test]
fn req042_ac5_ac14_avec_no_llm_config_absente_et_journal_mock() {
    let e = Env::new();
    for (key, line, _) in CONNECTION {
        e.project(&format!("{line}\n"));
        let absent = e.config.with_file_name("absent.toml");
        for args in [
            &["build", "--no-llm"][..],
            &["update", "--no-llm"],
            &["status"],
            &["query", "ask", "x"],
            &["query", "search", "x"],
        ] {
            let o = e
                .command()
                .env("NEPHILA_CONFIG", &absent)
                .arg("-C")
                .arg(&e.repo)
                .args(args)
                .output()
                .unwrap();
            assert!(!o.status.success());
            assert_eq!(
                text(&o.stderr),
                format!(
                    "erreur: {} : clé `{key}` permise seulement dans la configuration personnelle ({})\n",
                    e.shown(),
                    absent.display()
                )
            );
        }
        // configuration mock : aucun appel
        let o = e.run(&["build"]);
        assert!(!o.status.success());
        assert!(!e.log.exists(), "journal du mock créé");
        assert!(!e.repo.join(".nephila").exists());
    }
}

#[test]
fn req042_ac5_graphe_existant_inchange_export_et_summary_ok() {
    let e = Env::new();
    assert!(e.run(&["build"]).status.success());
    // export et summary peuvent écrire dans le graphe : un premier passage avant l'instantané
    assert!(e.run(&["summary"]).status.success());
    let before = snapshot(&e.repo.join(".nephila"));
    assert!(!before.is_empty());
    let log_before = fs::read(&e.log).ok();
    for (_, line, _) in CONNECTION {
        e.project(&format!("{line}\n"));
        for args in COMMANDS {
            assert!(!e.run(args).status.success(), "{args:?}");
        }
        assert_eq!(snapshot(&e.repo.join(".nephila")), before);
        assert_eq!(fs::read(&e.log).ok(), log_before);
        assert!(e.run(&["export", "--output", "-"]).status.success());
        assert!(e.run(&["summary"]).status.success());
    }
}

#[test]
fn req042_ac14_sans_c_le_chemin_est_relatif() {
    let e = Env::new();
    e.project("base_url = \"http://127.0.0.1:9\"\n");
    let o = e
        .command()
        .current_dir(&e.repo)
        .arg("status")
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).starts_with("erreur: ./.nephila.toml : clé `base_url` permise seulement"),
        "{}",
        text(&o.stderr)
    );
}

const INVALID: [(&str, &str, &str); 7] = [
    ("model = \"\"", "model", "`\"\"` (chaîne non vide attendue)"),
    ("model = 3", "model", "`3` (chaîne non vide attendue)"),
    (
        "max_chunk_chars = 0",
        "max_chunk_chars",
        "`0` (entier strictement positif attendu)",
    ),
    (
        "concurrency = -1",
        "concurrency",
        "`-1` (entier strictement positif attendu)",
    ),
    (
        "timeout_secs = 1.5",
        "timeout_secs",
        "`1.5` (entier strictement positif attendu)",
    ),
    (
        "batch_files = \"2\"",
        "batch_files",
        "`\"2\"` (entier strictement positif attendu)",
    ),
    (
        "format_retries = -1",
        "format_retries",
        "`-1` (entier positif ou nul attendu, au plus 4294967295)",
    ),
];

#[test]
fn req042_ac6_ac15_reglage_invalide() {
    let e = Env::new();
    let absent = e.config.with_file_name("absent.toml");
    for (line, key, rest) in INVALID {
        e.project(&format!("{line}\n"));
        let expected = format!(
            "erreur: {} : clé `{key}` : valeur invalide {rest}\n",
            e.shown()
        );
        let mut runs: Vec<(Vec<&str>, bool)> =
            COMMANDS.iter().map(|a| (a.to_vec(), false)).collect();
        runs.push((vec!["build", "--no-llm"], false));
        runs.push((vec!["build", "--batch-files", "4"], false));
        runs.push((vec!["build"], true));
        runs.push((vec!["status"], true));
        runs.push((vec!["query", "ask", "x"], true));
        runs.push((vec!["query", "search", "x"], true));
        for (args, no_config) in runs {
            let mut c = e.command();
            if no_config {
                c.env("NEPHILA_CONFIG", &absent);
            }
            let o = c.arg("-C").arg(&e.repo).args(&args).output().unwrap();
            assert!(!o.status.success(), "{args:?}");
            assert!(o.stdout.is_empty(), "{args:?}");
            assert_eq!(text(&o.stderr), expected, "{args:?}");
        }
        assert!(!e.repo.join(".nephila").exists());
        assert!(!e.log.exists());
    }
}

#[test]
fn req042_ac6_filtres_seuls_comportement_inchange() {
    let e = Env::new();
    e.project("exclude_types = [\"json\"]\n");
    let o = e.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
}

#[test]
fn req042_reglages_valides_acceptes_et_sans_effet() {
    let e = Env::new();
    e.project("model = \"m\"\nmax_chunk_chars = 10\nconcurrency = 2\ntimeout_secs = 5\nbatch_files = 2\nformat_retries = 0\n");
    for args in [
        &["build", "--no-llm"][..],
        &["status"],
        &["query", "search", "x"],
    ] {
        let o = e.run(args);
        let err = text(&o.stderr);
        assert!(!err.contains(".nephila.toml"), "{args:?} {err}");
        assert!(!err.contains("avertissement:"), "{args:?} {err}");
        assert!(o.status.success() || args[0] == "query", "{args:?} {err}");
    }
}

#[test]
fn req037_ac18_aide_et_cle_proche_inconnue() {
    let e = Env::new();
    for sub in ["build", "update"] {
        let o = e.run(&[sub, "--help"]);
        let help = text(&o.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let want = "Réglages LLM du dépôt, dans le même fichier (prioritaires sur la configuration personnelle) : model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries";
        let i = help.find(want).unwrap_or_else(|| panic!("{help}"));
        assert!(help[..i].contains("exclude_types = [\"json\"]"));
        assert!(help[i..].contains("Exemple : nephila build"));
        assert!(help.find("Exemple : nephila build").unwrap() > i);
    }
    e.project("modele = \"x\"\n");
    let o = e.run(&["status"]);
    assert_eq!(
        text(&o.stderr),
        format!(
            "erreur: {} : clé inconnue `modele` (clés permises : exclude, exclude_types, model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries)\n",
            e.shown()
        )
    );
}

#[test]
fn req042_ac5_query_ask_ne_contacte_pas_le_serveur() {
    let e = Env::new();
    assert!(e.run(&["build"]).status.success());
    let server = FakeServer::start(vec![]);
    fs::write(
        &e.config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{}\"\n",
            server.base_url
        ),
    )
    .unwrap();
    e.project("concurrency = 0\n");
    let o = e.run(&["query", "ask", "x"]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("clé `concurrency` : valeur invalide `0`"));
    assert!(server.requests().is_empty());
}
