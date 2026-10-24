//! Option `--show-progress` de build et update (REQ-043) : extraction et enrichissement.

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
    fs::write(
        &cfg,
        "provider = \"mock\"\nenrich_code = true\nbatch_files = 1\nmodel = \"mock\"\n",
    )
    .unwrap();
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
        let both = format!("{verbose}{progress}--jobs");
        assert!(flat.contains(&both), "{sub}: {flat}");
        assert!(!flat.contains("-s, --show-progress"));
        assert!(!flat.contains("--show-progress <"));
    }
}

// ---------------------------------------------------------------- enrichissement (T-091)

struct Mock {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Mock {
    fn new(n: usize, config: &str) -> Mock {
        let (tmp, repo) = repo_with(n);
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&mock).unwrap();
        let cfg = tmp.path().join("config.toml");
        fs::write(
            &cfg,
            format!("provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n{config}"),
        )
        .unwrap();
        let log = tmp.path().join("mock.log");
        Mock {
            _tmp: tmp,
            repo,
            config: cfg,
            mock,
            log,
        }
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let mut c = Command::cargo_bin("nephila").unwrap();
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
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .arg("-C")
            .arg(&self.repo)
            .args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn graph(&self) -> String {
        let s = nephila::store::Store::open_readonly(&self.repo).unwrap();
        let mut nodes: Vec<String> = s
            .nodes()
            .unwrap()
            .iter()
            .map(|n| format!("{n:?}"))
            .collect();
        let mut edges: Vec<String> = s
            .edges()
            .unwrap()
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        nodes.sort();
        edges.sort();
        format!("{nodes:?}\n{edges:?}")
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn enrich_lines(o: &std::process::Output) -> Vec<String> {
    progress_lines(o)
        .into_iter()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect()
}

fn ks(lines: &[String], n: usize) -> Vec<usize> {
    lines
        .iter()
        .map(|l| {
            let tail = l.split('(').nth(1).unwrap();
            let (k, rest) = tail.split_once('/').unwrap();
            assert_eq!(rest, format!("{n} fichiers)"), "{l}");
            k.parse().unwrap()
        })
        .collect()
}

fn without_progress(o: &std::process::Output) -> String {
    stderr(o)
        .lines()
        .filter(|l| !l.starts_with("progression:"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn req043_ac4_ac12_mock_three_files_exact_lines_and_same_effects() {
    let a = Mock::new(3, "concurrency = 1\nbatch_files = 1\n");
    let b = Mock::new(3, "concurrency = 1\nbatch_files = 1\n");
    let o = a.run(&["build", "--show-progress"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let plain = b.run(&["build"], &[]);
    assert_eq!(
        stderr(&o).lines().collect::<Vec<_>>(),
        [
            "progression: extraction 0 % (0/3 fichiers)",
            "progression: extraction 33 % (1/3 fichiers)",
            "progression: extraction 66 % (2/3 fichiers)",
            "progression: extraction 100 % (3/3 fichiers)",
            "progression: enrichissement 0 % (0/3 fichiers)",
            "progression: enrichissement 33 % (1/3 fichiers)",
            "progression: enrichissement 66 % (2/3 fichiers)",
            "progression: enrichissement 100 % (3/3 fichiers)",
        ]
    );
    assert!(stderr(&plain).is_empty(), "{}", stderr(&plain));
    assert_eq!(o.stdout, plain.stdout);
    assert_eq!(a.log(), b.log());
    assert!(!a.log().is_empty());
    assert_eq!(a.graph(), b.graph());
}

#[test]
fn req043_ac5_batch_of_four_moves_by_block() {
    let m = Mock::new(10, "concurrency = 1\nbatch_files = 4\n");
    let o = m.run(&["build", "--show-progress"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(ks(&enrich_lines(&o), 10), [0, 4, 8, 10]);
    assert_eq!(
        enrich_lines(&o)[1],
        "progression: enrichissement 40 % (4/10 fichiers)"
    );
}

#[test]
fn req043_ac5_chunks_count_once_per_file() {
    let m = Mock::new(
        2,
        "concurrency = 1\nbatch_files = 1\nmax_chunk_chars = 300\n",
    );
    let methods: String = (0..12)
        .map(|i| format!("public void method_number_{i}() {{ int a{i} = {i}; }}\n"))
        .collect();
    write_java(&m.repo, 0, &methods);
    let o = m.run(&["build", "--show-progress"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let calls_c0 = m
        .log()
        .lines()
        .filter(|l| l.starts_with("C0.java\t"))
        .count();
    assert_eq!(calls_c0, 3, "{}", m.log());
    assert_eq!(ks(&enrich_lines(&o), 2), [0, 1, 2]);
}

#[test]
fn req043_ac5_format_retries_do_not_count() {
    let m = Mock::new(2, "concurrency = 1\nbatch_files = 1\n");
    let o = m.run(
        &["build", "--show-progress"],
        &[("NEPHILA_MOCK_MALFORMED", "C0.java:2")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(m.log().lines().filter(|l| l.starts_with("C0.java")).count() >= 2);
    assert_eq!(ks(&enrich_lines(&o), 2), [0, 1, 2]);
}

#[test]
fn req043_ac6_ac13_timeout_file_counts_as_processed() {
    let a = Mock::new(4, "concurrency = 1\nbatch_files = 1\n");
    let b = Mock::new(4, "concurrency = 1\nbatch_files = 1\n");
    let env = [("NEPHILA_MOCK_TIMEOUT_FILES", "C1.java")];
    let o = a.run(&["build", "--show-progress"], &env);
    let plain = b.run(&["build"], &env);
    assert_eq!(
        enrich_lines(&o).last().unwrap(),
        "progression: enrichissement 100 % (4/4 fichiers)"
    );
    assert!(stderr(&o).contains("avertissement:"), "{}", stderr(&o));
    assert_eq!(without_progress(&o), stderr(&plain).trim_end());
    assert_eq!(o.status.code(), plain.status.code());
    assert_eq!(o.stdout, plain.stdout);
}

#[test]
fn req043_ac6_ac13_stop_leaves_enrichment_under_100() {
    let a = Mock::new(8, "concurrency = 1\nbatch_files = 1\n");
    let b = Mock::new(8, "concurrency = 1\nbatch_files = 1\n");
    let env = [("NEPHILA_MOCK_FAIL_AFTER", "2")];
    let o = a.run(&["build", "--show-progress"], &env);
    let plain = b.run(&["build"], &env);
    assert_ne!(o.status.code(), Some(0));
    let lines = enrich_lines(&o);
    assert_eq!(
        lines.last().unwrap(),
        "progression: enrichissement 62 % (5/8 fichiers)"
    );
    assert!(!stderr(&o).contains("enrichissement 100 %"));
    let all: Vec<String> = stderr(&o).lines().map(str::to_string).collect();
    let err_pos = all.iter().position(|l| l.starts_with("erreur:")).unwrap();
    assert!(all[err_pos..]
        .iter()
        .all(|l| !l.starts_with("progression:")));
    assert_eq!(without_progress(&o), stderr(&plain).trim_end());
    assert_eq!(o.status.code(), plain.status.code());
    assert_eq!(o.stdout, plain.stdout);
}

#[test]
fn req043_ac6_invalid_key_progress_stops_before_error_line() {
    let m = Mock::new(4, "concurrency = 1\nbatch_files = 1\n");
    let o = m.run(
        &["build", "--show-progress"],
        &[("NEPHILA_MOCK_INVALID_KEY", "1")],
    );
    assert_ne!(o.status.code(), Some(0));
    // REQ-054 : la clé est constatée au premier appel, après l'extraction.
    let err = stderr(&o);
    let lines: Vec<&str> = err.lines().collect();
    let ext = lines
        .iter()
        .position(|l| *l == "progression: extraction 100 % (4/4 fichiers)")
        .unwrap_or_else(|| panic!("{err}"));
    let enr = lines
        .iter()
        .position(|l| l.starts_with("progression: enrichissement 0 % (0/"))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(ext < enr, "{err}");
    let errs: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].starts_with("erreur:"))
        .collect();
    assert_eq!(errs.len(), 1, "{err}");
    assert!(lines[errs[0]].contains("clé d'API refusée"), "{err}");
    assert_eq!(errs[0], lines.len() - 1, "{err}");
    assert!(
        lines[errs[0]..]
            .iter()
            .all(|l| !l.starts_with("progression:")),
        "{err}"
    );
}

#[test]
fn req043_ac7_update_counts_only_files_to_process() {
    let m = Mock::new(10, "concurrency = 1\nbatch_files = 1\n");
    assert!(m.run(&["build"], &[]).status.success());
    write_java(&m.repo, 2, "int x;");
    write_java(&m.repo, 7, "int y;");
    let o = m.run(&["update", "--show-progress"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        stderr(&o).lines().collect::<Vec<_>>(),
        [
            "progression: extraction 0 % (0/2 fichiers)",
            "progression: extraction 50 % (1/2 fichiers)",
            "progression: extraction 100 % (2/2 fichiers)",
            "progression: enrichissement 0 % (0/2 fichiers)",
            "progression: enrichissement 50 % (1/2 fichiers)",
            "progression: enrichissement 100 % (2/2 fichiers)",
        ]
    );
    // Sans graphe : lignes d'un build complet.
    let fresh = Mock::new(10, "concurrency = 1\nbatch_files = 1\n");
    let o = fresh.run(&["update", "--show-progress"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        ks(&progress_lines(&o)[..11], 10),
        (0..=10).collect::<Vec<_>>()
    );
    assert_eq!(enrich_lines(&o).len(), 11);
}

#[test]
fn req043_ac8_multi_thread_lines_whole_and_monotonic() {
    let m = Mock::new(40, "concurrency = 4\nbatch_files = 3\n");
    let o = m.run(&["build", "--show-progress"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    for l in stderr(&o).lines() {
        assert!(
            l.starts_with("progression: ")
                || l.starts_with("avertissement:")
                || l.starts_with("erreur:"),
            "{l}"
        );
    }
    for phase in ["extraction", "enrichissement"] {
        let lines: Vec<String> = progress_lines(&o)
            .into_iter()
            .filter(|l| l.starts_with(&format!("progression: {phase}")))
            .collect();
        let k = ks(&lines, 40);
        assert!(k.windows(2).all(|w| w[0] < w[1]), "{phase} {k:?}");
        assert_eq!(*k.last().unwrap(), 40);
    }
}

#[test]
fn req043_ac9_redirected_output_is_plain_and_free_of_secrets() {
    let m = Mock::new(
        4,
        "concurrency = 1\nbatch_files = 1\napi_key_env = \"NEPHILA_T091_KEY\"\n",
    );
    write_java(&m.repo, 0, "String s = \"SECRET_CONTENU_42\";");
    let key = "sk-cle-secrete-91";
    let env = [
        ("NEPHILA_MOCK_TIMEOUT_FILES", "C1.java"),
        ("NEPHILA_T091_KEY", key),
    ];
    let a = m.run(&["build", "--show-progress"], &env);
    let mut env_nc = env.to_vec();
    env_nc.push(("NO_COLOR", "1"));
    // Graphe neuf : un build sans changement n'enrichit plus que le fichier en échec (REQ-049).
    fs::remove_dir_all(m.repo.join(".nephila")).unwrap();
    let b = m.run(&["build", "--show-progress"], &env_nc);
    for o in [&a, &b] {
        for stream in [&o.stdout, &o.stderr] {
            assert!(!stream.contains(&0x1B) && !stream.contains(&b'\r'));
        }
        for l in progress_lines(o) {
            assert!(!l.contains("SECRET_CONTENU_42") && !l.contains(key), "{l}");
        }
    }
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
}
