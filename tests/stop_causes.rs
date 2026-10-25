//! Causes d'arrêt de l'envoi : requête refusée (REQ-091, DEC-099), contre le faux serveur.

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use fake_server::{FakeServer, Reply};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const KEY_VAR: &str = "NEPHILA_T178_KEY";
const HINT: &str = "vérifiez model, thinking, effort et max_output_tokens";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Corps d'erreur du fournisseur pour le message `m`.
fn error_body(provider: &str, m: &str) -> String {
    if provider == "anthropic" {
        serde_json::json!({"type":"error","error":{"type":"invalid_request_error","message":m}})
            .to_string()
    } else {
        serde_json::json!({"error":{"message":m,"type":"invalid_request_error"}}).to_string()
    }
}

fn unknown_model(provider: &str) -> String {
    error_body(provider, "model: unknown model 'x'")
}

impl Env {
    fn new(provider: &str, server: &FakeServer, docs: usize, concurrency: usize) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        for i in 0..docs {
            fs::write(
                repo.join(format!("docs/d{i}.md")),
                format!("# D{i}\n\nTexte du document {i}.\n"),
            )
            .unwrap();
        }
        fs::write(
            tmp.path().join("config.toml"),
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = {concurrency}\nbatch_files = 1\n",
                server.base_url
            ),
        )
        .unwrap();
        Env { _tmp: tmp, repo }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", self._tmp.path().join("config.toml"))
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "10")
            .output()
            .unwrap()
    }

    fn add_class(&self) {
        fs::write(
            self.repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
    }
}

fn refusals(status: u16, body: &str, n: usize) -> Vec<Reply> {
    (0..n).map(|_| Reply::status(status, body)).collect()
}

fn posts(s: &FakeServer) -> usize {
    s.requests().iter().filter(|r| r.method == "POST").count()
}

fn lines(o: &Output) -> Vec<String> {
    text(&o.stderr).lines().map(String::from).collect()
}

fn error_lines(o: &Output) -> Vec<String> {
    lines(o)
        .into_iter()
        .filter(|l| l.starts_with("erreur:"))
        .collect()
}

/// Constats communs d'un refus : une ligne `erreur:`, dernière de stderr, E-19.
fn assert_rejected_line(
    o: &Output,
    s: &FakeServer,
    provider: &str,
    code: u16,
    detail: Option<&str>,
    ctx: &str,
) -> String {
    let err = text(&o.stderr);
    let all = lines(o);
    let errors = error_lines(o);
    assert_eq!(errors.len(), 1, "{ctx}: {err}");
    let last = all.last().unwrap();
    assert_eq!(last, &errors[0], "{ctx}: {err}");
    let path = s
        .requests()
        .iter()
        .find(|r| r.method == "POST")
        .unwrap()
        .path
        .clone();
    let url = format!("{}{path}", s.base_url);
    let part = detail.map(|d| format!(" : {d}")).unwrap_or_default();
    assert_eq!(
        last,
        &format!("erreur: requête refusée par {provider} (HTTP {code}) sur {url}{part} ; {HINT}"),
        "{ctx}"
    );
    assert!(!last.contains("envoi interrompu"), "{ctx}: {last}");
    assert!(!err.contains("panne réseau"), "{ctx}: {err}");
    assert!(!err.contains("réseau"), "{ctx}: {err}");
    assert_eq!(o.status.code(), Some(1), "{ctx}");
    last.clone()
}

#[test]
fn req091_ac1_ac7_first_refusal_stops_the_send_with_the_provider_text() {
    for provider in ["openai", "anthropic"] {
        let s = FakeServer::start_replies(refusals(400, &unknown_model(provider), 6));
        let env = Env::new(provider, &s, 3, 1);
        let o = env.run(&["build"]);
        assert_eq!(posts(&s), 1, "{provider}");
        let _ = assert_rejected_line(
            &o,
            &s,
            provider,
            400,
            Some("model: unknown model 'x'"),
            provider,
        );
        assert!(
            text(&o.stdout)
                .trim_end()
                .ends_with(", 3 fichiers à enrichir"),
            "{}",
            text(&o.stdout)
        );
        assert!(
            !text(&o.stderr).contains("avertissement:"),
            "{}",
            text(&o.stderr)
        );
    }
}

