//! Totaux des appels de `build` : suffixe de jetons des lignes `llm enrich fin`, ligne
//! `appels:` (REQ-165, REQ-026, DEC-134), mesurés avec le LLM simulé.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

const CONFIG: &str =
    "provider = \"mock\"\nenrich_code = true\nbatch_files = 1\nmodel = \"mock\"\nconcurrency = 1\n";

impl Env {
    fn empty(config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&mock).unwrap();
        let cfg = tmp.path().join("config.toml");
        fs::write(&cfg, config).unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config: cfg,
        }
    }

    /// Dépôt exemple (maven2) et fixtures du simulé.
    fn example(config: &str) -> Env {
        let env = Env::empty(config);
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir(&fx.join("maven2"), &env.repo);
        copy_dir(&fx.join("mock"), &env.mock);
        env
    }

    /// `n` fichiers Java, chacun avec sa fixture.
    fn files(n: usize, config: &str) -> Env {
        let env = Env::empty(config);
        fs::create_dir_all(env.repo.join("src")).unwrap();
        fs::create_dir_all(env.mock.join("src")).unwrap();
        for i in 0..n {
            fs::write(
                env.repo.join(format!("src/F{i:02}.java")),
                format!("package p;\npublic class F{i:02} {{}}\n"),
            )
            .unwrap();
            fs::write(
                env.mock.join(format!("src/F{i:02}.java.json")),
                format!(
                    r#"{{"version":2,"summaries":[{{"node":"type:p.F{i:02}","summary":"résumé {i:02}"}}],"concepts":[]}}"#
                ),
            )
            .unwrap();
        }
        env
    }

    fn run(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        for v in [
            "NEPHILA_MOCK_FAIL_AFTER",
            "NEPHILA_MOCK_DELAY_MS",
            "NEPHILA_MOCK_INVALID_KEY",
            "NEPHILA_MOCK_TIMEOUT_FILES",
            "NEPHILA_MOCK_KEY_REVOKED_AFTER",
            "NEPHILA_MOCK_MALFORMED",
            "NO_COLOR",
        ] {
            c.env_remove(v);
        }
        c.env("NEPHILA_RETRY_BASE_MS", "1");
        c.arg("-C").arg(&self.repo).args(args);
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn log_len(&self) -> usize {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Lignes de trace, horodatage `trace: [<s>.<mmm>s] ` retiré.
fn traces(err: &str) -> Vec<String> {
    err.lines()
        .filter_map(|l| l.strip_prefix("trace: ["))
        .filter_map(|l| l.split_once("s] ").map(|(_, rest)| rest.to_string()))
        .collect()
}

fn calls_lines(err: &str) -> Vec<String> {
    traces(err)
        .into_iter()
        .filter(|l| l.starts_with("appels:"))
        .collect()
}

#[test]
fn req165_ac7_mock_has_no_token_suffix_and_unknown_tokens() {
    let env = Env::example(CONFIG);
    let o = env.run(&["build", "--verbose"], &[]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(!err.contains("(jetons :"), "{err}");
    let n = env.log_len();
    assert!(n > 0);
    assert_eq!(
        calls_lines(&err),
        vec![format!(
            "appels: {n} appels, 0 nouveaux essais de format, {n} envois HTTP, jetons : inconnus"
        )],
        "{err}"
    );
}

#[test]
fn req165_ac8_one_line_right_after_enrichment_end() {
    let env = Env::example(CONFIG);
    let o = env.run(&["build", "--verbose"], &[]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    let t = traces(&err);
    let ends: Vec<usize> = (0..t.len())
        .filter(|&i| t[i].starts_with("enrichissement: fin"))
        .collect();
    assert_eq!(ends.len(), 1, "{err}");
    assert_eq!(calls_lines(&err).len(), 1);
    assert!(t[ends[0] + 1].starts_with("appels: "), "{err}");

    // Second build : plus rien à enrichir, ni fin de phase ni bilan.
    let before = env.log_len();
    let o = env.run(&["build", "--verbose"], &[]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert_eq!(env.log_len(), before);
    assert!(!err.contains("enrichissement: fin"), "{err}");
    assert!(calls_lines(&err).is_empty(), "{err}");
}

#[test]
fn req165_ac8_no_llm_has_no_calls_line() {
    let env = Env::example(CONFIG);
    let o = env.run(&["build", "--no-llm", "--verbose"], &[]);
    assert!(o.status.success());
    assert!(calls_lines(&stderr(&o)).is_empty(), "{}", stderr(&o));
}

#[test]
fn req165_ac8_repo_without_node_to_request_gives_zero_line() {
    let env = Env::empty(CONFIG);
    fs::create_dir_all(env.repo.join("web/src")).unwrap();
    fs::write(env.repo.join("web/src/side.js"), "console.log(1);\n").unwrap();
    let o = env.run(&["build", "--verbose"], &[]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert_eq!(env.log_len(), 0);
    let t = traces(&err);
    let i = t
        .iter()
        .position(|l| l.starts_with("enrichissement: fin"))
        .unwrap_or_else(|| panic!("{err}"));
    assert_eq!(
        t[i + 1],
        "appels: 0 appels, 0 nouveaux essais de format, 0 envois HTTP, jetons : entrée 0 dont cache 0, sortie 0"
    );
    assert_eq!(calls_lines(&err).len(), 1);
}

#[test]
fn req165_ac8_send_stop_still_writes_line_and_counts_started_calls() {
    let env = Env::files(8, CONFIG);
    let o = env.run(&["build", "--verbose"], &[("NEPHILA_MOCK_FAIL_AFTER", "2")]);
    let err = stderr(&o);
    let t = traces(&err);
    let i = t
        .iter()
        .position(|l| l.starts_with("enrichissement: fin"))
        .unwrap_or_else(|| panic!("{err}"));
    let started = t
        .iter()
        .filter(|l| l.starts_with("llm enrich départ"))
        .count();
    assert!(started >= 3, "{err}");
    assert!(
        t[i + 1].starts_with(&format!("appels: {started} appels, ")),
        "{started} {err}"
    );
    assert_eq!(calls_lines(&err).len(), 1);
}

#[test]
fn req165_ac14_format_retries_are_counted() {
    let env = Env::files(4, CONFIG);
    let o = env.run(
        &["build", "--verbose"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F01.java:2")],
    );
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert_eq!(
        calls_lines(&err),
        vec![
            "appels: 6 appels, 2 nouveaux essais de format, 6 envois HTTP, jetons : inconnus"
                .to_string()
        ],
        "{err}"
    );
}

#[test]
fn req165_ac15_silent_without_verbose_and_same_lines_with_or_without_color() {
    let env = Env::example(CONFIG);
    let o = env.run(&["build"], &[]);
    let (out, err) = (stdout(&o), stderr(&o));
    assert!(o.status.success(), "{err}");
    assert!(!err.contains("appels:") && !err.contains("jetons"), "{err}");
    assert!(!out.contains("appels:") && !out.contains("jetons"), "{out}");

    let a = Env::example(CONFIG);
    let oa = a.run(&["build", "--verbose"], &[]);
    let b = Env::example(CONFIG);
    let ob = b.run(&["build", "--verbose"], &[("NO_COLOR", "1")]);
    for o in [&oa, &ob] {
        assert!(!o.stdout.contains(&0x1b) && !o.stderr.contains(&0x1b));
    }
    assert_eq!(calls_lines(&stderr(&oa)), calls_lines(&stderr(&ob)));
    assert_eq!(calls_lines(&stderr(&oa)).len(), 1);
}

// ---- Faux serveur HTTP : openai et anthropic (AC-165-3 à AC-165-15) ----

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const KEY_VAR: &str = "NEPHILA_T252_KEY";

/// Usage normalisé d'un appel : entrée (cache compris), cache, sortie, réflexion.
#[derive(Clone, Copy)]
struct U {
    entry: u64,
    cache: u64,
    out: u64,
    reasoning: Option<u64>,
}

const fn u(entry: u64, cache: u64, out: u64) -> U {
    U {
        entry,
        cache,
        out,
        reasoning: None,
    }
}

/// Corps 200 conforme du fournisseur `p` : texte `text`, arrêt `stop`, `usage` éventuel.
fn body(p: &str, text: &str, stop: Option<&str>, usage: Option<U>) -> String {
    let mut v = if p == "anthropic" {
        serde_json::json!({"content":[{"type":"text","text":text}]})
    } else {
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]})
    };
    if let Some(s) = stop {
        if p == "anthropic" {
            v["stop_reason"] = s.into();
        } else {
            v["choices"][0]["finish_reason"] = s.into();
        }
    }
    if let Some(u) = usage {
        v["usage"] = if p == "anthropic" {
            serde_json::json!({
                "input_tokens": u.entry - u.cache,
                "cache_read_input_tokens": u.cache,
                "cache_creation_input_tokens": 0,
                "output_tokens": u.out
            })
        } else {
            let mut o = serde_json::json!({
                "prompt_tokens": u.entry,
                "prompt_tokens_details": {"cached_tokens": u.cache},
                "completion_tokens": u.out
            });
            if let Some(r) = u.reasoning {
                o["completion_tokens_details"] = serde_json::json!({"reasoning_tokens": r});
            }
            o
        };
    }
    v.to_string()
}

fn ok_stop(p: &str) -> Option<&'static str> {
    Some(if p == "anthropic" { "end_turn" } else { "stop" })
}

fn cut_stop(p: &str) -> Option<&'static str> {
    Some(if p == "anthropic" {
        "max_tokens"
    } else {
        "length"
    })
}

const EMPTY: &str = r#"{"version": 2, "summaries": [], "concepts": []}"#;
const D_GOOD: &str = r#"{"version": 2, "summaries": [{"node": "document:docs/d.md", "summary": "Résumé du document D"}], "concepts": []}"#;
const CUT: &str = r#"{"version": 2, "summaries": [{"node": "type:A", "summ"#;

/// Scénario : dépôt, configuration et réponses (selon que les réponses portent `usage`).
struct Scn {
    provider: &'static str,
    files: Vec<(String, String)>,
    cfg: &'static str,
    replies: Box<dyn Fn(bool) -> Vec<(u16, String)>>,
}

struct Run {
    out: Output,
    url: String,
    requests: usize,
    _tmp: tempfile::TempDir,
}

impl Run {
    /// stderr, port du faux serveur remplacé.
    fn err(&self) -> String {
        stderr(&self.out).replace(&self.url, "<URL>")
    }
}

fn docs(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| (format!("docs/d{i}.md"), format!("# D{i}\n\nTexte {i}.\n")))
        .collect()
}

fn one_doc() -> Vec<(String, String)> {
    vec![("docs/d.md".into(), "# D\n\nTexte du document.\n".into())]
}

fn java_files(names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|n| (format!("{n}.java"), format!("public class {n} {{}}\n")))
        .collect()
}

