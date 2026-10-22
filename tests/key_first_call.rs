//! Clé d'API constatée au premier appel, sans vérification préalable (REQ-054, DEC-065).

#[path = "support/fake_server.rs"]
#[allow(dead_code)]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use fake_server::FakeServer;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
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

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    /// Dépôt vide, configuration `mock` (lots de 10, un seul appel à la fois).
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            mock: tmp.path().join("mock"),
            config: tmp.path().join("config.toml"),
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
        };
        fs::create_dir_all(&env.repo).unwrap();
        fs::create_dir_all(&env.mock).unwrap();
        env.config_mock(1, 10);
        env
    }

    fn config_mock(&self, concurrency: usize, batch: usize) {
        fs::write(
            &self.config,
            format!("provider = \"mock\"\nmodel = \"mock\"\nconcurrency = {concurrency}\nbatch_files = {batch}\n"),
        )
        .unwrap();
    }

    fn config_http(&self, provider: &str, url: &str, concurrency: usize, batch: usize) {
        fs::write(
            &self.config,
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"NEPHILA_T110_KEY\"\nbase_url = \"{url}\"\nconcurrency = {concurrency}\nbatch_files = {batch}\n"
            ),
        )
        .unwrap();
    }

    fn file(&self, path: &str, content: &str) {
        let p = self.repo.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Dépôt exemple (`tests/fixtures/maven2`) et ses fixtures du mock.
    fn example() -> Env {
        let env = Env::new();
        let fx = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir(&fx.join("maven2"), &env.repo);
        copy_dir(&fx.join("mock"), &env.mock);
        env
    }

    /// 3 documents Markdown et 2 fichiers Java.
    fn five_files(&self) {
        for i in 1..=3 {
            self.file(
                &format!("docs/d{i}.md"),
                &format!("# Titre {i}\n\nTexte {i}.\n"),
            );
        }
        for name in ["Alpha", "Beta"] {
            self.file(
                &format!("src/{name}.java"),
                &format!("package p;\npublic class {name} {{}}\n"),
            );
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).args(args);
        for v in [
            "NEPHILA_MOCK_FAIL_AFTER",
            "NEPHILA_MOCK_DELAY_MS",
            "NEPHILA_MOCK_INVALID_KEY",
            "NEPHILA_MOCK_TIMEOUT_FILES",
            "NEPHILA_MOCK_KEY_REVOKED_AFTER",
        ] {
            c.env_remove(v);
        }
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env("NEPHILA_T110_KEY", "sk-test");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }
}

fn error_lines(o: &Output) -> Vec<String> {
    text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("erreur:"))
        .map(String::from)
        .collect()
}

fn anthropic_ok(reply: &str) -> String {
    serde_json::json!({"content":[{"type":"text","text":reply}]}).to_string()
}

fn openai_ok(reply: &str) -> String {
    serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":reply}}]})
        .to_string()
}

const ASK_REPLY: &str = r#"{"found":true,"answer":"ok","nodes":["Alpha"]}"#;