#[test]
fn req091_ac2_other_refusal_statuses_and_405_unchanged() {
    for code in [404u16, 413, 422] {
        let s = FakeServer::start_replies(refusals(code, &unknown_model("openai"), 6));
        let env = Env::new("openai", &s, 3, 1);
        let o = env.run(&["build"]);
        assert_eq!(posts(&s), 1, "{code}");
        let last = assert_rejected_line(
            &o,
            &s,
            "openai",
            code,
            Some("model: unknown model 'x'"),
            &code.to_string(),
        );
        assert!(last.contains("model: unknown model 'x'"), "{last}");
    }
    let s = FakeServer::start_replies(refusals(405, "{}", 6));
    let env = Env::new("openai", &s, 3, 1);
    let o = env.run(&["build"]);
    assert_eq!(posts(&s), 3);
    let err = text(&o.stderr);
    assert!(err.contains("panne réseau répétée"), "{err}");
    assert!(!err.contains("requête refusée"), "{err}");
}

#[test]
fn req091_ac3_ask_gives_the_single_error_line() {
    for provider in ["anthropic", "openai"] {
        let s = FakeServer::start_replies(refusals(400, &unknown_model(provider), 6));
        let env = Env::new(provider, &s, 1, 1);
        env.add_class();
        assert!(env.run(&["build", "--no-llm"]).status.success());
        let o = env.run(&["query", "ask", "Alpha"]);
        assert_eq!(posts(&s), 1, "{provider}");
        assert!(o.stdout.is_empty(), "{provider}");
        let all = lines(&o);
        assert_eq!(all.len(), 1, "{provider}: {all:?}");
        let last = assert_rejected_line(
            &o,
            &s,
            provider,
            400,
            Some("model: unknown model 'x'"),
            provider,
        );
        assert!(last.contains("model: unknown model 'x'"), "{last}");
    }
}

#[test]
fn req091_ac5_concurrent_calls_give_one_line_and_no_file_warning() {
    let s = FakeServer::start_replies(refusals(400, &unknown_model("openai"), 14));
    let env = Env::new("openai", &s, 10, 3);
    let o = env.run(&["build"]);
    let n = posts(&s);
    assert!((1..=3).contains(&n), "{n} requêtes");
    let err = text(&o.stderr);
    assert_eq!(
        err.lines()
            .filter(|l| l.contains("requête refusée"))
            .count(),
        1,
        "{err}"
    );
    assert!(!err.contains("avertissement:"), "{err}");
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn req091_ac6_verbose_and_progress_keep_the_error_last() {
    let s = FakeServer::start_replies(refusals(400, &unknown_model("openai"), 6));
    let env = Env::new("openai", &s, 3, 1);
    let o = env.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    let all = lines(&o);
    let end = all
        .iter()
        .position(|l| l.contains("llm enrich fin") && l.contains("requête refusée"))
        .unwrap_or_else(|| panic!("{err}"));
    let end_line = &all[end];
    assert!(
        end_line.contains(" [1/1] requête refusée ")
            && end_line.ends_with(" ms : requête refusée par openai (HTTP 400)"),
        "{end_line}"
    );
    assert!(!end_line.contains("unknown model"), "{end_line}");
    let stop = all
        .iter()
        .position(|l| {
            l.ends_with(
                "politique: arrêt de l'envoi (requête refusée par le fournisseur), 2 fichiers non envoyés",
            )
        })
        .unwrap_or_else(|| panic!("{err}"));
    assert!(stop > end);
    assert!(all
        .last()
        .unwrap()
        .starts_with("erreur: requête refusée par openai"));
    assert!(!err.contains("llm enrich attente"), "{err}");

    let s = FakeServer::start_replies(refusals(400, &unknown_model("openai"), 6));
    let env = Env::new("openai", &s, 3, 1);
    let o = env.run(&["build", "--show-progress"]);
    assert_rejected_line(
        &o,
        &s,
        "openai",
        400,
        Some("model: unknown model 'x'"),
        "progress",
    );
}

#[test]
fn req091_ac7_empty_body_and_long_message() {
    let s = FakeServer::start_replies(refusals(400, "", 6));
    let env = Env::new("openai", &s, 3, 1);
    let o = env.run(&["build"]);
    let last = assert_rejected_line(&o, &s, "openai", 400, None, "vide");
    assert!(!last.contains(" : "), "{last}");

    let long = format!("{}\n\t{}", "a".repeat(150), "b".repeat(150));
    let expected_long = format!("{} {}…", "a".repeat(150), "b".repeat(48));
    let s = FakeServer::start_replies(refusals(400, &error_body("openai", &long), 6));
    let env = Env::new("openai", &s, 3, 1);
    let o = env.run(&["build"]);
    let last = assert_rejected_line(&o, &s, "openai", 400, Some(&expected_long), "long");
    let middle = last
        .split_once(" : ")
        .unwrap()
        .1
        .rsplit_once(&format!(" ; {HINT}"))
        .unwrap()
        .0;
    assert!(middle.chars().count() <= 200, "{}", middle.chars().count());
    assert!(middle.ends_with('…'), "{middle}");
    assert!(!middle.contains('\n') && !middle.contains('\t'));
}
