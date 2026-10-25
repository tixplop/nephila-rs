//! Causes d'arrêt de l'envoi : requête refusée (REQ-091, DEC-099), redirection (REQ-092, DEC-100), contre le faux serveur.

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
        self.run_base(args, "10")
    }

    /// Comme `run`, avec `NEPHILA_RETRY_BASE_MS` = `base`.
    fn run_base(&self, args: &[&str], base: &str) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", self._tmp.path().join("config.toml"))
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", base)
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

fn redirect_path(provider: &str) -> &'static str {
    if provider == "anthropic" {
        "/v1/messages"
    } else {
        "/v1/chat/completions"
    }
}

/// Serveur A qui redirige toute requête vers B ; B ne doit jamais être contacté.
fn redirecting(provider: &str, status: u16, b: &FakeServer) -> FakeServer {
    let target = format!("{}{}", b.base_url, redirect_path(provider));
    FakeServer::start_replies(
        (0..8)
            .map(|_| Reply::with_header(status, "", "Location", &target))
            .collect(),
    )
}

fn assert_redirect_line(o: &Output, provider: &str, code: u16, dest: &str, ctx: &str) {
    let err = text(&o.stderr);
    let all = lines(o);
    let errors = error_lines(o);
    assert_eq!(errors.len(), 1, "{ctx}: {err}");
    assert_eq!(all.last().unwrap(), &errors[0], "{ctx}: {err}");
    assert_eq!(
        errors[0],
        format!("erreur: {provider} redirige (HTTP {code}) {dest} ; corrigez base_url"),
        "{ctx}"
    );
    assert!(!err.contains("panne réseau"), "{ctx}: {err}");
    assert!(!err.contains("envoi interrompu"), "{ctx}: {err}");
    assert!(!err.contains("avertissement:"), "{ctx}: {err}");
    assert_eq!(o.status.code(), Some(1), "{ctx}");
}

#[test]
fn req092_ac1_redirect_is_never_followed_build_and_ask() {
    for provider in ["openai", "anthropic"] {
        let b = FakeServer::start_replies(vec![]);
        let a = redirecting(provider, 302, &b);
        let env = Env::new(provider, &a, 3, 1);
        let o = env.run(&["build"]);
        assert_eq!(posts(&a), 1, "{provider}");
        assert_eq!(b.connections(), 0, "{provider}");
        let dest = format!("vers {}{}", b.base_url, redirect_path(provider));
        assert_redirect_line(&o, provider, 302, &dest, provider);

        let a = redirecting(provider, 302, &b);
        let env = Env::new(provider, &a, 1, 1);
        env.add_class();
        assert!(env.run(&["build", "--no-llm"]).status.success());
        let o = env.run(&["query", "ask", "Alpha"]);
        assert_eq!(posts(&a), 1, "ask {provider}");
        assert_eq!(b.connections(), 0, "ask {provider}");
        assert!(o.stdout.is_empty(), "ask {provider}");
        assert_eq!(lines(&o).len(), 1, "ask {provider}: {:?}", lines(&o));
        assert_redirect_line(&o, provider, 302, &dest, &format!("ask {provider}"));
    }
}

#[test]
fn req092_ac2_other_statuses_and_missing_location() {
    for code in [301u16, 307, 308] {
        let b = FakeServer::start_replies(vec![]);
        let a = redirecting("openai", code, &b);
        let env = Env::new("openai", &a, 3, 1);
        let o = env.run(&["build"]);
        assert_eq!(posts(&a), 1, "{code}");
        assert_eq!(b.connections(), 0, "{code}");
        let dest = format!("vers {}{}", b.base_url, redirect_path("openai"));
        assert_redirect_line(&o, "openai", code, &dest, &code.to_string());
    }
    for loc in [None, Some("  \t ")] {
        let reply = |_| match loc {
            None => Reply::status(302, ""),
            Some(v) => Reply::with_header(302, "", "Location", v),
        };
        let a = FakeServer::start_replies((0..6).map(reply).collect());
        let env = Env::new("openai", &a, 3, 1);
        let o = env.run(&["build"]);
        assert_eq!(posts(&a), 1, "{loc:?}");
        assert_redirect_line(&o, "openai", 302, "sans destination", &format!("{loc:?}"));
    }
}