fn run_scn(s: &Scn, with_usage: bool, args: &[&str], envs: &[(&str, &str)]) -> Run {
    let server = FakeServer::start((s.replies)(with_usage));
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    for (p, c) in &s.files {
        let path = repo.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, c).unwrap();
    }
    let cfg = tmp.path().join("config.toml");
    fs::write(
        &cfg,
        format!(
            "provider = \"{}\"\nenrich_code = true\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\n{}",
            s.provider, server.base_url, s.cfg
        ),
    )
    .unwrap();
    let mut c = Command::new(cargo_bin("nephila"));
    c.env_remove("NO_COLOR")
        .env("NEPHILA_RETRY_BASE_MS", "10")
        .env("NEPHILA_CONFIG", &cfg)
        .env(KEY_VAR, "sk-test")
        .arg("-C")
        .arg(&repo)
        .args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    let requests = server.requests().len();
    Run {
        out,
        url: server.base_url.clone(),
        requests,
        _tmp: tmp,
    }
}

/// Lignes `llm enrich fin`, horodatage retiré.
fn fin_lines(err: &str) -> Vec<String> {
    traces(err)
        .into_iter()
        .filter(|l| l.starts_with("llm enrich fin"))
        .collect()
}

/// Sûreté (AC-165-10, AC-165-14, AC-165-13) : vrai pour tout scénario `--verbose`.
fn check_safety(err: &str) {
    for l in calls_lines(err) {
        for bad in ["usage", "tokens", "llm enrich", "/", "(dont"] {
            assert!(!l.contains(bad), "{bad} dans {l}");
        }
    }
    for l in traces(err) {
        if l.starts_with("llm enrich fin") || l.starts_with("politique:") {
            for kind in [
                "délai dépassé",
                " réseau ",
                "clé refusée",
                "requête refusée",
                "requête redirigée",
            ] {
                if l.contains(kind) {
                    assert!(!l.contains("(jetons :"), "{l}");
                }
            }
        }
        if l.starts_with("llm enrich départ") || l.starts_with("llm enrich attente") {
            assert!(!l.contains("(jetons :"), "{l}");
        }
    }
    assert!(!err.as_bytes().contains(&0x1b));
}

