//! Option `--verbose` de build et update (REQ-026) : configuration, parcours, phases.

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

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new(config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let repo = tmp.path().join("repo");
        copy_dir(&fx.join("maven2"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("mock"), &mock);
        let cfg = tmp.path().join("config.toml");
        fs::write(&cfg, config).unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            config: cfg,
            mock,
        }
    }

    fn mock() -> Env {
        Env::new("provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 4\n")
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        self.cmd_with(args, &[])
    }

    fn cmd_with(&self, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
        let mut c = Command::cargo_bin("rkg").unwrap();
        for k in [
            "RKG_MOCK_FAIL_AFTER",
            "RKG_MOCK_INVALID_KEY",
            "RKG_MOCK_DELAY_MS",
            "RKG_MOCK_TIMEOUT_FILES",
            "RKG_MOCK_KEY_REVOKED_AFTER",
        ] {
            c.env_remove(k);
        }
        c.envs(envs.iter().copied());
        c.env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log)
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn trace_lines(o: &std::process::Output) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.starts_with("trace:"))
        .map(String::from)
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

fn well_formed(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("trace: [") else {
        return false;
    };
    let Some((time, _)) = rest.split_once("s] ") else {
        return false;
    };
    let Some((s, ms)) = time.split_once('.') else {
        return false;
    };
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_digit())
        && ms.len() == 3
        && ms.chars().all(|c| c.is_ascii_digit())
}

#[test]
fn req026_ac1_help_lists_verbose_and_status_rejects_it() {
    let env = Env::mock();
    for sub in ["build", "update"] {
        let o = env.cmd(&[sub, "--help"]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("--verbose"));
    }
    let o = env.cmd(&["status", "--verbose"]);
    assert!(!o.status.success());
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn req026_ac2_stdout_identical_and_trace_format() {
    let plain = Env::mock();
    let o1 = plain.cmd(&["build"]);
    assert!(
        !stderr(&o1).lines().any(|l| l.starts_with("trace:")),
        "{}",
        stderr(&o1)
    );
    let verbose = Env::mock();
    let o2 = verbose.cmd(&["build", "--verbose"]);
    assert_eq!(o1.stdout, o2.stdout);
    assert_eq!(o1.status.code(), o2.status.code());
    let lines = trace_lines(&o2);
    assert!(!lines.is_empty());
    for l in &lines {
        assert!(well_formed(l), "{l}");
    }
}

#[test]
fn req026_ac3_config_parcours_and_phases() {
    let env = Env::mock();
    let o = env.cmd(&["build", "-v"]);
    let lines = trace_lines(&o);
    assert!(
        lines.iter().any(|l| l.contains("config")
            && l.contains("mock")
            && l.contains("concurrency=4")
            && l.contains("batch_files=1")
            && l.contains("timeout_secs=")),
        "{lines:?}"
    );
    assert!(lines
        .iter()
        .any(|l| l.contains("verify_key") && l.contains(" ok ")));
    assert!(has(&lines, "parcours"));
    assert!(has(&lines, "structure: début"));
    assert!(has(&lines, "structure: fin"));
    assert!(has(&lines, "résolution:"));
    let pos = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(pos("config") < pos("verify_key"));
    assert!(pos("verify_key") < pos("parcours"));
    assert!(pos("parcours") < pos("structure: début"));
    assert!(pos("structure: fin") < pos("résolution:"));
    for l in stderr(&o).lines() {
        assert!(
            l.starts_with("trace:") || l.starts_with("avertissement:") || l.starts_with("erreur:"),
            "{l}"
        );
    }
}

#[test]
fn req026_ac7_no_llm_and_no_config() {
    let env = Env::mock();
    let o = env.cmd(&["build", "--no-llm", "--verbose"]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "llm inactif (--no-llm)"));
    assert!(!has(&lines, "llm enrich"));
    assert!(!has(&lines, "verify_key"));

    let env = Env::mock();
    fs::remove_file(&env.config).unwrap();
    let o = env.cmd(&["build", "-v"]);
    assert!(has(&trace_lines(&o), "llm inactif (aucune configuration)"));
}