#[test]
fn req092_ac4_long_location_is_truncated() {
    let long = format!("http://h/{}\t{}", "a".repeat(150), "b".repeat(150));
    let a = FakeServer::start_replies(
        (0..6)
            .map(|_| Reply::with_header(302, "", "Location", &long))
            .collect(),
    );
    let env = Env::new("openai", &a, 3, 1);
    let o = env.run(&["build"]);
    let last = lines(&o).last().unwrap().clone();
    let middle = last
        .strip_prefix("erreur: openai redirige (HTTP 302) vers ")
        .and_then(|r| r.strip_suffix(" ; corrigez base_url"))
        .unwrap_or_else(|| panic!("{last}"));
    assert!(
        middle.chars().count() <= 200 && middle.ends_with('…'),
        "{middle}"
    );
    assert!(!middle.contains('\t'));
}

#[test]
fn req092_ac3_verbose_trace_has_the_redirect_label_and_no_wait() {
    let b = FakeServer::start_replies(vec![]);
    let a = redirecting("openai", 302, &b);
    let env = Env::new("openai", &a, 3, 1);
    let o = env.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    let all = lines(&o);
    let end = all
        .iter()
        .find(|l| l.contains("llm enrich fin"))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(
        end.contains(" [1/1] requête redirigée ")
            && end.ends_with(" ms : openai redirige (HTTP 302)"),
        "{end}"
    );
    assert!(!err.contains("llm enrich attente"), "{err}");
    assert!(
        all.iter()
            .filter(|l| l.starts_with("trace:"))
            .all(|l| !l.contains(&b.base_url[b.base_url.rfind(':').unwrap()..])),
        "{err}"
    );
    assert!(
        all.iter().any(|l| l.ends_with(
            "politique: arrêt de l'envoi (requête redirigée par le fournisseur), 2 fichiers non envoyés"
        )),
        "{err}"
    );
}

// ---- Fournisseur saturé (REQ-093, DEC-099) et délais dépassés répétés (REQ-094) ----

const SATURATED_ERROR: &str = "erreur: envoi interrompu : fournisseur saturé : relancez nephila update plus tard ou réduisez concurrency ; 4 fichier(s) à enrichir";
const TIMEOUT_CAUSE: &str =
    "délais dépassés répétés : augmentez timeout_secs ou vérifiez le fournisseur";

fn saturated_run(provider: &str, status: u16, verbose: bool) -> (FakeServer, Output) {
    let s = FakeServer::start_replies(refusals(status, "{}", 20));
    let env = Env::new(provider, &s, 4, 1);
    let args: &[&str] = if verbose {
        &["build", "--verbose"]
    } else {
        &["build"]
    };
    let o = env.run_base(args, "1");
    (s, o)
}

#[test]
fn req093_ac1_ac4_saturation_stops_after_15_requests_with_e21() {
    for (provider, status) in [("openai", 429u16), ("openai", 529), ("anthropic", 529)] {
        let ctx = format!("{provider} {status}");
        let (s, o) = saturated_run(provider, status, false);
        let err = text(&o.stderr);
        assert_eq!(posts(&s), 15, "{ctx}: {err}");
        assert_eq!(error_lines(&o), [SATURATED_ERROR], "{ctx}: {err}");
        assert_eq!(lines(&o).last().unwrap(), SATURATED_ERROR, "{ctx}");
        assert!(!err.contains("panne réseau"), "{ctx}: {err}");
        let warnings: Vec<_> = lines(&o)
            .into_iter()
            .filter(|l| l.starts_with("avertissement:"))
            .collect();
        assert_eq!(warnings.len(), 3, "{ctx}: {err}");
        for w in &warnings {
            assert!(
                w.contains(&format!("erreur de {provider} (HTTP {status}) sur ")),
                "{ctx}: {w}"
            );
        }
        assert_eq!(o.status.code(), Some(1), "{ctx}");
    }
}

#[test]
fn req093_ac1_ac4_always_503_keeps_the_network_cause() {
    let s = FakeServer::start_replies(refusals(503, "{}", 20));
    let env = Env::new("openai", &s, 4, 1);
    let o = env.run_base(&["build", "--verbose"], "1");
    let err = text(&o.stderr);
    assert_eq!(posts(&s), 15, "{err}");
    assert_eq!(
        error_lines(&o),
        ["erreur: envoi interrompu : panne réseau répétée (3 erreurs réseau consécutives) ; 4 fichier(s) à enrichir"]
    );
    assert!(!err.contains("saturé"), "{err}");
    for k in 1..=3 {
        assert!(
            lines(&o)
                .iter()
                .any(|l| l.ends_with(&format!("politique: erreurs réseau consécutives {k}/3"))),
            "{err}"
        );
    }
}

