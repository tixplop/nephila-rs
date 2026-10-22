//! Nouveaux essais des réponses LLM non conformes (REQ-038, REQ-026, REQ-024, REQ-007).

use assert_cmd::cargo::cargo_bin;
use nephila::store::Store;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

const MOCK_CONFIG: &str =
    "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nconcurrency = 1\n";

impl Env {
    fn new(n: usize, config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(mock.join("src")).unwrap();
        for i in 0..n {
            fs::write(
                repo.join(format!("src/F{i:02}.java")),
                format!("package p;\npublic class F{i:02} {{}}\n"),
            )
            .unwrap();
            fs::write(
                mock.join(format!("src/F{i:02}.java.json")),
                format!(
                    r#"{{"version":2,"summaries":[{{"node":"type:p.F{i:02}","summary":"résumé {i:02}"}}],"concepts":[]}}"#
                ),
            )
            .unwrap();
        }
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, config).unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config: config_path,
        }
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
        ] {
            c.env_remove(v);
        }
        c.arg("-C").arg(&self.repo).args(args);
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// Nombre de lignes du journal du mock qui concernent `file`.
    fn calls_for(&self, file: &str) -> usize {
        self.log_lines()
            .iter()
            .filter(|l| l.split('\t').next() == Some(file))
            .count()
    }

    fn to_enrich(&self) -> usize {
        Store::open_readonly(&self.repo)
            .unwrap()
            .files_to_enrich()
            .unwrap()
            .len()
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn lines_naming(o: &Output, name: &str) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.contains(name))
        .map(String::from)
        .collect()
}

fn departures(o: &Output) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.contains("llm enrich départ"))
        .map(|l| l.split("] ").skip(1).collect::<Vec<_>>().join("] "))
        .collect()
}

#[test]
fn req038_ac1_config_trace_shows_default_without_key() {
    let env = Env::new(1, MOCK_CONFIG);
    let o = env.run(&["build", "--verbose"], &[]);
    let line = stderr(&o)
        .lines()
        .find(|l| l.contains("config: llm="))
        .unwrap()
        .to_string();
    assert!(line.ends_with(" batch_files=1 format_retries=2"), "{line}");
}

#[test]
fn req026_ac10_config_trace_with_tls_insecure_and_no_llm() {
    let cfg = format!("{MOCK_CONFIG}tls_insecure = true\n");
    let env = Env::new(1, &cfg);
    let o = env.run(&["build", "--verbose"], &[]);
    let line = stderr(&o)
        .lines()
        .find(|l| l.contains("config: llm="))
        .unwrap()
        .to_string();
    assert!(line.ends_with(" format_retries=2 tls_insecure"), "{line}");
    let o = env.run(&["build", "--verbose", "--no-llm"], &[]);
    assert!(stderr(&o)
        .lines()
        .any(|l| l.ends_with("config: llm inactif (--no-llm)")));
}

