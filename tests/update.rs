//! Commande `rkg update` de bout en bout avec le LLM simulé (REQ-011, REQ-008, REQ-010, REQ-019,
//! REQ-020).

use assert_cmd::cargo::cargo_bin;
use rkg::model::{EdgeKind, FileState};
use rkg::store::Store;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

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
    fn empty() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&mock).unwrap();
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

    /// Dépôt Maven d'exemple (10 fichiers) avec ses fixtures.
    fn maven() -> Env {
        let env = Env::empty();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir(&fx.join("maven2"), &env.repo);
        copy_dir(&fx.join("mock"), &env.mock);
        env
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn fixture(&self, rel: &str, json: &str) {
        let p = self.mock.join(format!("{rel}.json"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, json).unwrap();
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

    fn ok(&self, sub: &str) -> Output {
        let out = self.run(sub, &[]);
        assert!(out.status.success(), "{sub} : {}", stderr(&out));
        out
    }

    fn store(&self) -> Store {
        Store::open_readonly(&self.repo).unwrap()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn clear_log(&self) {
        let _ = fs::remove_file(&self.log);
    }

    fn logged_paths(&self) -> BTreeSet<String> {
        self.log_lines()
            .iter()
            .map(|l| l.split('\t').next().unwrap().to_string())
            .collect()
    }

    fn node_ids(&self) -> BTreeSet<String> {
        self.store()
            .nodes()
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect()
    }

    fn has_edge(&self, s: &str, k: EdgeKind, t: &str) -> bool {
        self.store()
            .edges()
            .unwrap()
            .iter()
            .any(|e| e.source == s && e.kind == k && e.target == t)
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const ORDER_SERVICE: &str = "core/src/main/java/com/acme/core/OrderService.java";
const ORDER_REPO: &str = "core/src/main/java/com/acme/core/OrderRepository.java";
const ORDER: &str = "core/src/main/java/com/acme/domain/Order.java";

fn java_fixture(class: &str) -> String {
    format!(
        "{{\"version\":1,\"declares\":[{{\"kind\":\"package\",\"name\":\"p\"}},{{\"kind\":\"type\",\"name\":\"p.{class}\",\"subkind\":\"class\"}}],\"references\":[]}}"
    )
}

#[test]
fn req011_ac7_no_graph_runs_full_build() {
    let env = Env::maven();
    let out = env.run("update", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("aucun graphe : build complet"));
    assert_eq!(env.logged_paths().len(), 10);
    assert!(env.node_ids().contains("type:com.acme.core.OrderService"));
}

#[test]
fn req011_ac2_up_to_date_makes_no_call_and_keeps_summary() {
    let env = Env::maven();
    env.ok("build");
    let before = env.store().summary().unwrap().unwrap();
    env.clear_log();
    std::thread::sleep(Duration::from_millis(20));
    let out = env.ok("update");
    assert!(stdout(&out).contains("graphe à jour"), "{}", stdout(&out));
    assert!(env.log_lines().is_empty());
    assert_eq!(env.store().summary().unwrap().unwrap(), before);
}

#[test]
fn req011_ac1_only_modified_file_is_sent_among_500() {
    let env = Env::empty();
    for i in 0..500 {
        let class = format!("C{i}");
        let rel = format!("src/p/{class}.java");
        env.write(&rel, &format!("package p;\nclass {class} {{}}\n"));
        env.fixture(&rel, &java_fixture(&class));
    }
    env.ok("build");
    let before = env.store().nodes().unwrap();
    assert!(before.len() >= 500);
    env.clear_log();
    env.write("src/p/C42.java", "package p;\nclass C42 { int x; }\n");
    env.ok("update");
    let lines = env.log_lines();
    assert_eq!(lines, vec!["src/p/C42.java\t1".to_string()], "{lines:?}");
    assert_eq!(env.store().nodes().unwrap(), before);
}

#[test]
fn req011_ac3_deleted_file_removes_its_nodes_and_links() {
    let env = Env::maven();
    env.ok("build");
    assert!(env.has_edge(
        "method:com.acme.core.OrderService.place",
        EdgeKind::Calls,
        "method:com.acme.core.OrderRepository.save"
    ));
    fs::remove_file(env.repo.join(ORDER_REPO)).unwrap();
    env.clear_log();
    env.ok("update");
    assert!(env.log_lines().is_empty());
    let ids = env.node_ids();
    assert!(!ids.contains("type:com.acme.core.OrderRepository"));
    assert!(!ids.contains("method:com.acme.core.OrderRepository.save"));
    let edges = env.store().edges().unwrap();
    assert!(edges
        .iter()
        .all(|e| !e.target.contains("OrderRepository") && !e.source.contains("OrderRepository")));
    assert!(ids.contains("type:com.acme.core.OrderService"));
}

#[test]
fn req011_ac4_rename_is_delete_plus_add() {
    let env = Env::maven();
    env.ok("build");
    let new = "core/src/main/java/com/acme/domain/Order2.java";
    fs::rename(env.repo.join(ORDER), env.repo.join(new)).unwrap();
    env.fixture(new, &java_fixture("Order2"));
    env.clear_log();
    env.ok("update");
    assert_eq!(env.logged_paths(), BTreeSet::from([new.to_string()]));
    let ids = env.node_ids();
    assert!(!ids.contains("type:com.acme.domain.Order"));
    assert!(ids.contains("type:p.Order2"));
    let files: Vec<String> = env
        .store()
        .files()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert!(!files.contains(&ORDER.to_string()));
    assert!(files.contains(&new.to_string()));
}

#[test]
fn req011_ac5_modified_file_replaces_nodes_and_refs() {
    let env = Env::maven();
    env.ok("build");
    assert!(env
        .node_ids()
        .contains("method:com.acme.core.OrderRepository.save"));
    env.fixture(
        ORDER_REPO,
        r#"{"version":1,"declares":[{"kind":"package","name":"com.acme.core"},{"kind":"type","name":"com.acme.core.OrderRepository","subkind":"interface"},{"kind":"method","name":"com.acme.core.OrderRepository.load"}],"references":[]}"#,
    );
    let mut text = fs::read_to_string(env.repo.join(ORDER_REPO)).unwrap();
    text.push_str("\n// modifié\n");
    env.write(ORDER_REPO, &text);
    env.clear_log();
    env.ok("update");
    assert_eq!(env.logged_paths(), BTreeSet::from([ORDER_REPO.to_string()]));
    let ids = env.node_ids();
    assert!(!ids.contains("method:com.acme.core.OrderRepository.save"));
    assert!(ids.contains("method:com.acme.core.OrderRepository.load"));
    // La référence de OrderService.place vers OrderRepository.save n'est plus résolue.
    assert!(!env.has_edge(
        "method:com.acme.core.OrderService.place",
        EdgeKind::Calls,
        "method:com.acme.core.OrderRepository.save"
    ));
}

#[test]
fn req011_ac6_touch_only_makes_no_call() {
    let env = Env::maven();
    env.ok("build");
    let f = fs::OpenOptions::new()
        .write(true)
        .open(env.repo.join(ORDER_SERVICE))
        .unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(3600))
        .unwrap();
    drop(f);
    env.clear_log();
    let out = env.ok("update");
    assert!(env.log_lines().is_empty());
    assert!(stdout(&out).contains("graphe à jour"));
}

#[test]
fn req011_ac8_pending_files_are_processed_even_if_unchanged() {
    let env = Env::maven();
    let failed = env.run("build", &[("RKG_MOCK_FAIL_AFTER", "2")]);
    assert!(!failed.status.success());
    let pending: BTreeSet<String> = env
        .store()
        .files()
        .unwrap()
        .into_iter()
        .filter(|f| f.state == FileState::Pending)
        .map(|f| f.path)
        .collect();
    assert_eq!(pending.len(), 8);
    env.clear_log();
    env.ok("update");
    assert_eq!(env.logged_paths(), pending);
    assert!(env
        .store()
        .files()
        .unwrap()
        .iter()
        .all(|f| f.state == FileState::Indexed));
}

#[test]
fn req010_ac2_update_after_failure_sends_exactly_the_pending_files() {
    let env = Env::maven();
    env.run("build", &[("RKG_MOCK_FAIL_AFTER", "7")]);
    let pending: BTreeSet<String> = env
        .store()
        .files()
        .unwrap()
        .into_iter()
        .filter(|f| f.state == FileState::Pending)
        .map(|f| f.path)
        .collect();
    assert_eq!(pending.len(), 3);
    env.clear_log();
    env.ok("update");
    assert_eq!(env.logged_paths(), pending);
    assert_eq!(env.log_lines().len(), 3);
}

#[test]
fn req011_failed_update_keeps_failed_file_pending_and_exits_nonzero() {
    let env = Env::maven();
    env.ok("build");
    for f in [ORDER, ORDER_REPO] {
        let mut t = fs::read_to_string(env.repo.join(f)).unwrap();
        t.push_str("\n// x\n");
        env.write(f, &t);
    }
    let out = env.run("update", &[("RKG_MOCK_FAIL_AFTER", "1")]);
    assert!(!out.status.success());
    let pending = env
        .store()
        .files()
        .unwrap()
        .iter()
        .filter(|f| f.state == FileState::Pending)
        .count();
    assert_eq!(pending, 1);
}

fn mini_ab(env: &Env) {
    env.write("A.java", "class A { void m() { B.foo(); } }\n");
    env.write("B.java", "class B { }\n");
    env.fixture(
        "A.java",
        r#"{"version":1,"declares":[{"kind":"type","name":"A","subkind":"class"},{"kind":"method","name":"A.m"}],"references":[{"source":"method:A.m","kind":"calls","target":"B.foo"}]}"#,
    );
    env.fixture(
        "B.java",
        r#"{"version":1,"declares":[{"kind":"type","name":"B","subkind":"class"}],"references":[]}"#,
    );
}

#[test]
fn req008_ac2_new_method_in_b_links_unchanged_a() {
    let env = Env::empty();
    mini_ab(&env);
    env.ok("build");
    assert!(!env.has_edge("method:A.m", EdgeKind::Calls, "method:B.foo"));
    env.fixture(
        "B.java",
        r#"{"version":1,"declares":[{"kind":"type","name":"B","subkind":"class"},{"kind":"method","name":"B.foo"}],"references":[]}"#,
    );
    env.write("B.java", "class B { void foo() {} }\n");
    env.clear_log();
    env.ok("update");
    assert_eq!(env.logged_paths(), BTreeSet::from(["B.java".to_string()]));
    assert!(env.has_edge("method:A.m", EdgeKind::Calls, "method:B.foo"));
}

#[test]
fn req011_same_size_edit_with_restored_mtime_is_detected() {
    let env = Env::empty();
    mini_ab(&env);
    env.write("B.java", "class B { void foo() {} }\n");
    env.ok("build");
    let path = env.repo.join("B.java");
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, "class B { void bar() {} }\n").unwrap();
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(mtime).unwrap();
    drop(f);
    env.clear_log();
    let out = env.ok("update");
    assert!(!stdout(&out).contains("graphe à jour"), "{}", stdout(&out));
    assert!(env.logged_paths().contains("B.java"));
}

#[test]
fn req008_ac3_renamed_method_unlinks_a_and_reference_is_unresolved() {
    let env = Env::empty();
    mini_ab(&env);
    env.fixture(
        "B.java",
        r#"{"version":1,"declares":[{"kind":"type","name":"B","subkind":"class"},{"kind":"method","name":"B.foo"}],"references":[]}"#,
    );
    env.write("B.java", "class B { void foo() {} }\n");
    env.ok("build");
    assert!(env.has_edge("method:A.m", EdgeKind::Calls, "method:B.foo"));
    env.fixture(
        "B.java",
        r#"{"version":1,"declares":[{"kind":"type","name":"B","subkind":"class"},{"kind":"method","name":"B.bar"}],"references":[]}"#,
    );
    env.write("B.java", "class B { void bar() {} }\n");
    env.clear_log();
    env.ok("update");
    assert!(!env.has_edge("method:A.m", EdgeKind::Calls, "method:B.foo"));
    let out = env.ok("export");
    assert!(stdout(&out).contains("B.foo"), "{}", stdout(&out));
    let unresolved = rkg::resolve::unresolved(&env.store()).unwrap();
    assert!(unresolved.iter().any(|r| r.target_name == "B.foo"));
}

#[test]
fn req019_ac2_summary_recomputed_on_update_with_change() {
    let env = Env::maven();
    env.ok("build");
    let before = env.store().summary().unwrap().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let mut t = fs::read_to_string(env.repo.join(ORDER)).unwrap();
    t.push_str("\n// x\n");
    env.write(ORDER, &t);
    env.ok("update");
    let after = env.store().summary().unwrap().unwrap();
    assert!(after.1 > before.1);
}

#[test]
fn req019_ac2_summary_reflects_deletion() {
    let env = Env::maven();
    env.ok("build");
    fs::remove_file(env.repo.join("core/pom.xml")).unwrap();
    env.ok("update");
    let out = env.ok("summary");
    assert!(!stdout(&out).contains("core ->") && !stdout(&out).lines().any(|l| l.trim() == "core"));
}

#[test]
fn req019_ac3_up_to_date_update_does_not_touch_summary() {
    let env = Env::maven();
    env.ok("build");
    let before = env.store().summary().unwrap().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    env.ok("update");
    assert_eq!(env.store().summary().unwrap().unwrap(), before);
}

#[test]
fn req020_ac1_update_fails_while_build_runs() {
    let env = Env::maven();
    let mut child = env
        .command("build", &[("RKG_MOCK_DELAY_MS", "400")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while fs::read_to_string(&env.log).unwrap_or_default().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(20), "build trop lent");
        std::thread::sleep(Duration::from_millis(20));
    }
    let t = Instant::now();
    let out = env.run("update", &[]);
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("indexation déjà en cours"),
        "{}",
        stderr(&out)
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn req020_ac1_update_without_graph_also_takes_lock() {
    let env = Env::maven();
    let mut child = env
        .command("update", &[("RKG_MOCK_DELAY_MS", "400")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while fs::read_to_string(&env.log).unwrap_or_default().is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "update trop lent"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = env.run("update", &[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("indexation déjà en cours"),
        "{}",
        stderr(&out)
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn req009_ac4_update_on_missing_root_fails_without_creating_rkg() {
    let env = Env::maven();
    let missing = env.repo.join("absent");
    let out = Command::new(cargo_bin("rkg"))
        .arg("-C")
        .arg(&missing)
        .arg("update")
        .env("RKG_CONFIG", &env.config)
        .env("RKG_MOCK_DIR", &env.mock)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("n'est pas un dossier"),
        "{}",
        stderr(&out)
    );
    assert!(!missing.exists());
}

#[test]
fn req011_update_on_unreadable_root_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::maven();
    env.ok("build");
    let before = env.store().summary().unwrap().unwrap();
    fs::set_permissions(&env.repo, fs::Permissions::from_mode(0o000)).unwrap();
    let out = env.run("update", &[]);
    let is_root = fs::read_dir(&env.repo).is_ok();
    fs::set_permissions(&env.repo, fs::Permissions::from_mode(0o755)).unwrap();
    if is_root {
        return; // exécuté en root : les droits ne s'appliquent pas
    }
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("n'est pas un dossier lisible"),
        "{}",
        stderr(&out)
    );
    assert_eq!(env.store().summary().unwrap().unwrap(), before);
}

#[test]
fn req011_update_on_old_schema_asks_for_build_and_keeps_db() {
    let env = Env::maven();
    env.ok("build");
    let db = env.repo.join(".rkg/graph.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }
    let size = fs::metadata(&db).unwrap().len();
    let out = env.run("update", &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("rkg build"), "{}", stderr(&out));
    assert_eq!(fs::metadata(&db).unwrap().len(), size);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 10);
}

#[test]
fn req002_update_checks_key_before_touching_anything() {
    let env = Env::maven();
    env.ok("build");
    let mut t = fs::read_to_string(env.repo.join(ORDER)).unwrap();
    t.push_str("\n// x\n");
    env.write(ORDER, &t);
    env.clear_log();
    let out = env.run("update", &[("RKG_MOCK_INVALID_KEY", "1")]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("clé"), "{}", stderr(&out));
    assert!(env.log_lines().is_empty());
}