/// Silence par défaut et sorties égales à celles du même scénario sans `usage`
/// (AC-165-9, AC-165-15).
fn check_plain_identical(s: &Scn) {
    let a = run_scn(s, true, &["build"], &[]);
    let b = run_scn(s, false, &["build"], &[]);
    assert_eq!(a.out.status.code(), b.out.status.code());
    assert_eq!(stdout(&a.out), stdout(&b.out));
    assert_eq!(a.err(), b.err());
    assert_eq!(a.requests, b.requests);
    let (out, err) = (stdout(&a.out), a.err());
    assert!(!out.contains("jetons") && !out.contains("appels:"), "{out}");
    for l in err.lines() {
        assert!(
            !l.contains("jetons :") && !l.starts_with("appels:") && !l.starts_with("trace:"),
            "{l}"
        );
    }
    assert!(!a.out.stdout.contains(&0x1b) && !a.out.stderr.contains(&0x1b));
}

fn scn_six(p: &'static str, reasoning: Option<u64>) -> Scn {
    Scn {
        provider: p,
        files: docs(6),
        cfg: "concurrency = 4\nbatch_files = 1\n",
        replies: Box::new(move |with| {
            let usage = with.then_some(U {
                entry: 1000,
                cache: 200,
                out: 50,
                reasoning,
            });
            (0..6)
                .map(|_| (200, body(p, EMPTY, ok_stop(p), usage)))
                .collect()
        }),
    }
}

