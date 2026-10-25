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
        assert_eq!(o.status.code(), Some(1), "{status}");
        assert_eq!(posts(&s).len(), 1, "{status}");
        assert!(
            !env.search_finds_summary(),
            "{status} : le fichier doit rester à enrichir"
        );
        // 401/403 : clé refusée (REQ-054) ; 400, 404, 413 : requête refusée (REQ-091), E-19.
        assert!(warnings(&o).is_empty(), "{status}: {}", text(&o.stderr));
        let last = text(&o.stderr).lines().last().unwrap().to_string();
        if status == 401 || status == 403 {
            assert!(last.starts_with("erreur: clé d'API refusée"), "{last}");
        } else {
            assert!(
                last.starts_with(&format!(
                    "erreur: requête refusée par openai (HTTP {status}) sur "
                )),
                "{status}: {last}"
            );
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

// ---- Traces des nouveaux essais (AC-052-10, AC-052-11) ----

const DOC: &str = "docs/d.md";

fn trace_lines(o: &Output) -> Vec<String> {
    text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("trace:"))
        .map(String::from)
        .collect()
}

fn waits(o: &Output) -> Vec<String> {
    trace_lines(o)
        .into_iter()
        .filter(|l| l.contains("llm enrich attente"))
        .collect()
}

/// `(ms, reste)` d'une ligne `attente` : `<label> <ms> ms <reste>`.
fn wait_parts(line: &str, label: &str) -> (u64, String) {
    let prefix = format!("llm enrich attente {label} ");
    let after = &line[line.find(&prefix).unwrap_or_else(|| panic!("{line}")) + prefix.len()..];
    let (ms, rest) = after.split_once(" ms ").unwrap_or_else(|| panic!("{line}"));
    (ms.parse().unwrap_or_else(|_| panic!("{line}")), rest.into())
}

fn position(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle} absent : {lines:#?}"))
}

fn assert_no_wait_or_retry_words(o: &Output) {
    for l in text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("avertissement:") || l.starts_with("erreur:"))
    {
        assert!(!l.contains("nouvel essai") && !l.contains("envois"), "{l}");
    }
}

#[test]
fn req052_ac10_wait_lines_sit_between_start_and_end_without_content() {
    let s = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(503, "{}"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let lines = trace_lines(&o);
    let start = position(&lines, &format!("llm enrich départ {DOC} [1/1]"));
    let w1 = position(&lines, "(nouvel essai 1/4)");
    let w2 = position(&lines, "(nouvel essai 2/4)");
    let end = position(&lines, &format!("llm enrich fin {DOC} [1/1] ok"));
    assert!(start < w1 && w1 < w2 && w2 < end, "{lines:#?}");
    let w = waits(&o);
    assert_eq!(w.len(), 2, "{w:#?}");
    for l in &w {
        assert!(l.contains("après HTTP 503"), "{l}");
        for forbidden in ["sk-test", "Texte du document", SUMMARY, "127.0.0.1"] {
            assert!(!l.contains(forbidden), "{l}");
        }
    }
    assert_no_wait_or_retry_words(&o);

    // Sans --verbose : mêmes sorties qu'un serveur qui répond bien du premier coup.
    let s1 = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(503, "{}"),
        good("openai"),
    ]);
    let s2 = FakeServer::start_replies(vec![good("openai")]);
    let (e1, e2) = (Env::new("openai", &s1), Env::new("openai", &s2));
    let (o1, o2) = (
        e1.run(&["build"], Some("10")),
        e2.run(&["build"], Some("10")),
    );
    assert_eq!(text(&o1.stdout), text(&o2.stdout));
    assert_eq!(text(&o1.stderr), text(&o2.stderr));
}

#[test]
fn req052_ac11_wait_line_format_and_ranges() {
    let s = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(503, "{}"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_eq!(o.status.code(), Some(0));
    let w = waits(&o);
    assert_eq!(w.len(), 2);
    let label = format!("{DOC} [1/1]");
    let (ms1, rest1) = wait_parts(&w[0], &label);
    let (ms2, rest2) = wait_parts(&w[1], &label);
    assert_eq!(rest1, "après HTTP 503 (nouvel essai 1/4)");
    assert_eq!(rest2, "après HTTP 503 (nouvel essai 2/4)");
    assert!((5..=10).contains(&ms1), "{ms1}");
    assert!((10..=20).contains(&ms2), "{ms2}");
    let shape = |l: &str| {
        let t = l.strip_prefix("trace: [").unwrap();
        let (time, rest) = t.split_once("s] ").unwrap();
        let (sec, milli) = time.split_once('.').unwrap();
        rest.starts_with("llm enrich attente ")
            && !sec.is_empty()
            && sec.bytes().all(|b| b.is_ascii_digit())
            && milli.len() == 3
            && milli.bytes().all(|b| b.is_ascii_digit())
    };
    assert!(w.iter().all(|l| shape(l)), "{w:#?}");
}

#[test]
fn req052_ac11_lot_label() {
    let lot = serde_json::json!({"version": 3, "files": [
        {"file": "docs/d.md", "summaries": [], "concepts": []},
        {"file": "docs/e.md", "summaries": [], "concepts": []},
    ]})
    .to_string();
    let s = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(200, &reply_body("openai", &lot)),
    ]);
    let env = Env::new("openai", &s);
    fs::write(env.repo.join("docs/e.md"), "# E\n\nTexte.\n").unwrap();
    let conf = env._tmp.path().join("config.toml");
    let c = fs::read_to_string(&conf)
        .unwrap()
        .replace("batch_files = 1", "batch_files = 2");
    fs::write(&conf, c).unwrap();
    let o = env.run(&["build", "--verbose"], Some("10"));
    let w = waits(&o);
    assert_eq!(w.len(), 1, "{}", text(&o.stderr));
    let (_, rest) = wait_parts(&w[0], "lot docs/d.md +1 [1/1]");
    assert_eq!(rest, "après HTTP 503 (nouvel essai 1/4)");
}

