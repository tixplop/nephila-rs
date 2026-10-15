//! Enrichissement en lots avec le fournisseur simulé (REQ-027, REQ-003, REQ-024).

use assert_cmd::cargo::cargo_bin;
use rkg::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

impl Env {
    /// `n` petits fichiers Java `src/F00.java`… ; fixtures : un résumé par type.
    fn new(n: usize, batch: usize, concurrency: usize) -> Env {
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
        let config = tmp.path().join("config.toml");
        let env = Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config,
        };
        env.set_config(batch, concurrency, 60000);
        env
    }

    fn set_config(&self, batch: usize, concurrency: usize, max_chunk: usize) {
        fs::write(
            &self.config,
            format!(
                "provider = \"mock\"\nmodel = \"mock\"\nconcurrency = {concurrency}\nbatch_files = {batch}\nmax_chunk_chars = {max_chunk}\n"
            ),
        )
        .unwrap();
    }

    fn run(&self, sub: &str, extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(cargo_bin("rkg"));
        c.arg("-C").arg(&self.repo).args(sub.split_whitespace());
        for v in [
            "RKG_MOCK_FAIL_AFTER",
            "RKG_MOCK_DELAY_MS",
            "RKG_MOCK_INVALID_KEY",
            "RKG_MOCK_TIMEOUT_FILES",
            "RKG_MOCK_KEY_REVOKED_AFTER",
        ] {
            c.env_remove(v);
        }
        c.env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log);
        for (k, v) in extra {
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

    fn summaries(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .filter_map(|n| n.summary.map(|s| (n.id, s)))
            .collect();
        v.sort();
        v
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn files_of(line: &str) -> Vec<String> {
    line.split('\t')
        .next()
        .unwrap()
        .split(';')
        .map(String::from)
        .collect()
}

fn to_enrich(env: &Env) -> usize {
    Store::open_readonly(&env.repo)
        .unwrap()
        .files_to_enrich()
        .unwrap()
        .len()
}

#[test]
fn req027_ac2_batch_one_logs_one_line_per_file() {
    let env = Env::new(5, 1, 1);
    let o = env.run("build", &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let log = env.log_lines();
    assert_eq!(log.len(), 5);
    assert!(log.iter().all(|l| !l.contains(';') && l.ends_with("\t1")));
    assert_eq!(env.summaries().len(), 5);

    // Sans la clé batch_files : même comportement que batch_files = 1.
    let env2 = Env::new(5, 1, 1);
    fs::write(
        &env2.config,
        "provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 1\nmax_chunk_chars = 60000\n",
    )
    .unwrap();
    let o2 = env2.run("build", &[]);
    assert!(o2.status.success(), "{}", stderr(&o2));
    assert_eq!(env2.log_lines(), log);
    let e1 = env.run("export --output -", &[]);
    let e2 = env2.run("export --output -", &[]);
    assert!(
        e1.status.success() && e2.status.success(),
        "{}",
        stderr(&e2)
    );
    assert_eq!(e1.stdout, e2.stdout);
}

#[test]
fn req027_ac3_ten_files_batch_four_gives_4_4_2() {
    let env = Env::new(10, 4, 1);
    let o = env.run("build", &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let log = env.log_lines();
    let sizes: Vec<usize> = log.iter().map(|l| files_of(l).len()).collect();
    assert_eq!(sizes, [4, 4, 2]);
    let all: Vec<String> = log.iter().flat_map(|l| files_of(l)).collect();
    let mut sorted = all.clone();
    sorted.sort();
    assert_eq!(all, sorted);
    assert_eq!(all.len(), 10);
    assert!(log.iter().all(|l| l.ends_with("\t1")));
    assert_eq!(env.summaries().len(), 10);
}

#[test]
fn req027_ac3_oversized_file_is_isolated_and_lots_respect_byte_bound() {
    let env = Env::new(6, 4, 1);
    // F02 est gros : plusieurs morceaux ; les autres font ~40 octets, deux ne tiennent pas ensemble.
    let big = format!(
        "package p;\npublic class F02 {{}}\n{}",
        "// remplissage\n".repeat(20)
    );
    fs::write(env.repo.join("src/F02.java"), &big).unwrap();
    for i in [0, 1, 3, 4, 5] {
        fs::write(
            env.repo.join(format!("src/F{i:02}.java")),
            format!("package p;\nclass F{i:02} {{}}\n"),
        )
        .unwrap();
    }
    env.set_config(4, 1, 60);
    let o = env.run("build", &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let log = env.log_lines();
    let own: Vec<&String> = log
        .iter()
        .filter(|l| l.starts_with("src/F02.java\t"))
        .collect();
    assert!(own.len() > 1, "{log:?}");
    for (i, l) in own.iter().enumerate() {
        assert!(l.ends_with(&format!("\t{}", i + 1)));
    }
    assert!(log.iter().any(|l| l.contains(';')), "aucun lot : {log:?}");
    for l in &log {
        let fs_: Vec<String> = files_of(l);
        let total: u64 = fs_
            .iter()
            .map(|f| fs::metadata(env.repo.join(f)).unwrap().len())
            .sum();
        if fs_.len() > 1 {
            assert!(total <= 60, "{l} : {total}");
            assert!(!fs_.contains(&"src/F02.java".to_string()));
        }
    }
    for i in [0, 1, 3, 4, 5] {
        let name = format!("src/F{i:02}.java");
        assert_eq!(
            log.iter().filter(|l| files_of(l).contains(&name)).count(),
            1,
            "{name}"
        );
    }
}

fn graph_and_warnings(batch: usize, concurrency: usize) -> (String, Vec<String>) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    copy_dir(&fixtures.join("maven2"), &repo);
    let mock = tmp.path().join("mock");
    copy_dir(&fixtures.join("mock"), &mock);
    let svc = mock.join("core/src/main/java/com/acme/core/OrderService.java.json");
    let text = fs::read_to_string(&svc).unwrap();
    assert!(text.contains("\"summaries\":["), "{text}");
    fs::write(
        &svc,
        text.replacen(
            "\"summaries\":[",
            "\"summaries\":[{\"node\":\"type:inconnu.X\",\"summary\":\"x\"},",
            1,
        ),
    )
    .unwrap();
    let cfg = tmp.path().join("c.toml");
    fs::write(
        &cfg,
        format!(
            "provider=\"mock\"\nmodel=\"mock\"\nconcurrency={concurrency}\nbatch_files={batch}\n"
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
        let o = Command::new(cargo_bin("rkg"))
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("RKG_CONFIG", &cfg)
            .env("RKG_MOCK_DIR", &mock)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    let (_, err) = run(&["build"]);
    let mut warns: Vec<String> = err
        .lines()
        .filter(|l| l.contains("écarté"))
        .map(String::from)
        .collect();
    warns.sort();
    let (graph, _) = run(&["export", "--output", "-"]);
    (graph, warns)
}

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

#[test]
fn req027_ac4_same_graph_and_warnings_for_any_batch_and_concurrency() {
    let (g1, w1) = graph_and_warnings(1, 1);
    assert!(g1.contains("\"summary\":\"Place les commandes.\""));
    assert!(!w1.is_empty(), "avertissements d'éléments écartés attendus");
    for (b, c) in [(4, 1), (4, 4), (1, 4)] {
        let (g, w) = graph_and_warnings(b, c);
        assert_eq!(g, g1, "batch={b} concurrency={c}");
        assert_eq!(w, w1, "batch={b} concurrency={c}");
    }
}

#[test]
fn req027_ac5_lots_run_in_parallel() {
    let env = Env::new(16, 4, 4);
    let mut c = Command::new(cargo_bin("rkg"));
    c.arg("-C")
        .arg(&env.repo)
        .args(["build", "--verbose"])
        .env("RKG_CONFIG", &env.config)
        .env("RKG_MOCK_DIR", &env.mock)
        .env("RKG_MOCK_LOG", &env.log)
        .env("RKG_MOCK_DELAY_MS", "400");
    let started = Instant::now();
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(started.elapsed().as_millis() < 5000);
    assert_eq!(env.log_lines().len(), 4);
    let err = stderr(&out);
    let line = err
        .lines()
        .find(|l| l.contains("enrichissement: fin"))
        .unwrap_or_else(|| panic!("{err}"));
    let ms: u128 = line
        .split("fin ")
        .nth(1)
        .unwrap()
        .split(" ms")
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(ms < 1200, "{ms} ms");
}

#[test]
fn req027_ac8_non_conforming_fixture_fails_whole_lot_only() {
    let env = Env::new(8, 4, 1);
    fs::write(env.mock.join("src/F05.java.json"), "pas du json").unwrap();
    let o = env.run("build", &[]);
    assert!(!o.status.success());
    let err = stderr(&o);
    for i in 4..8 {
        assert!(err.contains(&format!("F{i:02}.java")), "{err}");
    }
    assert_eq!(to_enrich(&env), 4);
    assert_eq!(env.summaries().len(), 4);
}