#[test]
fn req165_ac4_ac13_ac14_six_documents_each_provider() {
    for (p, reasoning) in [("openai", None), ("anthropic", None), ("openai", Some(10))] {
        let s = scn_six(p, reasoning);
        let r = run_scn(&s, true, &["build", "--verbose"], &[]);
        let err = r.err();
        assert!(r.out.status.success(), "{p} {err}");
        let tail = match reasoning {
            None => " ms (jetons : entrée 1000 dont cache 200, sortie 50)",
            Some(_) => " ms (jetons : entrée 1000 dont cache 200, sortie 50 dont réflexion 10)",
        };
        let ok: Vec<String> = fin_lines(&err)
            .into_iter()
            .filter(|l| l.contains(" ok "))
            .collect();
        assert_eq!(ok.len(), 6, "{p} {err}");
        for l in &ok {
            assert!(l.ends_with(tail), "{p} {l}");
        }
        let h = r.requests;
        assert_eq!(h, 6);
        let sortie = match reasoning {
            None => "sortie 300".to_string(),
            Some(_) => "sortie 300 dont réflexion 60".to_string(),
        };
        assert_eq!(
            calls_lines(&err),
            vec![format!(
                "appels: 6 appels, 0 nouveaux essais de format, {h} envois HTTP, jetons : entrée 6000 dont cache 1200, {sortie}"
            )],
            "{p} {err}"
        );
        // La ligne de bilan suit immédiatement `enrichissement: fin`.
        let t = traces(&err);
        let i = t
            .iter()
            .position(|l| l.starts_with("enrichissement: fin"))
            .unwrap();
        assert!(t[i + 1].starts_with("appels: "), "{err}");
        check_safety(&err);
    }
}

#[test]
fn req165_ac9_ac15_six_documents_plain_output_unchanged() {
    for (p, reasoning) in [("openai", None), ("anthropic", None), ("openai", Some(10))] {
        check_plain_identical(&scn_six(p, reasoning));
    }
}

#[test]
fn req165_ac15_calls_lines_same_with_and_without_no_color() {
    for p in ["openai", "anthropic"] {
        let s = scn_six(p, None);
        let a = run_scn(&s, true, &["build", "--verbose"], &[]);
        let b = run_scn(&s, true, &["build", "--verbose"], &[("NO_COLOR", "1")]);
        for r in [&a, &b] {
            assert!(r.out.status.success(), "{}", r.err());
            assert!(!r.out.stdout.contains(&0x1b) && !r.out.stderr.contains(&0x1b));
            assert!(!stdout(&r.out).contains("jetons"));
        }
        assert_eq!(calls_lines(&a.err()), calls_lines(&b.err()));
        assert_eq!(calls_lines(&a.err()).len(), 1);
    }
}

fn scn_retry_ok(p: &'static str) -> Scn {
    Scn {
        provider: p,
        files: one_doc(),
        cfg: "concurrency = 1\nbatch_files = 1\n",
        replies: Box::new(move |with| {
            vec![
                (503, "{}".into()),
                (503, "{}".into()),
                (
                    200,
                    body(p, D_GOOD, ok_stop(p), with.then_some(u(100, 0, 10))),
                ),
            ]
        }),
    }
}

fn scn_retry_500(p: &'static str) -> Scn {
    Scn {
        provider: p,
        files: one_doc(),
        cfg: "concurrency = 1\nbatch_files = 1\n",
        replies: Box::new(|_| (0..8).map(|_| (500, "{}".to_string())).collect()),
    }
}

#[test]
fn req165_ac5_ac14_retries_count_sends() {
    for p in ["openai", "anthropic"] {
        let r = run_scn(&scn_retry_ok(p), true, &["build", "--verbose"], &[]);
        let err = r.err();
        assert!(r.out.status.success(), "{p} {err}");
        assert_eq!(r.requests, 3);
        assert_eq!(
            calls_lines(&err),
            vec![
                "appels: 1 appels, 0 nouveaux essais de format, 3 envois HTTP, jetons : entrée 100 dont cache 0, sortie 10"
                    .to_string()
            ],
            "{p} {err}"
        );
        let fins = fin_lines(&err);
        assert_eq!(fins.len(), 1, "{err}");
        assert!(
            fins[0].ends_with(" ms (jetons : entrée 100 dont cache 0, sortie 10)"),
            "{}",
            fins[0]
        );
        check_safety(&err);

        let r = run_scn(&scn_retry_500(p), true, &["build", "--verbose"], &[]);
        let err = r.err();
        assert!(!r.out.status.success(), "{p}");
        assert_eq!(r.requests, 5);
        assert_eq!(
            calls_lines(&err),
            vec![
                "appels: 1 appels, 0 nouveaux essais de format, 5 envois HTTP, jetons : entrée 0 dont cache 0, sortie 0"
                    .to_string()
            ],
            "{p} {err}"
        );
        assert!(fin_lines(&err).iter().all(|l| !l.contains("(jetons :")));
        check_safety(&err);
    }
}