#[test]
fn req038_ac3_success_at_third_try_leaves_no_trace() {
    let env = Env::new(4, MOCK_CONFIG);
    let o = env.run(&["build"], &[("NEPHILA_MOCK_MALFORMED", "src/F01.java:2")]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(env.calls_for("src/F01.java"), 3);
    for f in ["src/F00.java", "src/F02.java", "src/F03.java"] {
        assert_eq!(env.calls_for(f), 1, "{f}");
    }
    assert!(lines_naming(&o, "F01").is_empty(), "{}", stderr(&o));
    assert!(!stderr(&o).contains("essais sur"), "{}", stderr(&o));
    let search = env.run(&["query", "search", "F01", "--json"], &[]);
    assert!(
        String::from_utf8_lossy(&search.stdout).contains("résumé 01"),
        "{}",
        String::from_utf8_lossy(&search.stdout)
    );
    assert_eq!(env.to_enrich(), 0);

    // Même dépôt sans la variable : stdout et graphe identiques.
    let base = Env::new(4, MOCK_CONFIG);
    let ob = base.run(&["build"], &[]);
    assert!(ob.status.success());
    let norm = |e: &Env, o: &Output| {
        String::from_utf8_lossy(&o.stdout).replace(e.repo.to_str().unwrap(), "<repo>")
    };
    assert_eq!(norm(&env, &o), norm(&base, &ob));
    let graph = |e: &Env| e.run(&["export", "--output", "-"], &[]).stdout;
    assert_eq!(graph(&env), graph(&base));
}

#[test]
fn req038_ac4_default_two_retries_then_configured_three() {
    let env = Env::new(2, MOCK_CONFIG);
    let o = env.run(&["build"], &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")]);
    assert!(!o.status.success());
    assert_eq!(env.calls_for("src/F00.java"), 3);
    assert_eq!(env.to_enrich(), 1);
    let named = lines_naming(&o, "src/F00.java");
    assert_eq!(named.len(), 1, "{}", stderr(&o));
    assert!(named[0].ends_with("(3 essais sur 3)"), "{named:?}");

    let env = Env::new(2, &format!("{MOCK_CONFIG}format_retries = 3\n"));
    let o = env.run(&["build"], &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(env.calls_for("src/F00.java"), 4);
    assert_eq!(env.to_enrich(), 0);
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
}

#[test]
fn req038_ac5_durably_unreadable_fixture_then_update_after_fix() {
    let env = Env::new(3, MOCK_CONFIG);
    let fixture = env.mock.join("src/F01.java.json");
    let good = fs::read_to_string(&fixture).unwrap();
    fs::write(&fixture, "pas du json").unwrap();
    let o = env.run(&["build"], &[]);
    assert!(!o.status.success());
    assert_eq!(env.calls_for("src/F01.java"), 3);
    assert_eq!(env.calls_for("src/F00.java"), 1);
    assert_eq!(env.calls_for("src/F02.java"), 1);
    assert_eq!(env.to_enrich(), 1);
    let named = lines_naming(&o, "src/F01.java");
    assert_eq!(named.len(), 1, "{}", stderr(&o));
    assert!(named[0].ends_with("(3 essais sur 3)"), "{named:?}");
    assert!(!stderr(&o).contains("F00") && !stderr(&o).contains("F02"));

    fs::write(&fixture, good).unwrap();
    let before = env.log_lines().len();
    let o = env.run(&["update"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = env.log_lines();
    assert_eq!(lines.len(), before + 1, "{lines:?}");
    assert!(lines[before].starts_with("src/F01.java\t"), "{lines:?}");
    assert_eq!(env.to_enrich(), 0);
}

#[test]
fn req038_ac6_other_errors_are_not_retried() {
    let env = Env::new(3, MOCK_CONFIG);
    let o = env.run(
        &["build"],
        &[("NEPHILA_MOCK_TIMEOUT_FILES", "src/F00.java")],
    );
    assert!(!o.status.success());
    assert_eq!(env.calls_for("src/F00.java"), 1);

    let env = Env::new(8, MOCK_CONFIG);
    let o = env.run(&["build"], &[("NEPHILA_MOCK_FAIL_AFTER", "2")]);
    assert!(!o.status.success());
    assert_eq!(env.log_lines().len(), 5, "{:?}", env.log_lines());

    let env = Env::new(8, MOCK_CONFIG);
    let o = env.run(&["build"], &[("NEPHILA_MOCK_KEY_REVOKED_AFTER", "2")]);
    assert!(!o.status.success());
    assert_eq!(env.log_lines().len(), 3, "{:?}", env.log_lines());

    // Éléments écartés (AC-024-4) : un seul appel par fichier.
    let env = Env::new(8, MOCK_CONFIG);
    fs::write(
        env.mock.join("src/F00.java.json"),
        r#"{"version":2,"summaries":[{"node":"type:p.F00","summary":"x"},{"node":"type:p.Inexistant","summary":"y"}],"concepts":[]}"#,
    )
    .unwrap();
    let o = env.run(&["build"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("élément(s)"), "{}", stderr(&o));
    for i in 0..8 {
        assert_eq!(env.calls_for(&format!("src/F{i:02}.java")), 1, "{i}");
    }

    // Une réponse non conforme suivie d'une erreur qui n'est pas de format : pas d'autre essai.
    for var in ["NEPHILA_MOCK_FAIL_AFTER", "NEPHILA_MOCK_KEY_REVOKED_AFTER"] {
        let env = Env::new(1, &format!("{MOCK_CONFIG}format_retries = 5\n"));
        let o = env.run(
            &["build"],
            &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:9"), (var, "2")],
        );
        assert!(!o.status.success());
        assert_eq!(env.log_lines().len(), 3, "{var}");
    }
}

#[test]
fn req038_ac7_network_errors_are_not_retried() {
    let env = Env::new(8, MOCK_CONFIG);
    let o = env.run(
        &["build"],
        &[
            ("NEPHILA_MOCK_MALFORMED", "src/F00.java:1"),
            ("NEPHILA_MOCK_FAIL_AFTER", "1"),
        ],
    );
    assert!(!o.status.success());
    let lines = env.log_lines();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(env.calls_for("src/F00.java"), 2);
    assert!(
        stderr(&o).contains("panne réseau répétée"),
        "{}",
        stderr(&o)
    );
    assert!(!stderr(&o).contains("essais sur"), "{}", stderr(&o));
}

#[test]
fn req038_ac8_only_failing_chunk_is_resent() {
    let env = Env::new(0, &format!("{MOCK_CONFIG}max_chunk_chars = 60\n"));
    let text: String = (0..3)
        .map(|i| format!("class C{i} {{ int a{i}; int b{i}; int c{i}; }}\n"))
        .collect();
    fs::write(env.repo.join("src/Big.java"), &text).unwrap();
    let o = env.run(&["build", "--no-llm"], &[]);
    assert!(o.status.success());
    let o = env.run(
        &["update", "--verbose"],
        &[("NEPHILA_MOCK_MALFORMED", "src/Big.java:1")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(env.to_enrich(), 0);
    let chunks: Vec<String> = env
        .log_lines()
        .iter()
        .map(|l| l.split('\t').nth(1).unwrap().to_string())
        .collect();
    assert_eq!(chunks, ["1", "1", "2", "3"], "{}", stderr(&o));
    let deps = departures(&o);
    assert_eq!(deps.len(), 4, "{deps:?}");
    assert!(
        deps[0].contains("[1/3]") && deps[0].ends_with(" car."),
        "{deps:?}"
    );
    assert_eq!(deps[1], format!("{} (essai 2/3)", deps[0]));
    assert!(
        deps[2].contains("[2/3]") && deps[2].ends_with(" car."),
        "{deps:?}"
    );
    assert!(
        deps[3].contains("[3/3]") && deps[3].ends_with(" car."),
        "{deps:?}"
    );
}

#[test]
fn req038_ac8_batch_resends_whole_lot() {
    let env = Env::new(4, &format!("{MOCK_CONFIG}batch_files = 2\n"));
    let o = env.run(
        &["build", "--verbose"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F01.java:1")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = env.log_lines();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0], lines[1]);
    assert_ne!(lines[1], lines[2]);
    assert_eq!(lines.iter().filter(|l| **l == lines[2]).count(), 1);
    assert_eq!(env.to_enrich(), 0);
    let deps = departures(&o);
    assert_eq!(deps.len(), 3, "{deps:?}");
    assert!(
        deps[0].starts_with("llm enrich départ lot src/F00.java +1 [1/1] ")
            && deps[0].ends_with(" car."),
        "{deps:?}"
    );
    assert_eq!(deps[1], format!("{} (essai 2/3)", deps[0]));
    assert!(deps[2].contains("lot src/F02.java +1") && !deps[2].contains("essai"));
}

#[test]
fn req026_ac11_trace_marks_retries_only_on_departure() {
    let env = Env::new(3, MOCK_CONFIG);
    let o = env.run(
        &["build", "--verbose"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F01.java:2")],
    );
    assert!(o.status.success());
    let own: Vec<String> = lines_naming(&o, "src/F01.java")
        .into_iter()
        .filter(|l| l.contains("llm enrich"))
        .map(|l| l.split("] ").skip(1).collect::<Vec<_>>().join("] "))
        .collect();
    assert_eq!(own.len(), 6, "{own:?}");
    let base = own[0].clone();
    assert!(
        base.starts_with("llm enrich départ src/F01.java [1/1] ") && base.ends_with(" car."),
        "{base}"
    );
    let non_conforming = " : réponse LLM non conforme";
    assert!(own[1].starts_with("llm enrich fin ") && own[1].contains(" erreur "));
    assert!(own[1].contains(non_conforming), "{}", own[1]);
    assert_eq!(own[2], format!("{base} (essai 2/3)"));
    assert!(own[3].contains(" erreur "), "{}", own[3]);
    assert_eq!(own[4], format!("{base} (essai 3/3)"));
    assert!(
        own[5].starts_with("llm enrich fin ") && own[5].contains(" ok ("),
        "{}",
        own[5]
    );
    // Aucune autre ligne ne porte la marque.
    assert_eq!(stderr(&o).matches("(essai").count(), 2);
    let err = stderr(&o);
    for forbidden in ["package p", "public class", "résumé 0", "\"summaries\""] {
        assert!(
            !err.contains(forbidden),
            "stderr contient {forbidden:?} : {err}"
        );
    }
    for f in ["src/F00.java", "src/F02.java"] {
        let ls = lines_naming(&o, f);
        assert_eq!(ls.len(), 2, "{ls:?}");
        assert!(ls.iter().all(|l| !l.contains("(essai")));
    }
    let ends = stderr(&o)
        .lines()
        .filter(|l| l.contains("llm enrich fin"))
        .count();
    assert_eq!(ends, env.log_lines().len());

    let env = Env::new(1, &format!("{MOCK_CONFIG}format_retries = 0\n"));
    let o = env.run(
        &["build", "--verbose"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:2")],
    );
    assert!(!o.status.success());
    assert!(!stderr(&o).contains("(essai"));
}

#[test]
fn req038_ac17_final_warning_carries_attempt_count() {
    let env = Env::new(2, MOCK_CONFIG);
    let o = env.run(&["build"], &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")]);
    let w = lines_naming(&o, "src/F00.java");
    assert_eq!(w.len(), 1, "{}", stderr(&o));
    assert!(
        w[0].starts_with("avertissement: src/F00.java : réponse LLM non conforme"),
        "{}",
        w[0]
    );
    assert!(w[0].ends_with(" (3 essais sur 3)"), "{}", w[0]);

    // format_retries = 1 : (2 essais sur 2) ; 0 : pas de suffixe.
    let env = Env::new(1, &format!("{MOCK_CONFIG}format_retries = 1\n"));
    let o = env.run(&["build"], &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")]);
    let w = lines_naming(&o, "src/F00.java");
    assert_eq!(w.len(), 1, "{}", stderr(&o));
    assert!(w[0].ends_with(" (2 essais sur 2)"), "{}", w[0]);
    let env = Env::new(1, &format!("{MOCK_CONFIG}format_retries = 0\n"));
    let o = env.run(&["build"], &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:1")]);
    assert!(!stderr(&o).contains("essais sur"), "{}", stderr(&o));
    assert_eq!(lines_naming(&o, "src/F00.java").len(), 1);

    let env = Env::new(4, &format!("{MOCK_CONFIG}batch_files = 2\n"));
    let o = env.run(&["build"], &[("NEPHILA_MOCK_MALFORMED", "src/F01.java:3")]);
    for f in ["src/F00.java", "src/F01.java"] {
        let w = lines_naming(&o, f);
        assert_eq!(w.len(), 1, "{}", stderr(&o));
        assert!(
            w[0].starts_with("avertissement: ") && w[0].ends_with(" (3 essais sur 3)"),
            "{}",
            w[0]
        );
    }
}

// ---- Fournisseur openai simulé ----

fn openai_reply(text: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string()
}

const GOOD: &str =
    r#"{"version":2,"summaries":[{"node":"type:p.F00","summary":"résumé"}],"concepts":[]}"#;

fn server_env(server: &FakeServer, extra: &str) -> Env {
    Env::new(
        1,
        &format!(
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_RETRIES_TEST_KEY\"\nconcurrency = 1\n{extra}",
            server.base_url
        ),
    )
}

fn posts(server: &FakeServer) -> Vec<fake_server::Recorded> {
    server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect()
}

#[test]
fn req038_ac9_openai_empty_reply_is_retried_with_identical_body() {
    let server = FakeServer::start(vec![
        (200, r#"{"choices":[]}"#.into()),
        (200, openai_reply(GOOD)),
    ]);
    let env = server_env(&server, "");
    let o = env.run(&["build"], &[("NEPHILA_RETRIES_TEST_KEY", "sk")]);
    assert!(o.status.success(), "{}", stderr(&o));
    let reqs = posts(&server);
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].body, reqs[1].body);
    assert_eq!(env.to_enrich(), 0);
    let q = env.run(&["query", "search", "F00", "--json"], &[]);
    assert!(q.status.success(), "{}", stderr(&q));
    assert!(String::from_utf8_lossy(&q.stdout).contains("résumé"));
}

#[test]
fn req038_ac9_openai_text_without_json_three_requests_then_zero_retries_one() {
    let bad = openai_reply("désolé, pas de JSON");
    let server = FakeServer::start(vec![
        (200, bad.clone()),
        (200, bad.clone()),
        (200, bad.clone()),
    ]);
    let env = server_env(&server, "");
    let o = env.run(&["build"], &[("NEPHILA_RETRIES_TEST_KEY", "sk")]);
    assert!(!o.status.success());
    assert_eq!(posts(&server).len(), 3);
    assert_eq!(env.to_enrich(), 1);

    let server = FakeServer::start(vec![(200, bad)]);
    let env = server_env(&server, "format_retries = 0\n");
    let o = env.run(&["build"], &[("NEPHILA_RETRIES_TEST_KEY", "sk")]);
    assert!(!o.status.success());
    assert_eq!(posts(&server).len(), 1);
}

#[test]
fn req038_ac6_http_error_is_not_retried() {
    // Un 500 est retenté par les nouveaux essais HTTP (REQ-052), pas par ceux du format (REQ-038).
    let server = FakeServer::start(vec![(500, "{}".into())]);
    let env = server_env(&server, "");
    let o = env.run(
        &["build"],
        &[
            ("NEPHILA_RETRIES_TEST_KEY", "sk"),
            ("NEPHILA_RETRY_BASE_MS", "1"),
        ],
    );
    assert!(!o.status.success());
    assert_eq!(posts(&server).len(), 5);
}

#[test]
fn req038_ac9_anthropic_reply_without_text_block_is_retried() {
    let good = serde_json::json!({"content":[{"type":"text","text":GOOD}]}).to_string();
    let server = FakeServer::start(vec![(200, r#"{"content":[]}"#.into()), (200, good)]);
    let env = Env::new(
        1,
        &format!(
            "provider = \"anthropic\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_RETRIES_TEST_KEY\"\nconcurrency = 1\n",
            server.base_url
        ),
    );
    let o = env.run(&["build"], &[("NEPHILA_RETRIES_TEST_KEY", "sk")]);
    assert!(o.status.success(), "{}", stderr(&o));
    let reqs = posts(&server);
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].body, reqs[1].body);
    assert_eq!(env.to_enrich(), 0);
    let q = env.run(&["query", "search", "F00", "--json"], &[]);
    assert!(q.status.success(), "{}", stderr(&q));
    assert!(String::from_utf8_lossy(&q.stdout).contains("résumé"));
}

#[test]
fn req038_ac6_lot_omitting_a_file_is_sent_once() {
    let entry = |i: usize| {
        format!(
            r#"{{"file":"src/F{i:02}.java","summaries":[{{"node":"type:p.F{i:02}","summary":"résumé {i:02}"}}],"concepts":[]}}"#
        )
    };
    let reply = format!(r#"{{"version":3,"files":[{},{}]}}"#, entry(0), entry(2));
    let server = FakeServer::start(vec![(200, openai_reply(&reply))]);
    let env = Env::new(
        3,
        &format!(
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_RETRIES_TEST_KEY\"\nconcurrency = 1\nbatch_files = 3\n",
            server.base_url
        ),
    );
    let o = env.run(&["build"], &[("NEPHILA_RETRIES_TEST_KEY", "sk")]);
    assert!(!o.status.success());
    assert_eq!(posts(&server).len(), 1, "{}", stderr(&o));
    assert_eq!(env.to_enrich(), 1);
}

// ---- Option --format-retries (REQ-038, T-079) ----

fn help_of(args: &[&str]) -> String {
    let o = Command::new(cargo_bin("nephila"))
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout).into_owned()
}

const RETRIES_HELP: &str = "--format-retries <N> Nombre de nouveaux essais d'un appel LLM dont la réponse est non conforme (remplace `format_retries` de la configuration ; défaut 2 ; 0 = aucun nouvel essai)";

#[test]
fn req038_ac2_ac14_help_mentions_option() {
    for args in [
        &["build", "--help"][..],
        &["update", "--help"][..],
        &["query", "ask", "--help"][..],
    ] {
        let h = help_of(args);
        let norm = h.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(norm.contains(RETRIES_HELP), "{args:?}\n{h}");
        // Le `[default: .]` de `-C` est d'une autre option : seule l'entrée de la nôtre
        // compte. Elle s'arrête à la prochaine occurrence de ` -` (DEC-048), car l'aide
        // d'une option courte suivante (`-h, --help`) commence par ` -h`.
        let from = norm.find("--format-retries").unwrap();
        let rest = &norm[from..];
        let entry = &rest[..rest[2..].find(" -").map_or(rest.len(), |i| i + 2)];
        assert!(!entry.contains("[default:"), "{args:?} {entry}");
        if args[0] != "query" {
            let p = |s: &str| norm.find(s).unwrap();
            assert!(p("--batch-files") < p("--format-retries"));
            assert!(p("--format-retries") < p("--exclude <MOTIF>"));
        }
    }
    for args in [
        &["query", "search", "--help"][..],
        &["query", "neighbors", "--help"][..],
        &["query", "path", "--help"][..],
        &["status", "--help"][..],
    ] {
        assert!(!help_of(args).contains("--format-retries"));
    }
}

#[test]
fn req038_ac2_ac15_usage_errors_touch_nothing() {
    let env = Env::new(1, MOCK_CONFIG);
    let mut cases: Vec<(Vec<&str>, &str)> = Vec::new();
    for v in ["-1", "abc"] {
        cases.push((vec!["build", "--format-retries", v], v));
        cases.push((vec!["update", "--format-retries", v], v));
        cases.push((vec!["query", "ask", "x", "--format-retries", v], v));
    }
    cases.push((
        vec!["query", "ask", "x", "--format-retries", "4294967296"],
        "4294967296",
    ));
    cases.push((vec!["build", "--format-retries", "1.5"], "1.5"));
    for (args, v) in cases {
        let o = env.run(&args, &[]);
        assert!(!o.status.success(), "{args:?}");
        let e = stderr(&o);
        assert!(e.contains("--format-retries"), "{e}");
        assert!(
            e.contains(&format!("`{v}` n'est pas un entier positif ou nul")),
            "{e}"
        );
        assert!(o.stdout.is_empty());
        assert!(!o.stdout.contains(&0x1b) && !o.stderr.contains(&0x1b));
        assert!(!env.log.exists(), "{args:?}");
        assert!(!env.repo.join(".nephila").exists());
    }
    for args in [
        &["status", "--format-retries", "1"][..],
        &["query", "search", "x", "--format-retries", "1"][..],
    ] {
        let o = env.run(args, &[]);
        assert!(!o.status.success(), "{args:?}");
        assert!(!env.repo.join(".nephila").exists());
    }
}

fn nephila_listing(env: &Env) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = fs::read_dir(env.repo.join(".nephila"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().unwrap().len(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn req038_ac2_ac15_usage_errors_keep_existing_index() {
    let env = Env::new(2, MOCK_CONFIG);
    let o = env.run(&["build"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let db = fs::read(env.repo.join(".nephila/graph.db")).unwrap();
    let listing = nephila_listing(&env);
    let _ = fs::remove_file(&env.log);
    for args in [
        &["update", "--format-retries", "-1"][..],
        &["update", "--format-retries", "abc"][..],
        &["query", "ask", "x", "--format-retries", "-1"][..],
        &["query", "ask", "x", "--format-retries", "abc"][..],
    ] {
        let o = env.run(args, &[]);
        assert!(!o.status.success(), "{args:?}");
        assert!(!env.log.exists(), "{args:?}");
        assert!(
            fs::read(env.repo.join(".nephila/graph.db")).unwrap() == db,
            "{args:?}"
        );
        assert_eq!(nephila_listing(&env), listing, "{args:?}");
    }
}

#[test]
fn req038_ac2_ac15_no_llm_accepts_option() {
    let env = Env::new(1, MOCK_CONFIG);
    for v in ["3", "0", "4294967295"] {
        let o = env.run(&["build", "--no-llm", "--format-retries", v], &[]);
        assert!(o.status.success(), "{}", stderr(&o));
        assert!(env.log_lines().is_empty());
    }
    // Sans configuration : acceptée, sans effet ni avertissement.
    let _ = fs::remove_file(&env.config);
    let o = env.run(&["build", "--format-retries", "3"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!stderr(&o).contains("avertissement"), "{}", stderr(&o));
}

#[test]
fn req038_ac4_option_overrides_key_both_ways() {
    let env = Env::new(2, &format!("{MOCK_CONFIG}format_retries = 3\n"));
    let o = env.run(
        &["build", "--format-retries", "0"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")],
    );
    assert!(!o.status.success());
    assert_eq!(env.calls_for("src/F00.java"), 1);
    assert_eq!(env.to_enrich(), 1);

    let env = Env::new(2, &format!("{MOCK_CONFIG}format_retries = 0\n"));
    let o = env.run(
        &["build", "--format-retries", "3"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(env.calls_for("src/F00.java"), 4);
    assert_eq!(env.to_enrich(), 0);

    // update : l'option s'applique aussi.
    let env = Env::new(2, MOCK_CONFIG);
    let o = env.run(
        &["update", "--format-retries", "4"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:4")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(env.calls_for("src/F00.java"), 5);
}

#[test]
fn req038_ac12_ac16_trace_and_invalid_key_with_option() {
    let env = Env::new(1, MOCK_CONFIG);
    let o = env.run(&["build", "--verbose", "--format-retries", "5"], &[]);
    let line = stderr(&o)
        .lines()
        .find(|l| l.contains("config: llm="))
        .unwrap()
        .to_string();
    assert!(line.ends_with(" format_retries=5"), "{line}");

    for bad in ["-1", "1.5", "\"2\""] {
        let env = Env::new(1, &format!("{MOCK_CONFIG}format_retries = {bad}\n"));
        let at = env.config.display();
        for args in [
            &["build"][..],
            &["build", "--format-retries", "3"][..],
            &["query", "ask", "x", "--format-retries", "3"][..],
        ] {
            let o = env.run(args, &[]);
            assert!(!o.status.success(), "{args:?} {bad}");
            let e = stderr(&o);
            assert_eq!(
                e.trim_end(),
                format!("erreur: configuration invalide ({at}) : clé « format_retries » doit être un entier positif ou nul (reçu : {bad})"),
                "{args:?}"
            );
        }
    }
}

#[test]
fn req026_ac11_option_four_marks_single_departure_of_second_try() {
    let env = Env::new(2, MOCK_CONFIG);
    let o = env.run(
        &["build", "--verbose", "--format-retries", "4"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F01.java:1")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let marked: Vec<_> = departures(&o)
        .into_iter()
        .filter(|l| l.contains("(essai "))
        .collect();
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert!(marked[0].ends_with("(essai 2/5)"), "{marked:?}");
}

#[test]
fn req038_ac17_option_one_gives_two_tries_suffix() {
    let env = Env::new(1, MOCK_CONFIG);
    let o = env.run(
        &["build", "--format-retries", "1"],
        &[("NEPHILA_MOCK_MALFORMED", "src/F00.java:3")],
    );
    let w = lines_naming(&o, "src/F00.java");
    assert_eq!(w.len(), 1, "{}", stderr(&o));
    assert!(w[0].ends_with(" (2 essais sur 2)"), "{}", w[0]);
}

#[test]
fn req038_ac11_openai_ask_retried_after_nonconforming_reply() {
    let ask_ok = r#"{"found":true,"answer":"oui","nodes":["type:p.F00"]}"#;
    let server = FakeServer::start(vec![
        (200, openai_reply(r#"{"found": "oui"}"#)),
        (200, openai_reply(ask_ok)),
    ]);
    let env = Env::new(1, MOCK_CONFIG);
    assert!(env.run(&["build"], &[]).status.success());
    fs::write(
        &env.config,
        format!(
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_RETRIES_TEST_KEY\"\n",
            server.base_url
        ),
    )
    .unwrap();
    let o = env.run(
        &["query", "ask", "F00 ?"],
        &[("NEPHILA_RETRIES_TEST_KEY", "sk")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("oui"));
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
    let chats: Vec<_> = posts(&server)
        .into_iter()
        .filter(|r| r.path.ends_with("/v1/chat/completions"))
        .collect();
    assert_eq!(chats.len(), 2);
}