#[test]
fn req093_ac3_verbose_counts_carry_the_saturation_mark() {
    let (_s, o) = saturated_run("openai", 429, true);
    let err = text(&o.stderr);
    let all = lines(&o);
    let mut at = 0;
    for k in 1..=3 {
        let end = format!("politique: erreurs réseau consécutives {k}/3 (fournisseur saturé)");
        at += all[at..]
            .iter()
            .position(|l| l.ends_with(&end))
            .unwrap_or_else(|| panic!("{end}: {err}"));
    }
    let stop = all
        .iter()
        .position(|l| {
            l.ends_with("politique: arrêt de l'envoi (fournisseur saturé), 1 fichiers non envoyés")
        })
        .unwrap_or_else(|| panic!("{err}"));
    assert!(stop > at);
    assert_eq!(all.last().unwrap(), SATURATED_ERROR);
    assert!(!err.contains("panne réseau"), "{err}");
}

/// Dépôt de documents pour le fournisseur simulé, avec son journal d'appels.
struct MockEnv {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    log: PathBuf,
}

impl MockEnv {
    fn new(docs: usize) -> MockEnv {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        for i in 0..docs {
            fs::write(
                repo.join(format!("docs/d{i}.md")),
                format!("# D{i}\n\nTexte {i}.\n"),
            )
            .unwrap();
        }
        fs::write(
            tmp.path().join("config.toml"),
            "provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 1\nbatch_files = 1\n",
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("mock")).unwrap();
        let log = tmp.path().join("mock.log");
        MockEnv {
            _tmp: tmp,
            repo,
            log,
        }
    }

    fn build(&self, timeouts: &[String], verbose: bool) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).arg("build");
        if verbose {
            c.arg("--verbose");
        }
        c.env("NEPHILA_CONFIG", self._tmp.path().join("config.toml"))
            .env("NEPHILA_MOCK_DIR", self._tmp.path().join("mock"))
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env("NEPHILA_MOCK_TIMEOUT_FILES", timeouts.join(","))
            .output()
            .unwrap()
    }

    /// Chemins du journal dans l'ordre d'envoi, sans doublon.
    fn order(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| l.split('\t').next().unwrap().to_string())
            .filter(|p| seen.insert(p.clone()))
            .collect()
    }

    /// Ordre d'envoi d'un build sans échec, puis graphe et journal remis à neuf.
    fn send_order(&self) -> Vec<String> {
        let o = self.build(&[], false);
        assert!(o.status.success(), "{}", text(&o.stderr));
        let order = self.order();
        fs::remove_dir_all(self.repo.join(".nephila")).unwrap();
        fs::remove_file(&self.log).unwrap();
        order
    }

    fn to_enrich(&self) -> usize {
        let o = Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(["status", "--json"])
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        v["to_enrich"].as_u64().unwrap() as usize
    }
}

const E22_6: &str = "erreur: envoi interrompu : délais dépassés répétés : augmentez timeout_secs ou vérifiez le fournisseur ; 6 fichier(s) à enrichir";

