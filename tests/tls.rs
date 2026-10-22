//! TLS de bout en bout : clés `tls_ca_cert` et `tls_insecure` (REQ-036, REQ-002, REQ-026).

#[path = "support/fake_server.rs"]
#[allow(dead_code)]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use fake_server::FakeServer;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EXTRACTION: &str =
    r#"{"version":2,"summaries":[{"node":"type:Alpha","summary":"Le A."}],"concepts":[]}"#;
const ASK: &str = r#"{"found":true,"answer":"reponse","nodes":["Alpha"]}"#;

fn fixture(name: &str) -> Vec<u8> {
    fs::read(format!(
        "{}/tests/fixtures/tls/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn server(cert: &str, responses: Vec<(u16, String)>) -> FakeServer {
    let key = cert.replace(".pem", ".key");
    FakeServer::start_tls(responses, &fixture(cert), &fixture(&key))
}

fn reply(provider: &str, body: &str) -> (u16, String) {
    let v = if provider == "openai" {
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":body}}]})
    } else {
        serde_json::json!({"content":[{"type":"text","text":body}]})
    };
    (200, v.to_string())
}

/// Réponses pour une commande `build` puis une commande `query ask`.
fn script(provider: &str) -> Vec<(u16, String)> {
    vec![reply(provider, EXTRACTION), reply(provider, ASK)]
}

struct Env {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("Alpha.java"), "public class Alpha {}\n").unwrap();
        let conf = tmp.path().join("conf");
        fs::create_dir_all(conf.join("certs")).unwrap();
        fs::write(conf.join("certs/ca.pem"), fixture("ca.pem")).unwrap();
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::write(tmp.path().join("home/ca.pem"), fixture("ca.pem")).unwrap();
        Env {
            config: conf.join("config.toml"),
            repo,
            tmp,
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.tmp.path().join(p)
    }

    fn config(&self, provider: &str, url: &str, extra: &str) {
        let key = if provider == "anthropic" {
            "api_key_env = \"NEPHILA_TLS_KEY\"\n"
        } else {
            ""
        };
        fs::write(
            &self.config,
            format!(
                "provider = \"{provider}\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{url}\"\n{key}{extra}\n"
            ),
        )
        .unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_TLS_KEY", "sk-test")
            .env("HOME", self.path("home"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn db(&self) -> PathBuf {
        self.repo.join(".nephila")
    }

    /// Graphe sans LLM, pour que `query ask` ait de quoi répondre.
    fn seed(&self) {
        let o = self.run(&["build", "--no-llm"]);
        assert!(o.status.success(), "{}", text(&o.stderr));
    }
}

fn warnings(o: &Output) -> Vec<String> {
    text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect()
}

fn assert_cert_failure(o: &Output) {
    assert!(!o.status.success());
    let e = text(&o.stderr);
    assert!(e.contains("certificat"), "{e}");
    assert!(
        e.contains("tls_ca_cert") && e.contains("tls_insecure"),
        "{e}"
    );
}

/// Refus de certificat par `build` (AC-036-1 modifié) : au premier appel, après
/// la phase structure ; le fichier de l'appel refusé est nommé par un
/// avertissement ; bilan sur stdout ; aucune requête HTTP reçue.
fn assert_build_cert_refused(env: &Env, o: &Output) {
    assert_cert_failure(o);
    assert!(env.db().exists());
    let w = warnings(o);
    assert!(
        w.iter().any(|l| l.contains("certificat")
            && l.contains("tls_ca_cert")
            && l.contains("tls_insecure")),
        "{}",
        text(&o.stderr)
    );
    assert!(
        text(&o.stdout).starts_with("build terminé"),
        "{}",
        text(&o.stdout)
    );
    // La structure est construite : le nœud du fichier est trouvé.
    let s = env.run(&["query", "search", "Alpha"]);
    assert!(text(&s.stdout).contains("Alpha"), "{}", text(&s.stdout));
    // Le fichier du dépôt reste à enrichir.
    let st = env.run(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&st.stdout).unwrap();
    assert_eq!(v["to_enrich"], 1, "{v}");
}

#[test]
fn req036_ac1_build_refused_at_first_call() {
    for provider in ["openai", "anthropic"] {
        let env = Env::new();
        let s = server("server-ip.pem", script(provider));
        env.config(provider, &s.base_url, "");
        let o = env.run(&["build"]);
        // Plus de vérification préalable (REQ-054) : la structure est construite.
        assert_build_cert_refused(&env, &o);
        assert!(s.requests().is_empty());
    }
}

#[test]
fn req036_ac1_ask_refused() {
    let env = Env::new();
    env.seed();
    let s = server("server-ip.pem", script("openai"));
    env.config("openai", &s.base_url, "");
    let o = env.run(&["query", "ask", "Alpha"]);
    assert_cert_failure(&o);
    assert!(o.stdout.is_empty());
    assert!(s.requests().is_empty());
}

fn full_cycle(provider: &str, extra: &str, cert: &str) -> (Output, Output) {
    let env = Env::new();
    let s = server(cert, script(provider));
    env.config(provider, &s.base_url, extra);
    let b = env.run(&["build"]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    let a = env.run(&["query", "ask", "Alpha"]);
    assert!(a.status.success(), "{}", text(&a.stderr));
    let paths: Vec<String> = s.requests().iter().map(|r| r.path.clone()).collect();
    let chat = if provider == "openai" {
        "/v1/chat/completions"
    } else {
        "/v1/messages"
    };
    assert_eq!(paths, [chat, chat], "{paths:?}");
    (b, a)
}

#[test]
fn req036_ac2_authority_makes_build_and_ask_succeed() {
    for provider in ["openai", "anthropic"] {
        let (b, a) = full_cycle(
            provider,
            "tls_ca_cert = \"certs/ca.pem\"\n",
            "server-ip.pem",
        );
        assert!(warnings(&b).is_empty() && warnings(&a).is_empty());
        assert!(text(&a.stdout).contains("reponse"));
    }
}

#[test]
fn req036_ac3_path_forms() {
    let env = Env::new();
    let abs = env.path("conf/certs/ca.pem");
    let bundle = env.path("conf/bundle.pem");
    fs::write(&bundle, fixture("bundle.pem")).unwrap();
    let forms = [
        "certs/ca.pem".to_string(),
        "~/ca.pem".to_string(),
        abs.display().to_string(),
        "bundle.pem".to_string(),
    ];
    for f in forms {
        let s = server("server-ip.pem", script("openai"));
        env.config("openai", &s.base_url, &format!("tls_ca_cert = \"{f}\"\n"));
        // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
        let _ = fs::remove_dir_all(env.repo.join(".nephila"));
        let b = env.run(&["build"]);
        assert!(b.status.success(), "{f}: {}", text(&b.stderr));
        assert_eq!(s.requests().len(), 1, "{f}");
    }
}

#[test]
fn req036_ac4_wrong_authority_and_wrong_name() {
    let env = Env::new();
    fs::write(env.path("conf/other.pem"), fixture("other-ca.pem")).unwrap();
    let s = server("server-ip.pem", script("openai"));
    env.config("openai", &s.base_url, "tls_ca_cert = \"other.pem\"\n");
    let o = env.run(&["build"]);
    assert_build_cert_refused(&env, &o);
    assert!(s.requests().is_empty());

    let s = server("server-name.pem", script("openai"));
    env.config("openai", &s.base_url, "tls_ca_cert = \"certs/ca.pem\"\n");
    let o = env.run(&["build"]);
    assert_build_cert_refused(&env, &o);
    assert!(s.requests().is_empty());
}

#[test]
fn req036_ac5_invalid_ca_file_is_a_configuration_error() {
    let env = Env::new();
    let conf = env.path("conf");
    fs::create_dir_all(conf.join("adir")).unwrap();
    fs::write(conf.join("nocert.pem"), fixture("nocert.pem")).unwrap();
    fs::write(conf.join("corrupt.pem"), fixture("corrupt.pem")).unwrap();
    for (name, shown) in [
        ("missing.pem", "missing.pem"),
        ("adir", "adir"),
        ("nocert.pem", "nocert.pem"),
        ("corrupt.pem", "corrupt.pem"),
    ] {
        for provider in ["openai", "mock"] {
            env.config(
                provider,
                "https://127.0.0.1:1",
                &format!("tls_ca_cert = \"{name}\"\n"),
            );
            for args in [&["build"][..], &["update"], &["query", "ask", "Alpha"]] {
                let o = env.run(args);
                assert!(!o.status.success(), "{name} {args:?}");
                let e = text(&o.stderr);
                assert!(e.contains("tls_ca_cert"), "{e}");
                assert!(
                    e.contains(&conf.join(shown).display().to_string()),
                    "{name}: {e}"
                );
                assert!(!env.db().exists(), "{name} {args:?}");
            }
            let o = env.run(&["build", "--no-llm"]);
            assert!(o.status.success(), "{name}: {}", text(&o.stderr));
            fs::remove_dir_all(env.db()).unwrap();
        }
    }
}

#[test]
fn req036_ac5_empty_and_mistyped_keys() {
    let env = Env::new();
    for (extra, key) in [
        ("tls_ca_cert = \"\"\n", "tls_ca_cert"),
        ("tls_insecure = \"yes\"\n", "tls_insecure"),
        ("tls_insecure = 1\n", "tls_insecure"),
    ] {
        env.config("openai", "https://127.0.0.1:1", extra);
        let o = env.run(&["build"]);
        assert!(!o.status.success(), "{extra}");
        let e = text(&o.stderr);
        assert!(e.contains(key), "{extra}: {e}");
        if key == "tls_ca_cert" {
            assert!(e.contains("vide"), "{e}");
        }
        assert!(!env.db().exists());
        assert!(env.run(&["build", "--no-llm"]).status.success());
        fs::remove_dir_all(env.db()).unwrap();
    }
}

#[test]
fn req036_ac6_insecure_with_one_warning() {
    for provider in ["openai", "anthropic"] {
        for cert in ["server-ip.pem", "server-name.pem"] {
            let (b, a) = full_cycle(provider, "tls_insecure = true\n", cert);
            for o in [&b, &a] {
                let w = warnings(o);
                assert_eq!(w.len(), 1, "{w:?}");
                assert!(w[0].contains("tls_insecure"), "{w:?}");
            }
            let (_, a2) = full_cycle(
                provider,
                "tls_ca_cert = \"certs/ca.pem\"\n",
                "server-ip.pem",
            );
            assert_eq!(a.stdout, a2.stdout);
        }
    }
}

#[test]
fn req036_ac6_no_warning_when_false_or_absent() {
    let env = Env::new();
    for extra in ["tls_insecure = false\n", ""] {
        let s = server("server-ip.pem", script("openai"));
        env.config("openai", &s.base_url, extra);
        let o = env.run(&["build"]);
        assert_build_cert_refused(&env, &o);
        assert!(
            !warnings(&o)
                .iter()
                .any(|w| w.contains("contrôle des certificats du serveur désactivé")),
            "{extra}"
        );
    }
}

#[test]
fn req036_ac6_http_url_gives_no_warning() {
    let env = Env::new();
    let s = FakeServer::start(script("openai"));
    env.config("openai", &s.base_url, "tls_insecure = true\n");
    let o = env.run(&["build"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(warnings(&o).is_empty());
}

#[test]
fn req036_ac7_mock_ignores_tls_settings() {
    let env = Env::new();
    let mock = env.path("mock");
    fs::create_dir_all(&mock).unwrap();
    let run = |env: &Env| {
        let log = env.path("mock.log");
        let _ = fs::remove_file(&log);
        let o = Command::new(cargo_bin("nephila"))
            .env("NEPHILA_CONFIG", &env.config)
            .env("NEPHILA_MOCK_DIR", &mock)
            .env("NEPHILA_MOCK_LOG", &log)
            .arg("-C")
            .arg(&env.repo)
            .arg("build")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", text(&o.stderr));
        let _ = fs::remove_dir_all(env.db());
        (o, fs::read_to_string(&log).unwrap_or_default())
    };
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
    )
    .unwrap();
    let (_, plain_log) = run(&env);
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\ntls_insecure = true\ntls_ca_cert = \"certs/ca.pem\"\n",
    )
    .unwrap();
    let (o, log) = run(&env);
    assert!(warnings(&o).is_empty());
    assert_eq!(log, plain_log);
}

#[test]
fn req036_ac8_verbose_config_line() {
    let env = Env::new();
    let s = server("server-ip.pem", script("openai"));
    let line = |o: &Output| {
        text(&o.stderr)
            .lines()
            .find(|l| l.contains("config: llm="))
            .unwrap()
            .to_string()
    };
    env.config(
        "openai",
        &s.base_url,
        "batch_files = 1\ntls_ca_cert = \"certs/ca.pem\"\ntls_insecure = true\n",
    );
    let o = env.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let expected = format!(
        " tls_ca_cert={} tls_insecure",
        Path::new(&env.path("conf")).join("certs/ca.pem").display()
    );
    assert!(line(&o).ends_with(&expected), "{}", line(&o));

    fs::remove_dir_all(env.db()).unwrap();
    let s = FakeServer::start(script("openai"));
    env.config("openai", &s.base_url, "batch_files = 1\n");
    let o = env.run(&["build", "--verbose"]);
    assert!(
        line(&o).ends_with("batch_files=1 format_retries=2"),
        "{}",
        line(&o)
    );
}
