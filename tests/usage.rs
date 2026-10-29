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
