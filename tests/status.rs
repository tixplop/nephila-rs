//! Commande `status` : détection des changements (REQ-012, AC-010-3, AC-001-4, AC-020-2).

use assert_cmd::cargo::cargo_bin;
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

const JAVA: &str = "core/src/main/java/com/acme/core/OrderService.java";
const JAVA2: &str = "core/src/main/java/com/acme/domain/Order.java";

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

    fn command(&self, args: &[&str], extra: &[(&str, &str)]) -> Command {
        let mut c = Command::new(cargo_bin("rkg"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
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

    fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        self.command(args, extra).output().unwrap()
    }

    /// `status` sans aucune configuration LLM ni fixture.
    fn status(&self, extra_args: &[&str]) -> Output {
        let mut args = vec!["status"];
        args.extend_from_slice(extra_args);
        let mut c = Command::new(cargo_bin("rkg"));
        c.arg("-C")
            .arg(&self.repo)
            .args(&args)
            .env("RKG_CONFIG", self.repo.join("absent.toml"))
            .env_remove("RKG_MOCK_DIR")
            .env_remove("RKG_MOCK_LOG");
        c.output().unwrap()
    }

    fn built() -> Env {
        let env = Env::new();
        let out = env.run(&["build"], &[]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        env
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn lines(o: &Output) -> Vec<String> {
    text(&o.stdout).lines().map(str::to_string).collect()
}

#[test]
fn req012_ac1_modified_added_deleted_are_listed() {
    let env = Env::built();
    fs::write(env.repo.join(JAVA), "class OrderService { int changed; }\n").unwrap();
    fs::write(env.repo.join("README.md"), "# modifié\n").unwrap();
    fs::write(env.repo.join("docs/nouveau.md"), "# nouveau\n").unwrap();
    fs::remove_file(env.repo.join(JAVA2)).unwrap();
    let out = env.status(&[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let l = lines(&out);
    assert_eq!(l[0], "graphe pas à jour");
    assert_eq!(l.len(), 5, "{l:?}");
    assert!(l.contains(&format!("modifié\t{JAVA}")), "{l:?}");
    assert!(l.contains(&"modifié\tREADME.md".to_string()), "{l:?}");
    assert!(l.contains(&"ajouté\tdocs/nouveau.md".to_string()), "{l:?}");
    assert!(l.contains(&format!("supprimé\t{JAVA2}")), "{l:?}");
}

#[test]
fn req012_ac2_unchanged_graph_is_up_to_date() {
    let env = Env::built();
    let out = env.status(&[]);
    assert!(out.status.success());
    assert_eq!(lines(&out), vec!["graphe à jour"]);
}

#[test]
fn req012_ac2_touch_only_is_still_up_to_date() {
    let env = Env::built();
    let p = env.repo.join(JAVA);
    let content = fs::read(&p).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&p, content).unwrap();
    assert_eq!(lines(&env.status(&[])), vec!["graphe à jour"]);
}

#[test]
fn req012_ac3_pending_files_after_failure_are_listed() {
    let env = Env::new();
    let out = env.run(&["build"], &[("RKG_MOCK_FAIL_AFTER", "2")]);
    assert!(!out.status.success());
    let out = env.status(&[]);
    assert!(out.status.success());
    let l = lines(&out);
    assert_eq!(l[0], "graphe pas à jour");
    let mut got: Vec<String> = l[1..].to_vec();
    got.sort();
    // L'ordre de traitement dépend de la concurrence : l'ensemble attendu est celui des fichiers
    // que le graphe ne marque pas indexés.
    let files = rkg::store::Store::open_readonly(&env.repo)
        .unwrap()
        .files()
        .unwrap();
    let indexed: Vec<&str> = files
        .iter()
        .filter(|f| f.state == rkg::model::FileState::Indexed)
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(indexed.len(), 2, "{indexed:?}");
    let mut expected: Vec<String> = files
        .iter()
        .filter(|f| f.state == rkg::model::FileState::Pending)
        .map(|f| format!("en attente\t{}", f.path))
        .collect();
    expected.sort();
    assert_eq!(expected.len(), 8);
    assert_eq!(got, expected, "{l:?}");
    for p in indexed {
        assert!(l.iter().all(|x| !x.ends_with(p)), "{l:?}");
    }
}

#[test]
fn req012_ac3_pending_file_removed_from_disk_is_reported_deleted() {
    let env = Env::new();
    let out = env.run(&["build"], &[("RKG_MOCK_FAIL_AFTER", "2")]);
    assert!(!out.status.success());
    fs::remove_file(env.repo.join(JAVA2)).unwrap();
    let l = lines(&env.status(&[]));
    assert_eq!(l[0], "graphe pas à jour");
    assert!(l.contains(&format!("supprimé\t{JAVA2}")), "{l:?}");
    assert!(!l.contains(&format!("en attente\t{JAVA2}")), "{l:?}");
    assert_eq!(l.len(), 9, "{l:?}");
}

#[test]
fn req012_ac4_no_graph_invites_to_build_with_nonzero_exit() {
    let env = Env::new();
    let out = env.status(&[]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("rkg build"),
        "{}",
        text(&out.stderr)
    );
    assert!(text(&out.stderr).starts_with("erreur:"));
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req012_ac5_status_makes_no_llm_call_and_needs_no_config() {
    let env = Env::built();
    let before = fs::read_to_string(&env.log).unwrap();
    fs::write(env.repo.join("README.md"), "# modifié\n").unwrap();
    let mut c = env.command(&["status"], &[("RKG_CONFIG", "/nonexistent/c.toml")]);
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(fs::read_to_string(&env.log).unwrap(), before);
}

#[test]
fn req012_json_output_lists_state_and_files() {
    let env = Env::built();
    fs::write(env.repo.join("README.md"), "# modifié\n").unwrap();
    fs::remove_file(env.repo.join(JAVA2)).unwrap();
    let out = env.status(&["--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["a_jour"], false);
    let files = v["fichiers"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert!(files
        .iter()
        .any(|f| f["chemin"] == "README.md" && f["état"] == "modifié"));
    assert!(files
        .iter()
        .any(|f| f["chemin"] == JAVA2 && f["état"] == "supprimé"));

    let env = Env::built();
    let v: serde_json::Value = serde_json::from_slice(&env.status(&["--json"]).stdout).unwrap();
    assert_eq!(v["a_jour"], true);
    assert!(v["fichiers"].as_array().unwrap().is_empty());
}

fn spawn_slow_build(env: &Env) -> Child {
    let child = env
        .command(&["build"], &[("RKG_MOCK_DELAY_MS", "400")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while fs::read_to_string(&env.log).unwrap_or_default().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(20), "build trop lent");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

#[test]
fn req010_ac3_status_after_killed_build_lists_unprocessed_as_pending() {
    let env = Env::new();
    let mut child = spawn_slow_build(&env);
    child.kill().unwrap();
    child.wait().unwrap();
    let out = env.status(&[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let l = lines(&out);
    assert_eq!(l[0], "graphe pas à jour");
    assert!(l.len() > 1);
    assert!(
        l[1..].iter().all(|x| x.starts_with("en attente\t")),
        "{l:?}"
    );
}

#[test]
fn req020_ac2_status_answers_during_running_build() {
    let env = Env::new();
    let mut child = spawn_slow_build(&env);
    let t = Instant::now();
    let out = env.status(&[]);
    let elapsed = t.elapsed();
    assert!(child.try_wait().unwrap().is_none(), "build déjà terminé");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        elapsed < Duration::from_secs(3),
        "status bloqué : {elapsed:?}"
    );
    let l = lines(&out);
    assert_eq!(l[0], "graphe pas à jour");
    assert!(
        l[1..].iter().any(|x| x.starts_with("en attente\t")),
        "{l:?}"
    );
}

#[test]
fn req001_ac4_status_on_indexed_repo_calls_no_llm() {
    let env = Env::built();
    let before = fs::read_to_string(&env.log).unwrap();
    let out = env.status(&[]);
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(&env.log).unwrap(), before);
}