#[test]
fn req054_ac1_no_models_request_openai_build() {
    let env = Env::new();
    env.file("docs/a.md", "# A\n\ntexte\n");
    let server = FakeServer::start(vec![(
        200,
        openai_ok(r#"{"version":2,"summaries":[],"concepts":[]}"#),
    )]);
    env.config_http("openai", &server.base_url, 1, 10);
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let reqs = server.requests();
    assert!(!reqs.is_empty(), "{}", text(&o.stderr));
    assert!(reqs.iter().all(|r| r.path != "/v1/models"), "{reqs:?}");
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/v1/chat/completions");
}

#[test]
fn req054_ac1_no_models_request_anthropic_build() {
    let env = Env::new();
    env.file("docs/a.md", "# A\n\ntexte\n");
    let server = FakeServer::start(vec![(
        200,
        anthropic_ok(r#"{"version":2,"summaries":[],"concepts":[]}"#),
    )]);
    env.config_http("anthropic", &server.base_url, 1, 10);
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let reqs = server.requests();
    assert!(!reqs.is_empty(), "{}", text(&o.stderr));
    assert!(reqs.iter().all(|r| r.path != "/v1/models"), "{reqs:?}");
    assert_eq!(reqs[0].path, "/v1/messages");
}

#[test]
fn req054_ac1_ask_single_request_and_update_none() {
    let env = Env::new();
    env.file("src/Alpha.java", "package p;\npublic class Alpha {}\n");
    let o = env.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    for (provider, ok) in [
        ("openai", openai_ok(ASK_REPLY)),
        ("anthropic", anthropic_ok(ASK_REPLY)),
    ] {
        let server = FakeServer::start(vec![(200, ok)]);
        env.config_http(provider, &server.base_url, 1, 10);
        let o = env.run(&["query", "ask", "Alpha"]);
        assert!(o.status.success(), "{provider}: {}", text(&o.stderr));
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "{provider}: {reqs:?}");
        assert_eq!(reqs[0].method, "POST");
    }
}

#[test]
fn req054_ac1_update_without_change_no_request() {
    let env = Env::new();
    env.file("docs/a.md", "# A\n\ntexte\n");
    // Même modèle (« m ») pour le build et l'update : un autre modèle ferait réenrichir (REQ-049).
    fs::write(
        &env.config,
        "provider = \"mock\"\nmodel = \"m\"\nconcurrency = 1\nbatch_files = 10\n",
    )
    .unwrap();
    assert!(env.run(&["build"]).status.success());
    let server = FakeServer::start(vec![]);
    env.config_http("openai", &server.base_url, 1, 10);
    let o = env.run(&["update"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(server.requests().is_empty());
}

fn refused_scenario(provider: &str, status: u16, extra: &[&str]) -> (Env, FakeServer, Output) {
    let env = Env::new();
    env.five_files();
    let server = FakeServer::start(vec![(status, "{}".into()); 6]);
    env.config_http(provider, &server.base_url, 1, 10);
    let mut args = vec!["build"];
    args.extend_from_slice(extra);
    let o = env.run(&args);
    (env, server, o)
}

#[test]
fn req054_ac2_refused_key_one_request_graph_kept() {
    for (provider, code) in [("openai", 401u16), ("anthropic", 403)] {
        let (env, server, o) = refused_scenario(provider, code, &[]);
        assert_eq!(server.requests().len(), 1, "{provider}");
        assert_eq!(o.status.code(), Some(1), "{provider}");
        let expected = format!(
            "erreur: clé d'API refusée par {provider} (HTTP {code}) ; vérifiez la variable désignée par api_key_env"
        );
        assert_eq!(error_lines(&o), vec![expected], "{}", text(&o.stderr));
        let out = text(&o.stdout);
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.starts_with("build terminé"), "{out}");
        for q in ["Alpha", "Beta", "titre-1", "titre-2", "titre-3"] {
            let s = env.run(&["query", "search", q]);
            assert!(
                text(&s.stdout).contains(q),
                "{provider} {q}: {}",
                text(&s.stdout)
            );
        }
        let st = env.run(&["status", "--json"]);
        let v: serde_json::Value = serde_json::from_slice(&st.stdout).unwrap();
        // Avant REQ-050 (T-118), les 5 fichiers sont à enrichir.
        assert_eq!(v["to_enrich"], 5, "{v}");
    }
}

#[test]
fn req054_ac3_ask_refused_key() {
    let env = Env::new();
    env.file("src/Alpha.java", "package p;\npublic class Alpha {}\n");
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let server = FakeServer::start(vec![(401, "{}".into()); 3]);
    env.config_http("openai", &server.base_url, 1, 10);
    let o = env.run(&["query", "ask", "Alpha"]);
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        text(&o.stderr).trim_end(),
        "erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env"
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn req054_ac4_mock_invalid_key() {
    let env = Env::example();
    let o = env
        .cmd(&["build"])
        .env("NEPHILA_MOCK_INVALID_KEY", "1")
        .output()
        .unwrap();
    assert_eq!(env.log_lines().len(), 1, "{:?}", env.log_lines());
    let errs = error_lines(&o);
    assert_eq!(errs.len(), 1, "{}", text(&o.stderr));
    assert!(errs[0].contains("clé d'API refusée"), "{errs:?}");
    assert!(!errs[0].contains("envoi interrompu"), "{errs:?}");
    assert_eq!(o.status.code(), Some(1));
    assert!(env.repo.join(".nephila").is_dir());
    let s = env.run(&["query", "search", "OrderService"]);
    assert!(text(&s.stdout).contains("OrderService"));
    // `query ask` : un seul appel `_ask`, refusé.
    fs::write(&env.log, "").unwrap();
    let o = env
        .cmd(&["query", "ask", "OrderService"])
        .env("NEPHILA_MOCK_INVALID_KEY", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("clé d'API refusée"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(env.log_lines(), vec!["_ask\t1".to_string()]);
}

#[test]
fn req054_ac5_mock_key_revoked_after_message_as_is() {
    let env = Env::new();
    for i in 1..=4 {
        env.file(&format!("docs/d{i}.md"), &format!("# T{i}\n\nTexte.\n"));
    }
    env.config_mock(1, 1);
    let o = env
        .cmd(&["build"])
        .env("NEPHILA_MOCK_KEY_REVOKED_AFTER", "2")
        .output()
        .unwrap();
    assert_eq!(env.log_lines().len(), 3, "{:?}", env.log_lines());
    let errs = error_lines(&o);
    assert_eq!(errs.len(), 1, "{}", text(&o.stderr));
    assert!(
        errs[0].contains("clé d'API refusée par le fournisseur (LLM simulé)"),
        "{errs:?}"
    );
    assert!(!errs[0].contains("envoi interrompu"), "{errs:?}");
    // AC-054-9 : aucune ligne `avertissement:` ne porte le refus.
    assert!(!text(&o.stderr)
        .lines()
        .any(|l| l.starts_with("avertissement:") && l.contains("clé d'API refusée")));
}

#[test]
fn req054_ac7_missing_key_variable_before_anything() {
    let env = Env::new();
    env.five_files();
    let server = FakeServer::start(vec![]);
    env.config_http("anthropic", &server.base_url, 1, 10);
    let o = env
        .cmd(&["build"])
        .env_remove("NEPHILA_T110_KEY")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(!env.repo.join(".nephila").exists());
    assert!(server.requests().is_empty());
    assert!(
        text(&o.stderr).contains("NEPHILA_T110_KEY"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn req054_ac8_verbose_no_verify_key_and_refusal_lines() {
    let env = Env::example();
    let o = env.run(&["build", "--verbose"]);
    assert!(
        !text(&o.stderr).contains("verify_key"),
        "{}",
        text(&o.stderr)
    );

    let (_env, _server, o) = refused_scenario("openai", 401, &["--verbose"]);
    let err = text(&o.stderr);
    let lines: Vec<&str> = err.lines().collect();
    let pos = |needle: &str| lines.iter().position(|l| l.contains(needle));
    let fin = lines
        .iter()
        .position(|l| l.contains("llm enrich fin") && l.contains("clé refusée"))
        .unwrap_or_else(|| panic!("{err}"));
    let stop = pos("politique: arrêt de l'envoi").unwrap_or_else(|| panic!("{err}"));
    let last = lines
        .iter()
        .rposition(|l| l.starts_with("erreur:"))
        .unwrap();
    assert!(fin < stop && stop < last, "{err}");
    assert!(!err.contains("verify_key"));
    assert!(!lines
        .iter()
        .any(|l| l.starts_with("avertissement:") && l.contains("clé")));
}

#[test]
fn req054_ac9_no_warning_for_refused_files() {
    let (_env, _server, o) = refused_scenario("openai", 401, &[]);
    let err = text(&o.stderr);
    for l in err.lines() {
        assert!(
            !(l.starts_with("avertissement:") && l.contains("clé")),
            "{err}"
        );
        assert!(!l.contains("envoi interrompu"), "{err}");
        assert!(!l.contains("fichier(s) à enrichir"), "{err}");
    }
    assert!(err
        .trim_end()
        .lines()
        .last()
        .unwrap()
        .starts_with("erreur:"));
    // Avant REQ-050 (T-118), les 5 fichiers sont à enrichir.
    assert!(text(&o.stdout)
        .trim_end()
        .ends_with(", 5 fichiers à enrichir"));

    // Plusieurs appels refusés en vol.
    let env = Env::new();
    env.five_files();
    let server = FakeServer::start(vec![(401, "{}".into()); 8]);
    env.config_http("openai", &server.base_url, 3, 1);
    let o = env.run(&["build"]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(
        err.lines()
            .filter(|l| l.contains("clé d'API refusée"))
            .count(),
        1,
        "{err}"
    );
    for l in err.lines() {
        assert!(
            !(l.starts_with("avertissement:") && l.contains("clé d'API refusée")),
            "{err}"
        );
        assert!(!l.contains("envoi interrompu"), "{err}");
        assert!(!l.contains("fichier(s) à enrichir"), "{err}");
    }
    assert_eq!(
        error_lines(&o),
        vec![
            "erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env"
                .to_string()
        ],
        "{err}"
    );
    assert!(err
        .trim_end()
        .lines()
        .last()
        .unwrap()
        .starts_with("erreur:"));
}

#[test]
fn req054_ac10_progress_ends_with_error_line() {
    let (_env, _server, o) = refused_scenario("openai", 401, &["--show-progress"]);
    let err = text(&o.stderr);
    let lines: Vec<&str> = err.lines().collect();
    let a = lines
        .iter()
        .position(|l| *l == "progression: extraction 100 % (5/5 fichiers)")
        .unwrap_or_else(|| panic!("{err}"));
    // Avant REQ-050 (T-118), les 5 fichiers sont à enrichir.
    let b = lines
        .iter()
        .position(|l| *l == "progression: enrichissement 0 % (0/5 fichiers)")
        .unwrap_or_else(|| panic!("{err}"));
    assert!(a < b);
    assert_eq!(
        *lines.last().unwrap(),
        "erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env"
    );
    assert!(!err.contains("verify_key"));
}