#[test]
fn req165_ac9_retry_scenarios_plain_output_unchanged() {
    for p in ["openai", "anthropic"] {
        check_plain_identical(&scn_retry_ok(p));
        check_plain_identical(&scn_retry_500(p));
    }
}

fn scn_format(p: &'static str) -> Scn {
    Scn {
        provider: p,
        files: one_doc(),
        cfg: "concurrency = 1\nbatch_files = 1\nformat_retries = 2\n",
        replies: Box::new(move |with| {
            vec![
                (
                    200,
                    body(
                        p,
                        r#"{"version": 1, "summaries": "pas une liste", "concepts": []}"#,
                        ok_stop(p),
                        with.then_some(u(100, 0, 10)),
                    ),
                ),
                (
                    200,
                    body(p, D_GOOD, ok_stop(p), with.then_some(u(120, 0, 12))),
                ),
            ]
        }),
    }
}

#[test]
fn req165_ac6_ac13_ac14_format_retry_has_own_values() {
    for p in ["openai", "anthropic"] {
        let r = run_scn(&scn_format(p), true, &["build", "--verbose"], &[]);
        let err = r.err();
        assert!(r.out.status.success(), "{p} {err}");
        assert_eq!(r.requests, 2);
        let fins = fin_lines(&err);
        assert_eq!(fins.len(), 2, "{err}");
        assert!(
            fins[0].contains(" erreur ") && fins[0].contains(" : réponse LLM non conforme"),
            "{}",
            fins[0]
        );
        assert!(
            fins[0].ends_with(" (jetons : entrée 100 dont cache 0, sortie 10)"),
            "{}",
            fins[0]
        );
        assert!(
            fins[1].ends_with(" (jetons : entrée 120 dont cache 0, sortie 12)"),
            "{}",
            fins[1]
        );
        assert_eq!(
            calls_lines(&err),
            vec![
                "appels: 2 appels, 1 nouveaux essais de format, 2 envois HTTP, jetons : entrée 220 dont cache 0, sortie 22"
                    .to_string()
            ],
            "{p} {err}"
        );
        check_safety(&err);
    }
}

#[test]
fn req165_ac9_format_scenario_plain_output_unchanged() {
    for p in ["openai", "anthropic"] {
        check_plain_identical(&scn_format(p));
    }
}

fn scn_truncated(p: &'static str) -> Scn {
    Scn {
        provider: p,
        files: java_files(&["A"]),
        cfg: "concurrency = 1\nbatch_files = 1\n",
        replies: Box::new(move |with| {
            (0..4)
                .map(|_| {
                    (
                        200,
                        body(p, CUT, cut_stop(p), with.then_some(u(300, 40, 77))),
                    )
                })
                .collect()
        }),
    }
}

#[test]
fn req165_ac6_ac13_truncated_reply_is_measured() {
    for p in ["openai", "anthropic"] {
        let r = run_scn(&scn_truncated(p), true, &["build", "--verbose"], &[]);
        let err = r.err();
        assert!(!r.out.status.success(), "{p}");
        assert_eq!(r.requests, 1, "{p} (AC-046-1 : un seul envoi)");
        let fins = fin_lines(&err);
        assert_eq!(fins.len(), 1, "{err}");
        assert!(
            fins[0].contains(" tronquée ")
                && fins[0].ends_with(
                    ": réponse tronquée par la limite de jetons (jetons : entrée 300 dont cache 40, sortie 77)"
                ),
            "{}",
            fins[0]
        );
        assert_eq!(
            calls_lines(&err),
            vec![
                "appels: 1 appels, 0 nouveaux essais de format, 1 envois HTTP, jetons : entrée 300 dont cache 40, sortie 77"
                    .to_string()
            ],
            "{p} {err}"
        );
        check_safety(&err);
    }
}

#[test]
fn req165_ac3_ac9_truncated_errors_identical_with_usage() {
    for p in ["openai", "anthropic"] {
        check_plain_identical(&scn_truncated(p));
        let a = run_scn(&scn_truncated(p), true, &["build"], &[]);
        assert!(
            a.err()
                .contains("réponse tronquée par la limite de jetons ; réduisez max_chunk_chars"),
            "{}",
            a.err()
        );
    }
}

fn lot_body(names: &[&str]) -> String {
    let files: Vec<serde_json::Value> = names
        .iter()
        .map(|n| {
            serde_json::json!({
                "file": format!("{n}.java"),
                "summaries": [{"node": format!("type:{n}"), "summary": format!("Classe {n}.")}],
                "concepts": []
            })
        })
        .collect();
    serde_json::json!({"version": 3, "files": files}).to_string()
}

fn scn_split(p: &'static str) -> Scn {
    Scn {
        provider: p,
        files: java_files(&["A", "B", "C", "D"]),
        cfg: "concurrency = 1\nbatch_files = 4\n",
        replies: Box::new(move |with| {
            let ab = lot_body(&["A", "B"]);
            let cd = lot_body(&["C", "D"]);
            vec![
                (
                    200,
                    body(p, CUT, cut_stop(p), with.then_some(u(900, 100, 60))),
                ),
                (200, body(p, &ab, ok_stop(p), with.then_some(u(500, 0, 30)))),
                (
                    200,
                    body(p, &cd, ok_stop(p), with.then_some(u(400, 50, 20))),
                ),
            ]
        }),
    }
}