#[test]
fn req026_ac7_update_without_change_and_without_graph() {
    let env = Env::mock();
    // Sans graphe : build complet tracé.
    let o = env.cmd(&["update", "-v"]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "structure: début"), "{lines:?}");
    assert!(has(&lines, "parcours"));
    let env2 = Env::mock();
    assert!(env2.cmd(&["build"]).status.success());
    let o = env2.cmd(&["update", "--verbose"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "graphe à jour");
    let lines = trace_lines(&o);
    assert!(has(&lines, "config"));
    assert!(has(
        &lines,
        "parcours: 0 ajoutés, 0 modifiés, 0 supprimés, 0 en attente"
    ));
}

fn enrich_ends(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.contains("llm enrich fin"))
        .collect()
}

#[test]
fn req026_ac3_enrich_calls_paired_and_match_mock_log() {
    let env = Env::mock();
    let o = env.cmd(&["build", "-v"]);
    let lines = trace_lines(&o);
    let ends = enrich_ends(&lines);
    let log = fs::read_to_string(&env.log).unwrap();
    assert!(!ends.is_empty());
    assert_eq!(ends.len(), log.lines().count(), "{lines:?}");
    assert!(has(&lines, "enrichissement: début"));
    assert!(has(&lines, ", 4 fils"), "{lines:?}");
    assert!(has(&lines, "enrichissement: fin"));
    for (i, l) in lines.iter().enumerate() {
        let Some(rest) = l.split("llm enrich fin ").nth(1) else {
            continue;
        };
        let key: Vec<&str> = rest.split_whitespace().take(2).collect();
        let start = format!("llm enrich départ {} {}", key[0], key[1]);
        assert!(lines[..i].iter().any(|p| p.contains(&start)), "{l}");
        assert!(l.contains(" ms"), "{l}");
        assert!(l.contains("ok ("), "{l}");
    }
    for l in stderr(&o).lines() {
        assert!(
            l.starts_with("trace:") || l.starts_with("avertissement:") || l.starts_with("erreur:"),
            "{l}"
        );
    }
}

#[test]
fn req026_ac4_timeout_file() {
    let env = Env::mock();
    let o = env.cmd_with(
        &["build", "-v"],
        &[("RKG_MOCK_TIMEOUT_FILES", "core/pom.xml")],
    );
    let lines = trace_lines(&o);
    let timeouts: Vec<_> = enrich_ends(&lines)
        .into_iter()
        .filter(|l| l.contains("délai dépassé"))
        .collect();
    assert_eq!(timeouts.len(), 1, "{lines:?}");
    assert!(timeouts[0].contains("core/pom.xml"));
}

#[test]
fn req026_ac4_network_policy_trace() {
    let env = Env::new("provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 1\n");
    let o = env.cmd_with(&["build", "-v"], &[("RKG_MOCK_FAIL_AFTER", "2")]);
    let lines = trace_lines(&o);
    let pos = |n: &str| {
        lines
            .iter()
            .position(|l| l.contains(n))
            .unwrap_or_else(|| panic!("{n} absent : {lines:?}"))
    };
    let (a, b, c, d) = (
        pos("politique: erreurs réseau consécutives 1/3"),
        pos("politique: erreurs réseau consécutives 2/3"),
        pos("politique: erreurs réseau consécutives 3/3"),
        pos("politique: arrêt de l'envoi"),
    );
    assert!(a < b && b < c && c < d);
    assert!(lines[d].contains("fichiers non envoyés"));
    assert!(has(&lines, "réseau"));
    assert!(has(&lines, "enrichissement: fin"));
}

