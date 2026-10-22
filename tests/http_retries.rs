//! Nouveaux essais HTTP avec attente progressive (REQ-052, DEC-064), contre le faux serveur.

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use fake_server::{FakeServer, Recorded, Reply};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const KEY_VAR: &str = "NEPHILA_T115_KEY";
const SUMMARY: &str = "Résumé du document D";
const ASK_REPLY: &str = r#"{"found":true,"answer":"réponse-ask","nodes":["Alpha"]}"#;

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn good_enrichment() -> String {
    serde_json::json!({
        "version": 2,
        "summaries": [{"node": "document:docs/d.md", "summary": SUMMARY}],
        "concepts": []
    })
    .to_string()
}

/// Corps de réponse conforme du fournisseur `provider` pour le texte `t`.
fn reply_body(provider: &str, t: &str) -> String {
    if provider == "anthropic" {
        serde_json::json!({"content":[{"type":"text","text":t}]}).to_string()
    } else {
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":t}}]}).to_string()
    }
}

fn good(provider: &str) -> Reply {
    Reply::status(200, &reply_body(provider, &good_enrichment()))
}

impl Env {
    /// Dépôt avec un document, configuration HTTP vers le faux serveur.
    fn new(provider: &str, server: &FakeServer) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/d.md"), "# D\n\nTexte du document.\n").unwrap();
        fs::write(
            tmp.path().join("config.toml"),
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\n",
                server.base_url
            ),
        )
        .unwrap();
        Env { _tmp: tmp, repo }
    }

    fn cmd(&self, args: &[&str], base_ms: Option<&str>) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).args(args);
        c.env("NEPHILA_CONFIG", self._tmp.path().join("config.toml"))
            .env(KEY_VAR, "sk-test")
            .env_remove("NEPHILA_RETRY_BASE_MS");
        if let Some(v) = base_ms {
            c.env("NEPHILA_RETRY_BASE_MS", v);
        }
        c
    }

    fn run(&self, args: &[&str], base_ms: Option<&str>) -> Output {
        self.cmd(args, base_ms).output().unwrap()
    }

    fn search_finds_summary(&self) -> bool {
        let q = self.run(&["query", "search", "d.md", "--json"], None);
        q.status.success() && text(&q.stdout).contains(SUMMARY)
    }
}

fn posts(s: &FakeServer) -> Vec<Recorded> {
    s.requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect()
}

fn warnings(o: &Output) -> Vec<String> {
    text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect()
}

#[test]
fn req052_ac1_two_503_then_conforming_reply_enriches_silently() {
    for provider in ["openai", "anthropic"] {
        let s = FakeServer::start_replies(vec![
            Reply::status(503, "{}"),
            Reply::status(503, "{}"),
            good(provider),
        ]);
        let env = Env::new(provider, &s);
        let o = env.run(&["build"], Some("10"));
        assert_eq!(o.status.code(), Some(0), "{provider}: {}", text(&o.stderr));
        let reqs = posts(&s);
        assert_eq!(reqs.len(), 3, "{provider}");
        assert!(reqs.iter().all(|r| r.body == reqs[0].body), "{provider}");
        assert!(env.search_finds_summary(), "{provider}");
        assert!(warnings(&o).is_empty(), "{}", text(&o.stderr));
    }
}

#[test]
fn req052_ac2_listed_statuses_are_retried_once() {
    for status in [429, 500, 502, 503, 504, 529] {
        let s = FakeServer::start_replies(vec![Reply::status(status, "{}"), good("openai")]);
        let env = Env::new("openai", &s);
        let o = env.run(&["build"], Some("10"));
        assert_eq!(o.status.code(), Some(0), "{status}: {}", text(&o.stderr));
        assert_eq!(posts(&s).len(), 2, "{status}");
        assert!(env.search_finds_summary(), "{status}");
    }
}

#[test]
fn req052_ac2_other_statuses_are_sent_once() {
    for status in [400, 404, 401, 403, 413] {
        let s = FakeServer::start_replies(vec![Reply::status(status, "{}"), good("openai")]);
        let env = Env::new("openai", &s);
        let o = env.run(&["build"], Some("10"));
        assert_ne!(o.status.code(), Some(0), "{status}");
        assert_eq!(posts(&s).len(), 1, "{status}");
        if status != 401 && status != 403 {
            assert!(!warnings(&o).is_empty(), "{status}: {}", text(&o.stderr));
        }
    }
}

#[test]
fn req052_ac3_always_500_gives_five_requests_and_plain_warning() {
    let s = FakeServer::start_replies((0..8).map(|_| Reply::status(500, "{}")).collect());
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], Some("10"));
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(posts(&s).len(), 5);
    let w = warnings(&o);
    assert!(
        w.iter().any(|l| l.contains("erreur de openai (HTTP 500)")),
        "{w:?}"
    );
    assert!(w.iter().all(|l| !l.contains("essai")), "{w:?}");
    assert!(
        !env.search_finds_summary(),
        "le fichier doit rester à enrichir"
    );
}

