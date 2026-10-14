//! Reprise après échec du LLM et verrou d'écriture (REQ-010, REQ-020).

use assert_cmd::cargo::cargo_bin;
use rkg::model::FileState;
use rkg::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

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
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let repo = tmp.path().join("repo");
        copy_dir(&fixtures.join("maven2"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fixtures.join("mock"), &mock);
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            mock,
            log,
        }
    }

    fn command(&self, sub: &str, extra: &[(&str, &str)]) -> Command {
        let mut c = Command::new(cargo_bin("rkg"));
        c.arg("-C")
            .arg(&self.repo)
            .arg(sub)
            .env_remove("RKG_MOCK_FAIL_AFTER")
            .env_remove("RKG_MOCK_DELAY_MS")
            .env_remove("RKG_MOCK_INVALID_KEY")
            .env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log);
        for (k, v) in extra {
            c.env(k, v);
        }
        c
    }

    fn run(&self, sub: &str, extra: &[(&str, &str)]) -> Output {
        self.command(sub, extra).output().unwrap()
    }

    /// Lance un build ralenti et attend qu'il ait commencé à appeler le LLM.
    fn spawn_slow_build(&self) -> Child {
        let child = self
            .command("build", &[("RKG_MOCK_DELAY_MS", "400")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while fs::read_to_string(&self.log).unwrap_or_default().is_empty() {
            assert!(start.elapsed() < Duration::from_secs(20), "build trop lent");
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn req010_ac1_failure_after_two_files_keeps_them_and_leaves_rest_pending() {
    let env = Env::new();
    let out = env.run("build", &[("RKG_MOCK_FAIL_AFTER", "2")]);
    assert!(!out.status.success());
    let store = Store::open_readonly(&env.repo).unwrap();
    let files = store.files().unwrap();
    let indexed: Vec<_> = files
        .iter()
        .filter(|f| f.state == FileState::Indexed)
        .collect();
    assert_eq!(indexed.len(), 2, "{files:?}");
    assert!(files.len() > 2);
    for n in store.nodes().unwrap() {
        if let Some(f) = &n.file {
            assert!(
                indexed.iter().any(|i| &i.path == f),
                "nœud {} d'un fichier non traité",
                n.id
            );
        }
    }
}

#[test]
fn req010_ac1_no_new_file_is_sent_after_first_failure() {
    let env = Env::new();
    let out = env.run("build", &[("RKG_MOCK_FAIL_AFTER", "2")]);
    assert!(!out.status.success());
    let calls = fs::read_to_string(&env.log).unwrap().lines().count();
    // 2 réussites + au plus un appel en vol par thread de travail.
    assert!(calls < 9, "{calls} appels : l'envoi n'a pas été arrêté");
}

#[test]
fn req010_ac3_killed_build_leaves_readable_graph_with_pending_files() {
    let env = Env::new();
    let mut child = env.spawn_slow_build();
    child.kill().unwrap();
    child.wait().unwrap();
    let store = Store::open_readonly(&env.repo).expect("graphe illisible après kill -9");
    let files = store.files().unwrap();
    assert!(!files.is_empty());
    assert!(files.iter().any(|f| f.state == FileState::Pending));
    store.nodes().unwrap();
    store.edges().unwrap();
}

#[test]
fn req020_ac1_second_build_fails_immediately_without_touching_graph() {
    let env = Env::new();
    let mut child = env.spawn_slow_build();
    let before = Store::open_readonly(&env.repo).unwrap().files().unwrap();
    let t = Instant::now();
    let out = env.run("build", &[]);
    assert!(t.elapsed() < Duration::from_secs(3), "build pas immédiat");
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("indexation déjà en cours"),
        "{}",
        stderr(&out)
    );
    let after = Store::open_readonly(&env.repo).unwrap().files().unwrap();
    assert_eq!(before.len(), after.len());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn req020_ac2_reads_work_during_build() {
    let env = Env::new();
    let mut child = env.spawn_slow_build();
    let store = Store::open_readonly(&env.repo).expect("lecture pendant le build");
    store.files().unwrap();
    store.nodes().unwrap();
    store.edges().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn req020_ac3_lock_of_dead_process_does_not_block_build() {
    let env = Env::new();
    let mut child = env.spawn_slow_build();
    child.kill().unwrap();
    child.wait().unwrap();
    let out = env.run("build", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn req020_ac3_stale_lock_file_without_holder_is_ignored() {
    let env = Env::new();
    fs::create_dir_all(env.repo.join(".rkg")).unwrap();
    fs::write(env.repo.join(".rkg/lock"), "12345\n").unwrap();
    let out = env.run("build", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn req020_lock_is_exclusive_and_released_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let first = rkg::index::lock::RepoLock::acquire(dir.path()).unwrap();
    let err = rkg::index::lock::RepoLock::acquire(dir.path()).unwrap_err();
    assert!(err.to_string().contains("indexation déjà en cours"));
    drop(first);
    rkg::index::lock::RepoLock::acquire(dir.path()).unwrap();
}
