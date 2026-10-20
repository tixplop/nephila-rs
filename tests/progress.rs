//! Option `--show-progress` de build et update (REQ-043) : phase d'extraction.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_with(n: usize) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    for i in 0..n {
        write_java(&repo, i, "");
    }
    (tmp, repo)
}

fn write_java(repo: &Path, i: usize, extra: &str) {
    fs::write(
        repo.join(format!("C{i}.java")),
        format!("package p;\npublic class C{i} {{ {extra} }}\n"),
    )
    .unwrap();
}

fn run(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("nephila")
        .unwrap()
        .env("NEPHILA_CONFIG", repo.join("absent-config.toml"))
        .env_remove("NO_COLOR")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap()
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn progress_lines(o: &std::process::Output) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.starts_with("progression:"))
        .map(str::to_string)
        .collect()
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    out.push((rel, fs::read(&p).unwrap()));
                }
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn req043_ac1_option_unknown_elsewhere() {
    let (_t, repo) = repo_with(1);
    for args in [
        vec!["status", "--show-progress"],
        vec!["query", "search", "x", "--show-progress"],
    ] {
        let o = run(&repo, &args);
        assert!(!o.status.success(), "{args:?}");
    }
}

#[test]
fn req043_ac2_ac14_conflict_is_usage_error() {
    let (_t, repo) = repo_with(2);
    for args in [
        vec!["build", "--show-progress", "--verbose"],
        vec!["build", "-v", "--show-progress"],
        vec!["update", "--show-progress", "--verbose"],
        vec!["update", "--show-progress", "-v"],
    ] {
        let o = run(&repo, &args);
        let err = stderr(&o);
        assert_eq!(o.status.code(), Some(2), "{args:?} {err}");
        assert!(o.stdout.is_empty(), "{args:?}");
        assert!(err.contains("'--show-progress'"), "{err}");
        assert!(err.contains("'--verbose'"), "{err}");
        for l in err.lines() {
            assert!(
                !l.starts_with("progression:")
                    && !l.starts_with("trace:")
                    && !l.starts_with("erreur:"),
                "{l}"
            );
        }
        assert!(!repo.join(".nephila").exists(), "{args:?}");
    }
    // Avec un .nephila.toml invalide : l'erreur d'usage passe avant.
    fs::write(repo.join(".nephila.toml"), "exclude = [").unwrap();
    let o = run(&repo, &["build", "--show-progress", "-v"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!repo.join(".nephila").exists());
}

#[test]
fn req043_ac2_indexed_repo_unchanged() {
    let (_t, repo) = repo_with(2);
    assert!(run(&repo, &["build", "--no-llm"]).status.success());
    let before = snapshot(&repo.join(".nephila"));
    assert!(!before.is_empty());
    fs::write(repo.join(".nephila.toml"), "exclude = [").unwrap();
    let o = run(&repo, &["update", "--show-progress", "--verbose"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(snapshot(&repo.join(".nephila")), before);
}

#[test]
fn req043_ac2_no_mock_log_on_conflict() {
    let (tmp, repo) = repo_with(2);
    let cfg = tmp.path().join("c.toml");
    fs::write(&cfg, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let log = tmp.path().join("mock.log");
    let o = Command::cargo_bin("nephila")
        .unwrap()
        .env("NEPHILA_CONFIG", &cfg)
        .env("NEPHILA_MOCK_LOG", &log)
        .arg("-C")
        .arg(&repo)
        .args(["build", "--show-progress", "-v"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(!log.exists());
}

#[test]
fn req043_ac3_ac12_twenty_files_no_llm() {
    let (_t, repo) = repo_with(20);
    let copy = {
        let (t, r) = repo_with(20);
        (t, r)
    };
    let o = run(&repo, &["build", "--no-llm", "--show-progress"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let expected: Vec<String> = (0..=10)
        .map(|i| {
            format!(
                "progression: extraction {} % ({}/20 fichiers)",
                i * 10,
                i * 2
            )
        })
        .collect();
    let lines: Vec<String> = stderr(&o).lines().map(str::to_string).collect();
    assert_eq!(lines, expected);
    assert!(!stderr(&o).contains("trace:"));
    let plain = run(&copy.1, &["build", "--no-llm"]);
    assert_eq!(o.stdout, plain.stdout);
    assert_eq!(o.status.code(), plain.status.code());
}

#[test]
fn req043_ac12_single_file() {
    let (_t, repo) = repo_with(1);
    let o = run(&repo, &["build", "--no-llm", "--show-progress"]);
    assert_eq!(
        stderr(&o).lines().collect::<Vec<_>>(),
        [
            "progression: extraction 0 % (0/1 fichiers)",
            "progression: extraction 100 % (1/1 fichiers)"
        ]
    );
}

#[test]
fn req043_ac7_update_no_llm() {
    let (_t, repo) = repo_with(10);
    assert!(run(&repo, &["build", "--no-llm"]).status.success());
    write_java(&repo, 3, "int x;");
    let o = run(&repo, &["update", "--no-llm", "--show-progress"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        stderr(&o).lines().collect::<Vec<_>>(),
        [
            "progression: extraction 0 % (0/1 fichiers)",
            "progression: extraction 100 % (1/1 fichiers)"
        ]
    );
    // Fichier supprimé seulement.
    fs::remove_file(repo.join("C4.java")).unwrap();
    let o = run(&repo, &["update", "--no-llm", "--show-progress"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(progress_lines(&o).is_empty(), "{}", stderr(&o));
    // Aucun changement.
    let o = run(&repo, &["update", "--no-llm", "--show-progress"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("graphe à jour"));
    assert!(progress_lines(&o).is_empty());
}

#[test]
fn req043_ac10_no_option_no_line() {
    let (_t, repo) = repo_with(3);
    for args in [
        vec!["build", "--no-llm"],
        vec!["build", "--no-llm", "-v"],
        vec!["update", "--no-llm"],
        vec!["update", "--no-llm", "--verbose"],
    ] {
        let o = run(&repo, &args);
        assert!(progress_lines(&o).is_empty(), "{args:?}");
    }
}

#[test]
fn req043_ac11_help_text() {
    for sub in ["build", "update"] {
        let o = run(Path::new("."), &[sub, "--help"]);
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        let verbose = "-v, --verbose Traces synthétiques des étapes et des appels LLM sur stderr (préfixe `trace:` ; incompatible avec --show-progress) ";
        let progress = "--show-progress Avancement des phases extraction et enrichissement sur stderr, par paliers de 10 % (préfixe `progression:` ; incompatible avec --verbose) ";
        let both = format!("{verbose}{progress}--batch-files");
        assert!(flat.contains(&both), "{sub}: {flat}");
        assert!(!flat.contains("-s, --show-progress"));
        assert!(!flat.contains("--show-progress <"));
    }
}