#[test]
fn req052_ac4_retry_after_one_second_is_respected() {
    let s = FakeServer::start_replies(vec![
        Reply::with_header(429, "{}", "retry-after", "1"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], None);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let r = posts(&s);
    assert_eq!(r.len(), 2);
    let gap = r[1].at - r[0].answered_at.unwrap();
    assert!(
        gap >= Duration::from_millis(950) && gap < Duration::from_millis(1500),
        "{gap:?}"
    );
}

#[test]
fn req052_ac4_retry_after_zero_retries_at_once() {
    let s = FakeServer::start_replies(vec![
        Reply::with_header(429, "{}", "retry-after", "0"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], None);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let r = posts(&s);
    assert_eq!(r.len(), 2);
    assert!(r[1].at - r[0].answered_at.unwrap() < Duration::from_millis(500));
}

#[test]
fn req052_ac4_retry_after_120_gives_no_retry_and_no_wait() {
    let s = FakeServer::start_replies(vec![
        Reply::with_header(503, "{}", "retry-after", "120"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let t = Instant::now();
    let o = env.run(&["build"], None);
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(posts(&s).len(), 1);
    assert!(!warnings(&o).is_empty());
}

#[test]
fn req052_ac6_connection_closed_without_reply_is_retried() {
    for provider in ["openai", "anthropic"] {
        let s = FakeServer::start_replies(vec![Reply::Close, good(provider)]);
        let env = Env::new(provider, &s);
        let o = env.run(&["build"], Some("10"));
        assert_eq!(o.status.code(), Some(0), "{provider}: {}", text(&o.stderr));
        assert_eq!(posts(&s).len(), 2, "{provider}");
        assert!(env.search_finds_summary(), "{provider}");
    }
}

#[test]
fn req052_ac7_exhausted_calls_count_once_each_in_the_policy() {
    let tmp_docs = 4;
    let s = FakeServer::start_replies(
        (0..40)
            .map(|_| Reply::Http {
                status: 503,
                body: "{}".into(),
                headers: vec![],
                delay_ms: 100,
            })
            .collect(),
    );
    let env = Env::new("openai", &s);
    for i in 1..tmp_docs {
        fs::write(
            env.repo.join(format!("docs/e{i}.md")),
            format!("# E{i}\n\nTexte.\n"),
        )
        .unwrap();
    }
    let t0 = Instant::now();
    let o = env.run(&["build", "--verbose"], Some("1"));
    assert_ne!(o.status.code(), Some(0));
    let r = posts(&s);
    assert_eq!(r.len(), 15, "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("panne réseau répétée"));
    // Les lignes de politique viennent après les 5 envois de leur appel.
    let err = text(&o.stderr);
    for (n, sent) in [(1, 5), (2, 10), (3, 15)] {
        let marker = format!("politique: erreurs réseau consécutives {n}/3");
        let line = err
            .lines()
            .find(|l| l.contains(&marker))
            .unwrap_or_else(|| panic!("{marker} absente : {err}"));
        let secs: f64 = line
            .split("[")
            .nth(1)
            .and_then(|x| x.split("s]").next())
            .and_then(|x| x.parse().ok())
            .unwrap();
        let answered = (r[sent - 1].answered_at.unwrap() - t0).as_secs_f64();
        assert!(
            secs >= answered - 0.1,
            "{marker} à {secs}s avant {answered}s"
        );
        if sent < r.len() {
            assert!(secs <= (r[sent].at - t0).as_secs_f64(), "{marker}");
        }
    }
}

#[test]
fn req052_ac8_ask_retries_then_answers() {
    for provider in ["openai", "anthropic"] {
        let s = FakeServer::start_replies(vec![
            Reply::status(503, "{}"),
            Reply::status(200, &reply_body(provider, ASK_REPLY)),
        ]);
        let env = Env::new(provider, &s);
        fs::write(
            env.repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
        let o = env.run(&["build", "--no-llm"], None);
        assert!(o.status.success(), "{}", text(&o.stderr));
        let o = env.run(&["query", "ask", "Alpha"], Some("10"));
        assert_eq!(o.status.code(), Some(0), "{provider}: {}", text(&o.stderr));
        assert!(text(&o.stdout).contains("réponse-ask"), "{provider}");
        assert_eq!(posts(&s).len(), 2, "{provider}");
    }
}

#[test]
fn req052_ac8_ask_always_503_gives_five_requests_and_error() {
    let s = FakeServer::start_replies((0..8).map(|_| Reply::status(503, "{}")).collect());
    let env = Env::new("openai", &s);
    fs::write(
        env.repo.join("Alpha.java"),
        "package p;\npublic class Alpha {}\n",
    )
    .unwrap();
    assert!(env.run(&["build", "--no-llm"], None).status.success());
    let o = env.run(&["query", "ask", "Alpha"], Some("10"));
    assert_ne!(o.status.code(), Some(0));
    assert!(o.stdout.is_empty());
    assert!(text(&o.stderr).contains("erreur de openai (HTTP 503)"));
    assert_eq!(posts(&s).len(), 5);
}

#[test]
fn req052_ac9_format_retry_and_http_retries_are_independent() {
    let s = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(200, &reply_body("openai", "désolé, pas de JSON")),
        Reply::status(503, "{}"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], Some("10"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(posts(&s).len(), 4);
    assert!(env.search_finds_summary());
}

#[test]
fn req052_ac12_malformed_base_variable_is_refused_before_any_request() {
    let s = FakeServer::start_replies(vec![good("openai")]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], Some("abc"));
    assert_ne!(o.status.code(), Some(0));
    assert!(
        text(&o.stderr)
            .lines()
            .any(|l| l == "erreur: NEPHILA_RETRY_BASE_MS : entier attendu, reçu « abc »"),
        "{}",
        text(&o.stderr)
    );
    assert!(s.requests().is_empty());
}