#[test]
fn req026_ac4_key_revoked() {
    let env = Env::new("provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 1\n");
    let o = env.cmd_with(&["build", "-v"], &[("RKG_MOCK_KEY_REVOKED_AFTER", "2")]);
    let lines = trace_lines(&o);
    let r = lines
        .iter()
        .position(|l| l.contains("llm enrich fin") && l.contains("clé refusée"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    let s = lines
        .iter()
        .position(|l| l.contains("politique: arrêt de l'envoi"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert!(r < s);
}

fn openai_env(server: &FakeServer, timeout: u32, source: &str) -> Env {
    let env = Env::new(&format!(
        "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"RKG_T049_KEY\"\n\
         concurrency = 1\ntimeout_secs = {timeout}\n",
        server.base_url
    ));
    fs::remove_dir_all(&env.repo).unwrap();
    fs::create_dir_all(&env.repo).unwrap();
    fs::write(env.repo.join("A.java"), source).unwrap();
    env
}

fn openai_reply(text: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string()
}

#[test]
fn req026_ac5_openai_timeout_duration() {
    let server =
        FakeServer::start_with_delays(vec![(200, "{}".into(), 0), (200, openai_reply("{}"), 2000)]);
    let env = openai_env(&server, 1, "class A {}\n");
    let o = env.cmd_with(&["build", "-v"], &[("RKG_T049_KEY", "sk-x")]);
    let lines = trace_lines(&o);
    let ends = enrich_ends(&lines);
    assert_eq!(ends.len(), 1, "{lines:?}");
    assert!(ends[0].contains("délai dépassé"), "{}", ends[0]);
    let ms: u64 = ends[0]
        .split(" ms")
        .next()
        .unwrap()
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!((900..=2000).contains(&ms), "{ms}");
}

#[test]
fn req026_ac5_openai_key_refused_at_verification() {
    let server = FakeServer::start(vec![(401, "{}".into())]);
    let env = openai_env(&server, 5, "class A {}\n");
    let o = env.cmd_with(&["build", "-v"], &[("RKG_T049_KEY", "sk-x")]);
    assert!(!o.status.success());
    let lines = trace_lines(&o);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("verify_key") && l.contains("clé refusée")),
        "{lines:?}"
    );
    assert!(stderr(&o).contains("erreur:"));
}

fn assert_no_secret_in_failed_call(reply_text: &str) {
    let server = FakeServer::start(vec![(200, "{}".into()), (200, openai_reply(reply_text))]);
    let env = openai_env(&server, 5, "class A {}\n");
    let o = env.cmd_with(&["build", "-v"], &[("RKG_T049_KEY", "sk-test-SECRET")]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "llm enrich fin"), "{lines:?}");
    for l in &lines {
        assert!(!l.contains("SECRET"), "{l}");
    }
}

#[test]
fn req026_ac6_no_secret_from_nonconforming_value() {
    assert_no_secret_in_failed_call(
        r#"{"version":1,"summaries":"SECRET_REPONSE_42 abc","concepts":[]}"#,
    );
}

#[test]
fn req026_ac6_no_secret_from_unknown_field() {
    assert_no_secret_in_failed_call(
        r#"{"version":1,"summaries":[],"concepts":[],"SECRET_CHAMP_42":1}"#,
    );
}

#[test]
fn req026_ac6_no_secret_in_traces() {
    let long = format!("SECRET_REPONSE_42 {}", "x".repeat(5000));
    let server = FakeServer::start(vec![(200, "{}".into()), (200, openai_reply(&long))]);
    let env = openai_env(
        &server,
        5,
        "class A { String s = \"SECRET_CONTENU_42\"; }\n",
    );
    let o = env.cmd_with(&["build", "-v"], &[("RKG_T049_KEY", "sk-test-SECRET")]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "llm enrich fin"), "{lines:?}");
    for l in &lines {
        for m in ["SECRET_CONTENU_42", "SECRET_REPONSE_42", "sk-test-SECRET"] {
            assert!(!l.contains(m), "{l}");
        }
        assert!(l.chars().count() <= 300, "{l}");
    }
}