#[test]
fn req165_ac6_split_batch_each_call_has_own_values_and_total_sums() {
    for p in ["openai", "anthropic"] {
        let r = run_scn(&scn_split(p), true, &["build", "--verbose"], &[]);
        let err = r.err();
        assert!(r.out.status.success(), "{p} {err}");
        assert_eq!(r.requests, 3);
        let fins = fin_lines(&err);
        assert_eq!(fins.len(), 3, "{err}");
        assert!(fins[0].contains(" tronquée "), "{}", fins[0]);
        assert!(
            fins[0].ends_with("(jetons : entrée 900 dont cache 100, sortie 60)"),
            "{}",
            fins[0]
        );
        assert!(
            fins[1].ends_with("(jetons : entrée 500 dont cache 0, sortie 30)"),
            "{}",
            fins[1]
        );
        assert!(
            fins[2].ends_with("(jetons : entrée 400 dont cache 50, sortie 20)"),
            "{}",
            fins[2]
        );
        assert_eq!(
            calls_lines(&err),
            vec![
                "appels: 3 appels, 0 nouveaux essais de format, 3 envois HTTP, jetons : entrée 1800 dont cache 150, sortie 110"
                    .to_string()
            ],
            "{p} {err}"
        );
        check_safety(&err);
    }
}

#[test]
fn req165_ac9_split_batch_plain_output_unchanged() {
    for p in ["openai", "anthropic"] {
        check_plain_identical(&scn_split(p));
    }
}

#[test]
fn req165_ac3_non_conforming_replies_with_usage_keep_the_same_errors() {
    for p in ["openai", "anthropic"] {
        // Réponse 200 sans message / sans bloc texte, avec `usage`.
        let empty = move |with: bool| {
            let usage = with.then_some(u(100, 0, 10));
            let mut v: serde_json::Value = if p == "anthropic" {
                serde_json::json!({"content": []})
            } else {
                serde_json::json!({"choices": []})
            };
            let ref_body = body(p, "x", None, usage);
            let parsed: serde_json::Value = serde_json::from_str(&ref_body).unwrap();
            if let Some(us) = parsed.get("usage") {
                v["usage"] = us.clone();
            }
            v.to_string()
        };
        let s = Scn {
            provider: p,
            files: one_doc(),
            cfg: "concurrency = 1\nbatch_files = 1\nformat_retries = 0\n",
            replies: Box::new(move |with| (0..4).map(|_| (200, empty(with))).collect()),
        };
        let a = run_scn(&s, true, &["build"], &[]);
        let b = run_scn(&s, false, &["build"], &[]);
        assert!(!a.out.status.success(), "{p}");
        assert_eq!(a.err(), b.err(), "{p}");
        assert_eq!(stdout(&a.out), stdout(&b.out), "{p}");
        // Mesuré comme un appel avec réponse et usage.
        let v = run_scn(&s, true, &["build", "--verbose"], &[]);
        let err = v.err();
        let line = calls_lines(&err);
        assert_eq!(line.len(), 1, "{err}");
        assert!(
            line[0].ends_with("jetons : entrée 100 dont cache 0, sortie 10"),
            "{p} {}",
            line[0]
        );
        check_safety(&err);
    }
}

#[test]
fn req165_ac7_ac14_one_reply_in_three_without_usage() {
    for p in ["openai", "anthropic"] {
        let s = Scn {
            provider: p,
            files: docs(3),
            cfg: "concurrency = 1\nbatch_files = 1\n",
            replies: Box::new(move |with| {
                let usage = with.then_some(u(100, 0, 10));
                vec![
                    (200, body(p, EMPTY, ok_stop(p), usage)),
                    (200, body(p, EMPTY, ok_stop(p), None)),
                    (200, body(p, EMPTY, ok_stop(p), usage)),
                ]
            }),
        };
        let r = run_scn(&s, true, &["build", "--verbose"], &[]);
        let err = r.err();
        assert!(r.out.status.success(), "{p} {err}");
        let line = calls_lines(&err);
        assert_eq!(line.len(), 1, "{err}");
        assert!(
            line[0].ends_with(" (1 appels sans compte de jetons)"),
            "{}",
            line[0]
        );
        assert!(!line[0].contains("inconnus"), "{}", line[0]);
        assert!(
            line[0].contains("jetons : entrée 200 dont cache 0, sortie 20"),
            "{}",
            line[0]
        );
        // Une seule des trois lignes `llm enrich fin` n'a pas de suffixe.
        let fins = fin_lines(&err);
        assert_eq!(fins.iter().filter(|l| l.contains("(jetons :")).count(), 2);
        check_safety(&err);
        check_plain_identical(&s);
    }
}

