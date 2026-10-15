//! Option `--verbose` de build et update (REQ-026) : configuration, parcours, phases.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

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