#[test]
fn req094_ac1_ac6_three_timeouts_stop_the_send_with_e22() {
    let env = MockEnv::new(6);
    let order = env.send_order();
    let o = env.build(&order[..3], false);
    let err = text(&o.stderr);
    assert_eq!(env.order(), order[..3], "{err}");
    let raw = fs::read_to_string(&env.log).unwrap();
    assert_eq!(raw.lines().count(), 3, "{raw}");
    assert_eq!(error_lines(&o), [E22_6], "{err}");
    assert_eq!(lines(&o).last().unwrap(), E22_6);
    assert!(!err.contains("panne réseau"), "{err}");
    let warnings = lines(&o)
        .into_iter()
        .filter(|l| l.starts_with("avertissement:"))
        .count();
    assert_eq!(warnings, 3, "{err}");
    assert_eq!(env.to_enrich(), 6);
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn req094_ac5_ac6_verbose_counts_then_the_stop_line() {
    let env = MockEnv::new(6);
    let order = env.send_order();
    let o = env.build(&order[..3], true);
    let err = text(&o.stderr);
    let all = lines(&o);
    let mut at = 0;
    for k in 1..=3 {
        let fin = all[at..]
            .iter()
            .position(|l| l.contains("llm enrich fin") && l.contains("délai dépassé"))
            .unwrap_or_else(|| panic!("{err}"));
        at += fin + 1;
        assert!(
            all[at].ends_with(&format!("politique: délais dépassés consécutifs {k}/3")),
            "{k}: {}",
            all[at]
        );
    }
    assert!(
        all[at + 1..].iter().any(|l| l.ends_with(
            "politique: arrêt de l'envoi (délais dépassés répétés), 3 fichiers non envoyés"
        )),
        "{err}"
    );
    assert!(!err.contains("erreurs réseau consécutives"), "{err}");
}

#[test]
fn req094_ac3_ac6_a_success_between_timeouts_does_not_stop() {
    let env = MockEnv::new(6);
    let order = env.send_order();
    let bad: Vec<String> = [0, 1, 3, 4].iter().map(|&i| order[i].clone()).collect();
    let o = env.build(&bad, true);
    let err = text(&o.stderr);
    assert_eq!(env.order(), order, "{err}");
    assert_eq!(env.to_enrich(), 4);
    assert!(!err.contains("arrêt de l'envoi"), "{err}");
    assert!(
        error_lines(&o)
            .iter()
            .all(|l| !l.contains("envoi interrompu")),
        "{err}"
    );
    let counts: Vec<String> = lines(&o)
        .iter()
        .filter_map(|l| l.split("politique: délais dépassés consécutifs ").nth(1))
        .map(String::from)
        .collect();
    assert_eq!(counts, ["1/3", "2/3", "1/3", "2/3"], "{err}");
}

#[test]
fn req094_ac2_slow_provider_gets_three_requests_only() {
    let slow = |n: usize| -> Vec<Reply> {
        (0..n)
            .map(|_| Reply::Http {
                status: 200,
                body: "{}".into(),
                headers: vec![],
                delay_ms: 2000,
            })
            .collect()
    };
    let s = FakeServer::start_replies(slow(5));
    let env = Env::new("openai", &s, 5, 1);
    let conf = env._tmp.path().join("config.toml");
    fs::write(
        &conf,
        fs::read_to_string(&conf).unwrap() + "timeout_secs = 1\n",
    )
    .unwrap();
    let t = std::time::Instant::now();
    let o = env.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert!(t.elapsed() < std::time::Duration::from_secs(10), "{err}");
    // Compte côté client : exactement 3 appels partis.
    let started = lines(&o)
        .iter()
        .filter(|l| l.contains("llm enrich départ"))
        .count();
    assert_eq!(started, 3, "{err}");
    // Le faux serveur sert une connexion à la fois (2 s chacune) : il enregistre la 3e requête,
    // déjà envoyée par le client, un peu après la fin de la commande.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while posts(&s) < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(posts(&s), 3, "{err}");
    assert_eq!(
        error_lines(&o),
        [format!(
            "erreur: envoi interrompu : {TIMEOUT_CAUSE} ; 5 fichier(s) à enrichir"
        )],
        "{err}"
    );
    assert_eq!(o.status.code(), Some(1));
}

const E12: &str =
    "erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env";

/// REQ-095 : première requête `status` avec `retry-after: 30`, seconde `401` 300 ms plus tard.
fn waiting_then_refused(status: u16, verbose: bool) -> (FakeServer, Output, std::time::Duration) {
    let s = FakeServer::start_replies(vec![
        Reply::with_header(status, "{}", "retry-after", "30"),
        Reply::Http {
            status: 401,
            body: "{}".into(),
            headers: vec![],
            delay_ms: 300,
        },
    ]);
    let env = Env::new("openai", &s, 2, 2);
    let args: &[&str] = if verbose {
        &["build", "--verbose"]
    } else {
        &["build"]
    };
    let t = std::time::Instant::now();
    let o = env.run_base(args, "10");
    let elapsed = t.elapsed();
    let status_out = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&env.repo)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&status_out.stdout).unwrap();
    assert_eq!(v["to_enrich"].as_u64(), Some(2), "{}", text(&o.stderr));
    (s, o, elapsed)
}