#[test]
fn req052_ac11_network_error_cause() {
    let s = FakeServer::start_replies(vec![Reply::Close, good("openai")]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let w = waits(&o);
    assert_eq!(w.len(), 1, "{w:#?}");
    let (_, rest) = wait_parts(&w[0], &format!("{DOC} [1/1]"));
    assert_eq!(rest, "après erreur réseau (nouvel essai 1/4)");
}

#[test]
fn req052_ac11_retry_after_is_the_wait() {
    let s = FakeServer::start_replies(vec![
        Reply::with_header(429, "{}", "retry-after", "1"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], None);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let w = waits(&o);
    assert_eq!(w.len(), 1, "{w:#?}");
    let (ms, rest) = wait_parts(&w[0], &format!("{DOC} [1/1]"));
    assert_eq!(ms, 1000);
    assert_eq!(rest, "après HTTP 429 (nouvel essai 1/4)");
}

#[test]
fn req052_ac11_no_wait_line_without_a_new_attempt() {
    let cases = [
        Reply::with_header(503, "{}", "retry-after", "120"),
        Reply::status(400, "{}"),
        Reply::status(401, "{}"),
    ];
    for first in cases {
        let s = FakeServer::start_replies(vec![first.clone(), good("openai")]);
        let env = Env::new("openai", &s);
        let o = env.run(&["build", "--verbose"], None);
        assert!(waits(&o).is_empty(), "{first:?}: {}", text(&o.stderr));
        assert_eq!(posts(&s).len(), 1);
        assert_no_wait_or_retry_words(&o);
    }
}

#[test]
fn req052_ac11_exhausted_calls_give_four_waits_one_start_one_end() {
    let s = FakeServer::start_replies((0..8).map(|_| Reply::status(500, "{}")).collect());
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("1"));
    assert_ne!(o.status.code(), Some(0));
    let w = waits(&o);
    assert_eq!(w.len(), 4, "{w:#?}");
    for (i, l) in w.iter().enumerate() {
        assert!(l.ends_with(&format!("(nouvel essai {}/4)", i + 1)), "{l}");
    }
    let lines = trace_lines(&o);
    let starts = lines
        .iter()
        .filter(|l| l.contains("llm enrich départ"))
        .count();
    let ends: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("llm enrich fin"))
        .collect();
    assert_eq!(starts, 1);
    assert_eq!(ends.len(), 1);
    assert!(ends[0].contains(" réseau "), "{}", ends[0]);
    assert_no_wait_or_retry_words(&o);
}

#[test]
fn req052_ac11_format_retry_restarts_the_wait_count() {
    let s = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(200, &reply_body("openai", "désolé, pas de JSON")),
        Reply::status(503, "{}"),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let lines = trace_lines(&o);
    let second = lines
        .iter()
        .position(|l| l.contains("llm enrich départ") && l.contains("(essai 2/"))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    let wait_after = lines[second..]
        .iter()
        .find(|l| l.contains("llm enrich attente"))
        .unwrap();
    assert!(wait_after.ends_with("(nouvel essai 1/4)"), "{wait_after}");
    assert_eq!(waits(&o).len(), 2);
}

// ---- Une seule tentative (AC-052-6, intégration) ----