#[test]
fn req165_ac13_refused_key_line_has_no_token_suffix() {
    let s = Scn {
        provider: "openai",
        files: one_doc(),
        cfg: "concurrency = 1\nbatch_files = 1\n",
        replies: Box::new(|with| {
            let b = if with {
                r#"{"error":{"message":"no"},"usage":{"prompt_tokens":5,"completion_tokens":1}}"#
            } else {
                r#"{"error":{"message":"no"}}"#
            };
            vec![(401, b.to_string())]
        }),
    };
    let r = run_scn(&s, true, &["build", "--verbose"], &[]);
    let err = r.err();
    assert!(!r.out.status.success());
    assert!(
        fin_lines(&err).iter().any(|l| l.contains("clé refusée")),
        "{err}"
    );
    check_safety(&err);
    check_plain_identical(&s);
}

#[test]
fn req165_ac10_no_secret_in_traces_with_usage() {
    for p in ["openai", "anthropic"] {
        let long = format!("SECRET_REPONSE_42 {}", "x".repeat(5000));
        let s = Scn {
            provider: p,
            files: vec![(
                "A.java".into(),
                "class A { String s = \"SECRET_CONTENU_42\"; }\n".into(),
            )],
            cfg: "concurrency = 1\nbatch_files = 1\n",
            replies: Box::new(move |_| {
                (0..4)
                    .map(|_| (200, body(p, &long, ok_stop(p), Some(u(100, 0, 10)))))
                    .collect()
            }),
        };
        let r = run_scn(&s, true, &["build", "-v"], &[(KEY_VAR, "sk-test-SECRET")]);
        let err = r.err();
        let lines: Vec<&str> = err.lines().filter(|l| l.starts_with("trace:")).collect();
        assert!(lines.iter().any(|l| l.contains("(jetons :")), "{err}");
        assert!(lines.iter().any(|l| l.contains("appels: ")), "{err}");
        for l in &lines {
            for m in ["SECRET_CONTENU_42", "SECRET_REPONSE_42", "sk-test-SECRET"] {
                assert!(!l.contains(m), "{l}");
            }
        }
        check_safety(&err);
    }
}

// ---- REQ-166 : ligne `appels:` de `query ask --verbose` ----

const ASK_Q: &str = "Que fait BillingService ?";
const ASK_JSON: &str = r#"{"found":true,"answer":"Il facture.","nodes":["BillingService"]}"#;
const ASK_KEY: &str = "NEPHILA_T253_KEY";

/// Dépôt indexé sans LLM, configuration du fournisseur `provider` (`mock` : dossier simulé).
struct AskEnv {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
}

impl AskEnv {
    fn new(provider: &str, base_url: &str) -> AskEnv {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(repo.join("billing")).unwrap();
        fs::create_dir_all(&mock).unwrap();
        fs::write(
            repo.join("billing/BillingService.java"),
            "package com.acme.billing;\n\npublic class BillingService {\n    public void charge() {}\n}\n",
        )
        .unwrap();
        fs::write(mock.join("_ask.json"), ASK_JSON).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let env = AskEnv {
            _tmp: tmp,
            repo,
            config,
            mock,
        };
        let o = env.cmd(&["build", "--no-llm"], &[]);
        assert!(o.status.success(), "{}", stderr(&o));
        fs::write(
            &env.config,
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{ASK_KEY}\"\nbase_url = \"{base_url}\"\n"
            ),
        )
        .unwrap();
        env
    }

    fn cmd(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.env_remove("NO_COLOR")
            .env_remove("NEPHILA_MOCK_MALFORMED")
            .env("NEPHILA_RETRY_BASE_MS", "10")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env(ASK_KEY, "sk-test")
            .arg("-C")
            .arg(&self.repo)
            .args(args);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ask(&self, extra: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut a = vec!["query", "ask", ASK_Q];
        a.extend_from_slice(extra);
        self.cmd(&a, envs)
    }
}

fn ask_http(provider: &str, replies: Vec<(u16, String)>) -> (AskEnv, FakeServer) {
    let server = FakeServer::start(replies);
    let env = AskEnv::new(provider, &server.base_url);
    (env, server)
}

fn no_enrich_trace(err: &str) {
    assert!(
        !err.contains("llm enrich") && !err.contains("(jetons :"),
        "{err}"
    );
}