#[test]
fn req095_ac1_stop_interrupts_the_retry_wait() {
    for status in [503u16, 429] {
        let (s, o, elapsed) = waiting_then_refused(status, false);
        let err = text(&o.stderr);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "{status}: {elapsed:?} {err}"
        );
        assert_eq!(posts(&s), 2, "{status}: {err}");
        assert_eq!(error_lines(&o), [E12], "{status}: {err}");
        assert_eq!(lines(&o).last().unwrap(), E12, "{status}");
        assert_eq!(o.status.code(), Some(1), "{status}");
    }
}

#[test]
fn req095_ac4_verbose_has_one_wait_line_and_the_waiting_call_ends_after_the_policy_line() {
    let (_s, o, elapsed) = waiting_then_refused(503, true);
    let err = text(&o.stderr);
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "{elapsed:?} {err}"
    );
    let all = lines(&o);
    let waits: Vec<_> = all
        .iter()
        .filter(|l| l.contains("llm enrich attente"))
        .collect();
    assert_eq!(waits.len(), 1, "{err}");
    assert!(
        waits[0].ends_with("30000 ms après HTTP 503 (nouvel essai 1/4)"),
        "{}",
        waits[0]
    );
    let policy = all
        .iter()
        .position(|l| {
            l.contains("politique: arrêt de l'envoi (clé d'API refusée par le fournisseur)")
        })
        .unwrap_or_else(|| panic!("{err}"));
    let end = all
        .iter()
        .position(|l| l.contains("llm enrich fin") && l.contains(" réseau "))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(end > policy, "{err}");
    let ms: u64 = all[end]
        .split(" réseau ")
        .nth(1)
        .and_then(|r| r.split(" ms").next())
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("{}", all[end]));
    assert!(ms < 30000, "{}", all[end]);
    let warnings: Vec<_> = all
        .iter()
        .filter(|l| l.starts_with("avertissement:"))
        .collect();
    assert_eq!(warnings.len(), 1, "{err}");
    assert!(
        warnings[0].contains("erreur de openai (HTTP 503) sur "),
        "{err}"
    );
    assert_eq!(all.last().unwrap(), E12);
}

#[test]
fn req095_ac5_no_interrupted_line_and_warning_only_for_the_waiting_document() {
    for (status, shown) in [(503u16, "HTTP 503"), (429, "HTTP 429")] {
        let (_s, o, _) = waiting_then_refused(status, true);
        let err = text(&o.stderr);
        let all = lines(&o);
        for l in all
            .iter()
            .filter(|l| l.starts_with("trace:") && l.contains("llm enrich "))
        {
            assert!(
                l.contains(" départ ") || l.contains(" fin ") || l.contains(" attente "),
                "{l}"
            );
            assert!(!l.contains("interrompue"), "{l}");
        }
        let policy = all
            .iter()
            .position(|l| {
                l.contains("politique: arrêt de l'envoi (clé d'API refusée par le fournisseur), ")
            })
            .unwrap_or_else(|| panic!("{err}"));
        let end = all
            .iter()
            .position(|l| l.contains("llm enrich fin") && l.contains(" réseau "))
            .unwrap_or_else(|| panic!("{err}"));
        assert!(end > policy, "{err}");
        assert!(all[end].contains(shown), "{}", all[end]);
        let ms: u64 = all[end]
            .split(" réseau ")
            .nth(1)
            .and_then(|r| r.split(" ms").next())
            .and_then(|n| n.trim().parse().ok())
            .unwrap();
        assert!(ms < 2000, "{}", all[end]);

        let (_s, o, _) = waiting_then_refused(status, false);
        let err = text(&o.stderr);
        let warnings: Vec<_> = lines(&o)
            .into_iter()
            .filter(|l| l.starts_with("avertissement:"))
            .collect();
        assert_eq!(warnings.len(), 1, "{err}");
        assert!(
            warnings[0].contains(&format!("erreur de openai ({shown}) sur ")),
            "{err}"
        );
        assert_eq!(lines(&o).last().unwrap(), E12, "{err}");
        assert!(
            error_lines(&o).iter().all(|l| !l.contains("saturé")),
            "{err}"
        );
    }
}