#[test]
fn req052_ac6_timeout_is_sent_once() {
    let s = FakeServer::start_replies(vec![
        Reply::Http {
            status: 200,
            body: reply_body("openai", &good_enrichment()),
            headers: vec![],
            delay_ms: 2500,
        },
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let conf = env._tmp.path().join("config.toml");
    let c = fs::read_to_string(&conf).unwrap() + "timeout_secs = 1\n";
    fs::write(&conf, c).unwrap();
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(posts(&s).len(), 1, "{}", text(&o.stderr));
    assert!(waits(&o).is_empty());
    assert!(text(&o.stderr).contains("délai dépassé"));
}

// --- REQ-097 : lecture interrompue du corps d'une réponse 2xx ---

fn cut() -> Reply {
    Reply::Partial {
        status: 200,
        declared_len: 1000,
        sent: 10,
    }
}

fn ask_env(provider: &str, s: &FakeServer) -> Env {
    let env = Env::new(provider, s);
    fs::write(
        env.repo.join("Alpha.java"),
        "package p;\npublic class Alpha {}\n",
    )
    .unwrap();
    assert!(env.run(&["build", "--no-llm"], None).status.success());
    env
}

#[test]
fn req097_ac1_always_cut_gives_two_requests_and_a_warning() {
    for provider in ["openai", "anthropic"] {
        let s = FakeServer::start_replies((0..8).map(|_| cut()).collect());
        let env = Env::new(provider, &s);
        let o = env.run(&["build"], Some("10"));
        assert_ne!(o.status.code(), Some(0), "{provider}");
        assert_eq!(posts(&s).len(), 2, "{provider}");
        let w = warnings(&o);
        assert_eq!(w.len(), 1, "{provider}: {}", text(&o.stderr));
        assert!(w[0].contains("lecture de la réponse"), "{}", w[0]);
        assert!(!env.search_finds_summary(), "{provider}");
    }
}

#[test]
fn req097_ac1_cut_then_conforming_enriches() {
    for provider in ["openai", "anthropic"] {
        let s = FakeServer::start_replies(vec![cut(), good(provider)]);
        let env = Env::new(provider, &s);
        let o = env.run(&["build"], Some("10"));
        assert_eq!(o.status.code(), Some(0), "{provider}: {}", text(&o.stderr));
        assert_eq!(posts(&s).len(), 2, "{provider}");
        assert!(env.search_finds_summary(), "{provider}");
    }
}

#[test]
fn req097_ac1_ask_always_cut_gives_two_requests_and_an_error() {
    let s = FakeServer::start_replies((0..8).map(|_| cut()).collect());
    let env = ask_env("openai", &s);
    let o = env.run(&["query", "ask", "Alpha"], Some("10"));
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(o.stdout.is_empty());
    assert_eq!(posts(&s).len(), 2);
    let errs: Vec<String> = text(&o.stderr)
        .lines()
        .filter(|l| l.starts_with("erreur:"))
        .map(String::from)
        .collect();
    assert_eq!(errs.len(), 1, "{}", text(&o.stderr));
    assert!(errs[0].contains("lecture de la réponse"), "{}", errs[0]);
}

#[test]
fn req097_ac2_cut_then_503_gives_two_requests() {
    let s = FakeServer::start_replies(vec![cut(), Reply::status(503, "{}"), good("openai")]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], Some("10"));
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(posts(&s).len(), 2);
    assert!(!env.search_finds_summary());
}

#[test]
fn req097_ac2_two_503_then_cut_then_conforming_gives_four_requests() {
    let s = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(503, "{}"),
        cut(),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], Some("10"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(posts(&s).len(), 4);
    assert!(env.search_finds_summary());
}

#[test]
fn req097_ac2_four_503_then_cut_gives_five_requests() {
    let mut replies: Vec<Reply> = (0..4).map(|_| Reply::status(503, "{}")).collect();
    replies.push(cut());
    replies.push(good("openai"));
    let s = FakeServer::start_replies(replies);
    let env = Env::new("openai", &s);
    let o = env.run(&["build"], Some("10"));
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(posts(&s).len(), 5);
    assert!(!env.search_finds_summary());
}

#[test]
fn req097_ac3_wait_line_says_interrupted_read() {
    let s = FakeServer::start_replies(vec![cut(), good("openai")]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let w = waits(&o);
    assert_eq!(w.len(), 1, "{w:#?}");
    let (_, rest) = wait_parts(&w[0], &format!("{DOC} [1/1]"));
    assert_eq!(rest, "après lecture interrompue (nouvel essai 1/4)");
    assert!(!w[0].contains("erreur réseau"), "{}", w[0]);
}

#[test]
fn req097_ac4_waits_follow_the_cause_of_each_new_send() {
    let s = FakeServer::start_replies(vec![
        Reply::status(503, "{}"),
        Reply::status(503, "{}"),
        cut(),
        good("openai"),
    ]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(posts(&s).len(), 4);
    let w = waits(&o);
    assert_eq!(w.len(), 3, "{w:#?}");
    for (line, end) in w.iter().zip([
        "après HTTP 503 (nouvel essai 1/4)",
        "après HTTP 503 (nouvel essai 2/4)",
        "après lecture interrompue (nouvel essai 3/4)",
    ]) {
        assert!(line.ends_with(end), "{line}");
    }
}

#[test]
fn req097_ac4_cut_then_503_gives_one_wait_line_and_http_503_end() {
    let s = FakeServer::start_replies(vec![cut(), Reply::status(503, "{}"), good("openai")]);
    let env = Env::new("openai", &s);
    let o = env.run(&["build", "--verbose"], Some("10"));
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(posts(&s).len(), 2);
    let w = waits(&o);
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(
        w[0].ends_with("après lecture interrompue (nouvel essai 1/4)"),
        "{}",
        w[0]
    );
    let lines = trace_lines(&o);
    let end = &lines[position(&lines, "llm enrich fin")];
    assert!(end.contains("HTTP 503"), "{end}");
}