#[test]
fn req166_ac1_ac6_ac7_one_call_each_provider_same_stdout() {
    for p in ["openai", "anthropic"] {
        let reply = body(p, ASK_JSON, ok_stop(p), Some(u(900, 100, 40)));
        let (env, _s) = ask_http(p, vec![(200, reply.clone()); 4]);
        let plain = env.ask(&[], &[]);
        assert!(plain.status.success(), "{}", stderr(&plain));
        assert!(!stderr(&plain).contains("appels:") && !stderr(&plain).contains("jetons"));
        let o = env.ask(&["--verbose"], &[]);
        let err = stderr(&o);
        let t = traces(&err);
        assert_eq!(t.len(), 2, "{p} : {err}");
        assert!(t[0].starts_with("config: llm="), "{err}");
        assert_eq!(
            t[1],
            "appels: 1 appels, 0 nouveaux essais de format, 1 envois HTTP, jetons : entrée 900 dont cache 100, sortie 40"
        );
        assert_eq!(err.lines().count(), 2, "{err}");
        no_enrich_trace(&err);
        assert_eq!(o.stdout, plain.stdout, "{p}");
        let j_plain = env.ask(&["--json"], &[]);
        let j = env.ask(&["--json", "--verbose"], &[]);
        assert_eq!(j.stdout, j_plain.stdout, "{p} json");
    }
}

#[test]
fn req166_ac2_ac6_mock_format_retries() {
    let env = AskEnv::new("mock", "");
    fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let plain = env.ask(&[], &[("NEPHILA_MOCK_MALFORMED", "_ask:1")]);
    assert!(plain.status.success(), "{}", stderr(&plain));
    assert_eq!(stderr(&plain), "");
    let o = env.ask(&["--verbose"], &[("NEPHILA_MOCK_MALFORMED", "_ask:1")]);
    assert!(o.status.success(), "{}", stderr(&o));
    let t = traces(&stderr(&o));
    assert_eq!(t.len(), 2, "{}", stderr(&o));
    assert_eq!(
        t[1],
        "appels: 2 appels, 1 nouveaux essais de format, 2 envois HTTP, jetons : inconnus"
    );
    assert_eq!(o.stdout, plain.stdout);
    // Échec final : la ligne `appels:` précède la ligne `erreur:`, dernière de stderr.
    let o = env.ask(
        &["--verbose", "--format-retries", "2"],
        &[("NEPHILA_MOCK_MALFORMED", "_ask:3")],
    );
    assert!(!o.status.success());
    let err = stderr(&o);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 3, "{err}");
    assert_eq!(
        traces(&err)[1],
        "appels: 3 appels, 2 nouveaux essais de format, 3 envois HTTP, jetons : inconnus"
    );
    assert_eq!(
        lines[2],
        "erreur: réponse LLM non conforme au schéma : JSON illisible (NEPHILA_MOCK_MALFORMED) (3 essais sur 3)",
        "{err}"
    );
    no_enrich_trace(&err);
}

#[test]
fn req166_ac3_no_call_no_calls_line() {
    let env = AskEnv::new("mock", "");
    fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let o = env.cmd(&["query", "ask", "zzzabsent qqqabsent ?", "--verbose"], &[]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert_eq!(err.lines().count(), 2, "{err}");
    assert!(!err.contains("appels:"), "{err}");
    assert!(err.lines().nth(1).unwrap().starts_with("erreur:"), "{err}");
    let o = env.cmd(&["query", "ask", "zzzabsent qqqabsent ?"], &[]);
    assert!(!stderr(&o).contains("appels:"));
}

#[test]
fn req166_ac4_ac6_refused_key_and_http_retries() {
    let (env, _s) = ask_http("openai", vec![(401, "{}".into())]);
    let o = env.ask(&["--verbose"], &[]);
    assert!(!o.status.success());
    let err = stderr(&o);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 3, "{err}");
    assert!(lines[0].starts_with("trace: [") && lines[0].contains("config: llm="));
    assert!(lines[1].ends_with(
        "appels: 1 appels, 0 nouveaux essais de format, 1 envois HTTP, jetons : entrée 0 dont cache 0, sortie 0"
    ), "{err}");
    assert_eq!(
        lines[2],
        "erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env",
        "{err}"
    );
    no_enrich_trace(&err);

    let ok = body("openai", ASK_JSON, ok_stop("openai"), Some(u(900, 100, 40)));
    let (env, _s) = ask_http(
        "openai",
        vec![(503, "{}".into()), (503, "{}".into()), (200, ok)],
    );
    let o = env.ask(&["--verbose"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let t = traces(&stderr(&o));
    assert_eq!(t.len(), 2, "{}", stderr(&o));
    assert_eq!(
        t[1],
        "appels: 1 appels, 0 nouveaux essais de format, 3 envois HTTP, jetons : entrée 900 dont cache 100, sortie 40"
    );
}

#[test]
fn req166_ac6_not_found_reply_still_writes_calls_line() {
    let env = AskEnv::new("mock", "");
    fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    fs::write(env.mock.join("_ask.json"), r#"{"found":false}"#).unwrap();
    let o = env.ask(&["--verbose"], &[]);
    assert!(!o.status.success());
    let err = stderr(&o);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 3, "{err}");
    assert_eq!(
        traces(&err)[1],
        "appels: 1 appels, 0 nouveaux essais de format, 1 envois HTTP, jetons : inconnus"
    );
    assert_eq!(lines[2], "erreur: pas trouvé dans le graphe");
}
