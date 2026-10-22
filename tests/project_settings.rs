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
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
        )
        .unwrap();
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
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\n",
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

const INVALID: [(&str, &str, &str); 14] = [
    ("model = \"\"", "model", "`\"\"` (chaîne non vide attendue)"),
    ("model = 3", "model", "`3` (chaîne non vide attendue)"),
    (
        "max_chunk_chars = 0",
        "max_chunk_chars",
        "`0` (entier strictement positif attendu, au plus 1000000)",
    ),
    (
        "concurrency = -1",
        "concurrency",
        "`-1` (entier strictement positif attendu, au plus 64)",
    ),
    (
        "timeout_secs = 1.5",
        "timeout_secs",
        "`1.5` (entier strictement positif attendu, au plus 3600)",
    ),
    (
        "batch_files = \"2\"",
        "batch_files",
        "`\"2\"` (entier strictement positif attendu, au plus 100)",
    ),
    (
        "format_retries = -1",
        "format_retries",
        "`-1` (entier positif ou nul attendu, au plus 10)",
    ),
    (
        "concurrency = 65",
        "concurrency",
        "`65` (entier strictement positif attendu, au plus 64)",
    ),
    (
        "timeout_secs = 3601",
        "timeout_secs",
        "`3601` (entier strictement positif attendu, au plus 3600)",
    ),
    (
        "format_retries = 11",
        "format_retries",
        "`11` (entier positif ou nul attendu, au plus 10)",
    ),
    (
        "max_chunk_chars = 1000001",
        "max_chunk_chars",
        "`1000001` (entier strictement positif attendu, au plus 1000000)",
    ),
    (
        "batch_files = 101",
        "batch_files",
        "`101` (entier strictement positif attendu, au plus 100)",
    ),
    (
        "max_chunk_chars = 1_000_001",
        "max_chunk_chars",
        "`1000001` (entier strictement positif attendu, au plus 1000000)",
    ),
    (
        "format_retries = 50",
        "format_retries",
        "`50` (entier positif ou nul attendu, au plus 10)",
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
        runs.push((vec!["build", "--format-retries", "2"], false));
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
            assert!(!text(&o.stderr).contains("4294967295"), "{args:?}");
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
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\n",
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

// ---- T-087 : application des réglages dans build, update et query ask ----

const KEY_VAR: &str = "NEPHILA_T087_KEY";
const ENRICH_A: &str =
    r#"{"version":2,"summaries":[{"node":"type:p.Alpha","summary":"résumé A"}],"concepts":[]}"#;
const ASK_A: &str = r#"{"found":true,"answer":"réponse","nodes":["type:p.Alpha"]}"#;

fn http_reply(provider: &str, text: &str) -> String {
    if provider == "anthropic" {
        serde_json::json!({"content":[{"type":"text","text":text}]}).to_string()
    } else {
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string()
    }
}

impl Env {
    /// Dépôt réduit à `A.java` et configuration personnelle `provider` pointée sur `server`.
    fn http(provider: &str, server_url: &str, extra: &str) -> Env {
        let e = Env::new();
        fs::remove_dir_all(&e.repo).unwrap();
        fs::create_dir_all(&e.repo).unwrap();
        fs::write(
            e.repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
        fs::write(
            &e.config,
            format!(
                "provider = \"{provider}\"\nenrich_code = true\nmodel = \"m-perso\"\nbase_url = \"{server_url}\"\napi_key_env = \"{KEY_VAR}\"\n{extra}"
            ),
        )
        .unwrap();
        e
    }

    fn run_key(&self, args: &[&str]) -> std::process::Output {
        self.command()
            .env(KEY_VAR, "sk-test")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    /// `n` fichiers `src/Fnn.java` et leurs fixtures, config `mock` donnée.
    fn small(&self, n: usize, config: &str) {
        fs::remove_dir_all(&self.repo).unwrap();
        fs::remove_dir_all(&self.mock).unwrap();
        fs::create_dir_all(self.repo.join("src")).unwrap();
        fs::create_dir_all(self.mock.join("src")).unwrap();
        for i in 0..n {
            fs::write(
                self.repo.join(format!("src/F{i:02}.java")),
                format!("package p;\npublic class F{i:02} {{}}\n"),
            )
            .unwrap();
            fs::write(
                self.mock.join(format!("src/F{i:02}.java.json")),
                format!(
                    r#"{{"version":2,"summaries":[{{"node":"type:p.F{i:02}","summary":"résumé {i:02}"}}],"concepts":[]}}"#
                ),
            )
            .unwrap();
        }
        fs::write(&self.config, config).unwrap();
        let _ = fs::remove_file(&self.log);
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn to_enrich(&self) -> usize {
        nephila::store::Store::open_readonly(&self.repo)
            .unwrap()
            .files_to_enrich()
            .unwrap()
            .len()
    }
}

fn posts(server: &FakeServer) -> Vec<fake_server::Recorded> {
    server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect()
}

#[test]
fn req042_ac1_modele_du_fichier_de_projet_dans_les_requetes() {
    for provider in ["openai", "anthropic"] {
        for (project, expected) in [("model = \"m-projet\"\n", "m-projet"), ("", "m-perso")] {
            let server = FakeServer::start(vec![
                (200, http_reply(provider, ENRICH_A)),
                (200, http_reply(provider, ASK_A)),
            ]);
            let e = Env::http(provider, &server.base_url, "");
            if !project.is_empty() {
                e.project(project);
            }
            let o = e.run_key(&["build"]);
            assert!(o.status.success(), "{provider}: {}", text(&o.stderr));
            let o = e.run_key(&["query", "ask", "Alpha"]);
            assert!(o.status.success(), "{provider}: {}", text(&o.stderr));
            let p = posts(&server);
            assert_eq!(p.len(), 2, "{provider}: enrichissement et ask");
            let path = if provider == "openai" {
                "/v1/chat/completions"
            } else {
                "/v1/messages"
            };
            for r in &p {
                assert_eq!(r.path, path);
                let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
                assert_eq!(body["model"], expected, "{provider}: {}", r.body);
            }
        }
    }
}

#[test]
fn req042_ac3_essais_de_format_et_lots_du_fichier_de_projet() {
    let e = Env::new();
    let personal = "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nconcurrency = 1\nformat_retries = 0\n";
    e.small(1, personal);
    e.project("format_retries = 3\n");
    let malformed = [("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")];
    let run = |args: &[&str]| {
        let _ = fs::remove_file(&e.log);
        e.command()
            .envs(malformed)
            .arg("-C")
            .arg(&e.repo)
            .args(args)
            .output()
            .unwrap()
    };
    let o = run(&["build"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(e.log_lines().len(), 4);
    assert_eq!(e.to_enrich(), 0);
    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(e.repo.join(".nephila")).unwrap();
    let o = run(&["build", "--format-retries", "0"]);
    assert!(!o.status.success());
    assert_eq!(e.log_lines().len(), 1);
    assert_eq!(e.to_enrich(), 1);

    // query ask
    fs::write(e.mock.join("_ask.json"), ASK_A.replace("p.Alpha", "p.F00")).unwrap();
    assert!(run(&["build"]).status.success());
    let ask = |extra: &[&str]| {
        let mut a = vec!["query", "ask", "F00"];
        a.extend_from_slice(extra);
        e.command()
            .env("NEPHILA_MOCK_MALFORMED", "_ask:3")
            .arg("-C")
            .arg(&e.repo)
            .args(a)
            .output()
            .unwrap()
    };
    let o = ask(&[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let o = ask(&["--format-retries", "0"]);
    assert!(!o.status.success());

    // lots
    e.small(
        4,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nconcurrency = 1\nbatch_files = 1\n",
    );
    e.project("batch_files = 2\nconcurrency = 1\n");
    assert!(e.run(&["build"]).status.success());
    let log = e.log_lines();
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(log
        .iter()
        .all(|l| l.split('\t').next().unwrap().split(';').count() == 2));
    let _ = fs::remove_file(&e.log);
    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(e.repo.join(".nephila")).unwrap();
    assert!(e.run(&["build", "--batch-files", "1"]).status.success());
    assert_eq!(e.log_lines().len(), 4);
}

#[test]
fn req042_ac4_fils_morceaux_et_delai() {
    let e = Env::new();
    e.small(
        4,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nconcurrency = 1\ntimeout_secs = 300\nmax_chunk_chars = 60000\n",
    );
    fs::write(
        e.repo.join("src/F02.java"),
        format!(
            "package p;\npublic class F02 {{}}\n{}",
            "// remplissage\n".repeat(100)
        ),
    )
    .unwrap();
    e.project("concurrency = 3\ntimeout_secs = 7\nmax_chunk_chars = 1000\n");
    let o = e.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    let config = err.lines().find(|l| l.contains("config: llm=")).unwrap();
    for needle in ["concurrency=3", "timeout_secs=7", "max_chunk_chars=1000"] {
        assert!(config.contains(needle), "{config}");
    }
    assert!(err.contains("structure: début 4 fichiers, 3 fils"), "{err}");
    let enrich = err
        .lines()
        .find(|l| l.contains("enrichissement: début"))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(enrich.trim_end().ends_with(", 3 fils"), "{enrich}");
    let own = e
        .log_lines()
        .into_iter()
        .filter(|l| l.starts_with("src/F02.java\t"))
        .count();
    assert!(own > 1, "{:?}", e.log_lines());

    // délai
    let server = FakeServer::start_with_delays(vec![
        (200, "{}".into(), 0),
        (200, http_reply("openai", ENRICH_A), 2000),
    ]);
    let e = Env::http("openai", &server.base_url, "timeout_secs = 300\n");
    e.project("timeout_secs = 1\n");
    let o = e.run_key(&["build", "-v"]);
    assert!(
        text(&o.stderr).contains("délai dépassé"),
        "{}",
        text(&o.stderr)
    );
}

const SIX: &str = "model = \"x\"\nmax_chunk_chars = 1000\nconcurrency = 2\ntimeout_secs = 5\nbatch_files = 2\nformat_retries = 1\n";

#[test]
fn req042_ac7_llm_inactif_reglages_sans_effet() {
    let e = Env::new();
    let absent = e.config.with_file_name("absent.toml");
    let run = |args: &[&str]| {
        e.command()
            .env("NEPHILA_CONFIG", &absent)
            .arg("-C")
            .arg(&e.repo)
            .args(args)
            .output()
            .unwrap()
    };
    e.project(SIX);
    let o = run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(text(&o.stdout).contains("enrichissement : désactivé"));
    assert!(
        err.lines()
            .any(|l| l.ends_with("] config: llm inactif (aucune configuration)")),
        "{err}"
    );
    assert!(
        err.contains("structure: début") && err.contains(", 4 fils"),
        "{err}"
    );
    assert!(!err.contains("(.nephila.toml :"));
    assert!(
        !err.contains("avertissement:") && !err.contains("erreur:"),
        "{err}"
    );
    fs::write(
        e.repo.join("core/Extra.java"),
        "package p;\nclass Extra {}\n",
    )
    .unwrap();
    let o = run(&["update"]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(text(&o.stdout).contains("enrichissement : désactivé"));
    assert!(
        !err.contains("avertissement:") && !err.contains("erreur:"),
        "{err}"
    );
    assert!(!e.log.exists());

    // --no-llm avec une configuration personnelle
    let o = e.run(&["build", "--no-llm", "--verbose"]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.lines()
            .any(|l| l.ends_with("] config: llm inactif (--no-llm)")),
        "{err}"
    );
    assert!(!err.contains("(.nephila.toml :"));
    assert!(!e.log.exists());

    // ask sans configuration personnelle
    let o = run(&["query", "ask", "x"]);
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains(&absent.display().to_string()),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn req042_ac8_configuration_personnelle_validee_seule() {
    let e = Env::new();
    fs::write(&e.config, "provider = \"openai\"\nenrich_code = true\n").unwrap();
    e.project("model = \"x\"\n");
    let o = e.run(&["build"]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("model"), "{}", text(&o.stderr));
    assert!(!e.repo.join(".nephila").exists());

    fs::write(
        &e.config,
        "provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 0\n",
    )
    .unwrap();
    e.project("concurrency = 2\n");
    let o = e.run(&["build"]);
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains("concurrency"),
        "{}",
        text(&o.stderr)
    );
    assert!(!e.repo.join(".nephila").exists());
}

#[test]
fn req042_ac9_changer_de_modele_reenrichit_une_fois_les_autres_reglages_rien() {
    let e = Env::new();
    fs::write(
        &e.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\n",
    )
    .unwrap();
    let o = e.run(&["build"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let sent = |e: &Env| -> Vec<String> {
        let mut v: Vec<String> = fs::read_to_string(&e.log)
            .unwrap_or_default()
            .lines()
            .map(|l| l.split('\t').next().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    let first = sent(&e);
    assert!(!first.is_empty());

    // Autres réglages : rien n'est envoyé.
    let _ = fs::remove_file(&e.log);
    e.project("format_retries = 3\nconcurrency = 1\nbatch_files = 2\n");
    let o = e.run(&["update"]);
    assert!(o.status.success());
    assert_eq!(text(&o.stdout), "graphe à jour\n");
    assert!(sent(&e).is_empty());

    // Modèle : status et recherche inchangés, update envoie chaque fichier une fois.
    e.project("model = \"autre\"\nformat_retries = 3\nconcurrency = 1\nbatch_files = 1\n");
    let o = e.run(&["status"]);
    assert!(
        text(&o.stdout).contains("graphe à jour"),
        "{}",
        text(&o.stdout)
    );
    let o = e.run(&["query", "search", "x"]);
    assert!(
        !text(&o.stderr).contains("pas à jour"),
        "{}",
        text(&o.stderr)
    );
    let _ = fs::remove_file(&e.log);
    let o = e.run(&["update"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(sent(&e), first);
    let _ = fs::remove_file(&e.log);
    let o = e.run(&["update"]);
    assert!(o.status.success());
    assert_eq!(text(&o.stdout), "graphe à jour\n");
    assert!(sent(&e).is_empty());
}

#[test]
fn req042_ac11_filtres_et_reglages_combines() {
    let e = Env::new();
    e.project("exclude = [\"docs/legacy/\"]\nexclude_types = [\"json\"]\nmodel = \"m-projet\"\nformat_retries = 1\n");
    let o = e.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(text(&o.stdout).contains("fichiers exclus par les filtres"));
    let config = err.lines().find(|l| l.contains("config: llm=")).unwrap();
    assert!(
        config.contains("model=m-projet") && config.contains("format_retries=1"),
        "{config}"
    );
    assert!(!err.contains("valent pour cette commande"), "{err}");
}

#[test]
fn req042_ac16_aucune_trace_sans_verbose() {
    let with = Env::new();
    with.project("model = \"m-projet\"\nformat_retries = 3\n");
    let without = Env::new();
    let a = with.run(&["build"]);
    let b = without.run(&["build"]);
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
    assert_eq!(a.status.code(), b.status.code());
    let err = text(&a.stderr);
    assert!(
        !err.contains(".nephila.toml") && !err.contains("m-projet"),
        "{err}"
    );
}

const CAPS: &str = "max_chunk_chars = 1000000\nconcurrency = 64\ntimeout_secs = 3600\nbatch_files = 100\nformat_retries = 10\n";

fn config_line(err: &str) -> String {
    err.lines()
        .find(|l| l.contains("] config:"))
        .unwrap_or_else(|| panic!("{err}"))
        .to_string()
}

#[test]
fn req042_ac18_reglages_au_plafond_acceptes() {
    let e = Env::new();
    e.project(CAPS);
    let o = e.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let line = config_line(&err);
    assert!(
        line.contains(
            "concurrency=64 timeout_secs=3600 max_chunk_chars=1000000 batch_files=100 format_retries=10"
        ),
        "{line}"
    );
}

#[test]
fn req042_ac19_bornes_personnelles_et_options_inchangees() {
    let e = Env::new();
    let mut personal = String::from("provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n");
    let mut wants = Vec::new();
    for (key, cap) in nephila::config::PROJECT_CAPS {
        personal.push_str(&format!("{key} = {}\n", cap + 1));
        wants.push(format!("{key}={}", cap + 1));
    }
    fs::write(&e.config, personal).unwrap();
    let o = e.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let line = config_line(&err);
    for want in &wants {
        assert!(line.contains(want.as_str()), "{want} {line}");
    }
    assert!(!err.contains("erreur:"), "{err}");

    let e = Env::new();
    fs::write(
        &e.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nformat_retries = 4294967295\n",
    )
    .unwrap();
    let o = e.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let line = config_line(&err);
    assert!(line.contains("format_retries=4294967295"), "{line}");

    let cap = |k: &str| nephila::config::project_cap(k).unwrap() + 1;
    let (retries, batch) = (cap("format_retries"), cap("batch_files"));
    let e = Env::new();
    e.project("format_retries = 3\nbatch_files = 2\n");
    let o = e.run(&[
        "build",
        "--verbose",
        "--format-retries",
        &retries.to_string(),
        "--batch-files",
        &batch.to_string(),
    ]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let line = config_line(&err);
    assert!(
        line.contains(&format!("batch_files={batch} format_retries={retries}")),
        "{line}"
    );
}

#[test]
fn req042_ac22_valeurs_egales_au_plafond_et_sans_couleur() {
    let e = Env::new();
    for line in ["concurrency = 64", "max_chunk_chars = 1_000_000"] {
        e.project(&format!("{line}\n"));
        let o = e.run(&["build", "--no-llm"]);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{line} {err}");
        assert!(!err.lines().any(|l| l.starts_with("erreur:")), "{err}");
    }
    e.project("concurrency = 65\n");
    let plain = e
        .command()
        .env_remove("NO_COLOR")
        .arg("-C")
        .arg(&e.repo)
        .args(["build", "--no-llm"])
        .output()
        .unwrap();
    let no_color = e
        .command()
        .env("NO_COLOR", "1")
        .arg("-C")
        .arg(&e.repo)
        .args(["build", "--no-llm"])
        .output()
        .unwrap();
    assert!(!plain.status.success());
    assert_eq!(plain.stderr, no_color.stderr);
    assert!(!plain.stderr.contains(&0x1B));
    assert!(text(&plain.stderr).contains("valeur invalide `65`"));
}
