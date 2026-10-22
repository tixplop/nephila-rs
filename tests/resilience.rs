//! Reprise après échec de l'enrichissement et verrou d'écriture (REQ-010, REQ-020).

use assert_cmd::cargo::cargo_bin;
use nephila::model::FileState;
use nephila::store::Store;
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
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nconcurrency = 1\n",
        )
        .unwrap();
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
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .arg(sub)
            .env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_TIMEOUT_FILES")
            .env_remove("NEPHILA_MOCK_KEY_REVOKED_AFTER")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
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
            .command("build", &[("NEPHILA_MOCK_DELAY_MS", "400")])
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

fn paths_in_graph(env: &Env, state: FileState) -> Vec<String> {
    let mut v: Vec<String> = Store::open_readonly(&env.repo)
        .unwrap()
        .files()
        .unwrap()
        .into_iter()
        .filter(|f| f.state == state)
        .map(|f| f.path)
        .collect();
    v.sort();
    v
}

fn to_enrich(env: &Env) -> Vec<String> {
    let mut v: Vec<String> = Store::open_readonly(&env.repo)
        .unwrap()
        .files_to_enrich()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    v.sort();
    v
}

fn paths_pending(files: &[nephila::model::FileRecord]) -> Vec<String> {
    files
        .iter()
        .filter(|f| f.state == FileState::Pending)
        .map(|f| f.path.clone())
        .collect()
}

fn logged_paths(env: &Env) -> Vec<String> {
    let mut v: Vec<String> = fs::read_to_string(&env.log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('\t').next().unwrap().to_string())
        .collect();
    v.sort();
    v.dedup();
    v
}

fn call_count(env: &Env) -> usize {
    fs::read_to_string(&env.log)
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn req010_ac1_failure_after_two_files_keeps_every_structure_and_leaves_rest_to_enrich() {
    let env = Env::new();
    let out = env.run("build", &[("NEPHILA_MOCK_FAIL_AFTER", "2")]);
    assert!(!out.status.success());
    let store = Store::open_readonly(&env.repo).unwrap();
    let files = store.files().unwrap();
    assert!(files.len() >= 8);
    assert!(files.iter().all(|f| f.state == FileState::Indexed));
    assert_eq!(to_enrich(&env).len(), files.len() - 2);
    // Tous les fichiers ont leurs nœuds dans le graphe.
    let nodes = store.nodes().unwrap();
    for f in files.iter().filter(|f| f.path.ends_with(".java")) {
        assert!(
            nodes.iter().any(|n| n.file.as_deref() == Some(&f.path)),
            "aucun nœud pour {}",
            f.path
        );
    }
}

#[test]
fn req010_ac1_three_consecutive_network_errors_stop_the_sending() {
    let env = Env::new();
    let out = env.run("build", &[("NEPHILA_MOCK_FAIL_AFTER", "2")]);
    assert!(!out.status.success());
    // 2 réussites + 3 échecs réseau consécutifs, puis arrêt (concurrency = 1).
    assert_eq!(call_count(&env), 5);
    assert!(
        stderr(&out).contains("panne réseau répétée"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn req010_ac1_with_concurrency_at_most_five_plus_n_minus_one_calls() {
    let env = Env::new();
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nconcurrency = 3\n",
    )
    .unwrap();
    let out = env.run("build", &[("NEPHILA_MOCK_FAIL_AFTER", "2")]);
    assert!(!out.status.success());
    let calls = call_count(&env);
    assert!((5..=7).contains(&calls), "{calls} appels");
}

#[test]
fn req010_ac4_timeout_file_does_not_stop_the_others() {
    let env = Env::new();
    let bad = "README.md";
    let out = env.run("build", &[("NEPHILA_MOCK_TIMEOUT_FILES", bad)]);
    assert!(!out.status.success());
    let all = Store::open_readonly(&env.repo).unwrap().files().unwrap();
    assert!(paths_in_graph(&env, FileState::Pending).is_empty());
    assert_eq!(to_enrich(&env), vec![bad]);
    assert_eq!(logged_paths(&env).len(), all.len());
    let err = stderr(&out);
    assert!(err.contains("avertissement:") && err.contains(bad), "{err}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 fichiers à enrichir"), "{stdout}");
}

#[test]
fn req010_ac4_nonconforming_fixture_does_not_stop_the_others() {
    let env = Env::new();
    let bad = "README.md";
    fs::write(env.mock.join("README.md.json"), "pas du json").unwrap();
    let out = env.run("build", &[]);
    assert!(!out.status.success());
    let all = Store::open_readonly(&env.repo).unwrap().files().unwrap();
    assert_eq!(to_enrich(&env), vec![bad]);
    assert_eq!(logged_paths(&env).len(), all.len());
}

#[test]
fn req010_ac4_update_behaves_the_same() {
    let env = Env::new();
    let ok = env.run("build", &[]);
    assert!(ok.status.success(), "{}", stderr(&ok));
    // Trois fichiers modifiés ; celui en échec est traité en premier (concurrency = 1).
    let indexed = paths_in_graph(&env, FileState::Indexed);
    let changed: Vec<String> = indexed.iter().take(3).cloned().collect();
    assert_eq!(changed.len(), 3);
    let bad = changed[0].as_str();
    for f in &changed {
        let mut t = fs::read_to_string(env.repo.join(f)).unwrap();
        t.push_str("\nmodif\n");
        fs::write(env.repo.join(f), t).unwrap();
    }
    fs::remove_file(&env.log).ok();
    let out = env.run("update", &[("NEPHILA_MOCK_TIMEOUT_FILES", bad)]);
    assert!(!out.status.success());
    assert_eq!(logged_paths(&env), changed);
    assert!(paths_in_graph(&env, FileState::Pending).is_empty());
    assert_eq!(to_enrich(&env), vec![bad]);
    let err = stderr(&out);
    assert!(err.contains("avertissement:") && err.contains(bad), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 fichiers à enrichir"));
}

#[test]
fn req010_ac5_four_timeouts_do_not_stop_the_sending() {
    let env = Env::new();
    let all: Vec<String> = {
        // Premier build réussi pour connaître les fichiers, puis reconstruction avec échecs.
        let ok = env.run("build", &[]);
        assert!(ok.status.success());
        let mut v = paths_in_graph(&env, FileState::Indexed);
        v.sort();
        v
    };
    assert!(all.len() >= 8);
    let bad: Vec<String> = all.iter().take(4).cloned().collect();
    fs::remove_file(&env.log).unwrap();
    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    let out = env.run("build", &[("NEPHILA_MOCK_TIMEOUT_FILES", &bad.join(","))]);
    assert!(!out.status.success());
    assert_eq!(logged_paths(&env), all);
    assert_eq!(to_enrich(&env), bad);
    assert_eq!(paths_in_graph(&env, FileState::Indexed).len(), all.len());
}

#[test]
fn req010_ac6_revoked_key_stops_at_first_refusal() {
    let env = Env::new();
    let out = env.run("build", &[("NEPHILA_MOCK_KEY_REVOKED_AFTER", "2")]);
    assert!(!out.status.success());
    assert_eq!(call_count(&env), 3);
    let err = stderr(&out);
    assert!(
        err.contains("clé d'API refusée par le fournisseur (LLM simulé)"),
        "{err}"
    );
    assert!(!err.contains("envoi interrompu"), "{err}");
    assert!(!err.contains("panne réseau"), "{err}");
    assert_eq!(paths_in_graph(&env, FileState::Indexed).len(), 10);
    assert_eq!(to_enrich(&env).len(), 8);
}

#[test]
fn req010_ac7_update_after_ac4_sends_only_the_file_to_enrich() {
    let env = Env::new();
    let bad = "README.md";
    let out = env.run("build", &[("NEPHILA_MOCK_TIMEOUT_FILES", bad)]);
    assert!(!out.status.success());
    fs::remove_file(&env.log).unwrap();
    let out = env.run("update", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(logged_paths(&env), vec![bad]);
    assert!(to_enrich(&env).is_empty());
}

#[test]
fn req010_ac3_build_killed_during_enrichment_leaves_readable_graph_with_files_to_enrich() {
    let env = Env::new();
    let mut child = env.spawn_slow_build();
    child.kill().unwrap();
    child.wait().unwrap();
    let store = Store::open_readonly(&env.repo).expect("graphe illisible après kill -9");
    let files = store.files().unwrap();
    assert!(!files.is_empty());
    assert!(files.iter().all(|f| f.state == FileState::Indexed));
    let pending = to_enrich(&env);
    assert!(!pending.is_empty());
    let nodes = store.nodes().unwrap();
    store.edges().unwrap();
    for f in files.iter().filter(|f| f.path.ends_with(".java")) {
        assert!(
            nodes.iter().any(|n| n.file.as_deref() == Some(&f.path)),
            "aucun nœud pour {}",
            f.path
        );
    }
    let status = env.run("status", &[]);
    assert!(status.status.success(), "{}", stderr(&status));
    let text = String::from_utf8_lossy(&status.stdout).into_owned();
    let expected = format!("{} fichiers à enrichir", pending.len());
    assert!(
        text.lines().any(|l| l.contains(&expected)),
        "{expected} absent de :\n{text}"
    );
    for f in files.iter().filter(|f| f.path.ends_with(".java")) {
        let stem = Path::new(&f.path).file_stem().unwrap().to_string_lossy();
        let search = env
            .command("query", &[])
            .args(["search", &stem])
            .output()
            .unwrap();
        assert!(search.status.success(), "{}", stderr(&search));
        assert!(
            String::from_utf8_lossy(&search.stdout).contains(stem.as_ref()),
            "{stem} introuvable par search"
        );
    }
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
    fs::create_dir_all(env.repo.join(".nephila")).unwrap();
    fs::write(env.repo.join(".nephila/lock"), "12345\n").unwrap();
    let out = env.run("build", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn req020_lock_is_exclusive_and_released_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let first = nephila::index::lock::RepoLock::acquire(dir.path()).unwrap();
    let err = nephila::index::lock::RepoLock::acquire(dir.path()).unwrap_err();
    assert!(err.to_string().contains("indexation déjà en cours"));
    drop(first);
    nephila::index::lock::RepoLock::acquire(dir.path()).unwrap();
}

#[test]
fn req010_ac3_state_left_by_a_kill_during_structure_lists_pending_files_and_update_completes() {
    let env = Env::new();
    let out = env.run("build", &[("NEPHILA_CONFIG", "/nonexistent/absent.toml")]);
    assert!(out.status.success(), "{}", stderr(&out));
    // État qu'un `kill -9` pendant la phase structure laisse : le graphe est réinitialisé avec
    // tous les fichiers « en attente » (une transaction), puis une partie seulement est analysée
    // (chaque fichier est une transaction).
    let pending: Vec<String> = {
        let mut store = Store::open_existing(&env.repo).unwrap();
        let files = store.files().unwrap();
        let nodes = store.nodes().unwrap();
        let refs = store.refs().unwrap();
        let declared = store.declarations().unwrap();
        let java: Vec<String> = {
            let mut v: Vec<String> = files
                .iter()
                .filter(|f| f.path.ends_with(".java"))
                .map(|f| f.path.clone())
                .collect();
            v.sort();
            v
        };
        assert!(java.len() >= 4);
        let pending: Vec<String> = java.iter().take(2).cloned().collect();
        store.reset_with_pending_files(&files).unwrap();
        for f in files.iter().filter(|f| !pending.contains(&f.path)) {
            let ids: Vec<&String> = declared
                .iter()
                .filter(|(p, _)| *p == f.path)
                .map(|(_, id)| id)
                .collect();
            let ns: Vec<_> = nodes
                .iter()
                .filter(|n| ids.contains(&&n.id))
                .cloned()
                .map(|mut n| {
                    n.file = n.file.map(|_| f.path.clone());
                    n
                })
                .collect();
            let rs: Vec<_> = refs
                .iter()
                .filter(|r| ids.contains(&&r.source))
                .cloned()
                .collect();
            store.replace_structure(&f.path, &ns, &rs).unwrap();
        }
        pending
    };
    assert_eq!(paths_in_graph(&env, FileState::Pending), pending);
    let status = env.run("status", &[("NEPHILA_CONFIG", "/nonexistent/absent.toml")]);
    assert!(status.status.success(), "{}", stderr(&status));
    let text = String::from_utf8_lossy(&status.stdout).into_owned();
    for p in &pending {
        assert!(
            text.lines()
                .any(|l| l.contains(p) && l.contains("en attente")),
            "{text}"
        );
    }
    let search = env
        .command("query", &[("NEPHILA_CONFIG", "/nonexistent/absent.toml")])
        .args(["search", "OrderService"])
        .output()
        .unwrap();
    assert!(search.status.success(), "{}", stderr(&search));
    assert!(String::from_utf8_lossy(&search.stdout).contains("OrderService"));
    let upd = env.run("update", &[("NEPHILA_CONFIG", "/nonexistent/absent.toml")]);
    assert!(upd.status.success(), "{}", stderr(&upd));
    assert!(paths_in_graph(&env, FileState::Pending).is_empty());
}

#[test]
fn req010_ac3_build_killed_during_structure_phase_leaves_readable_graph() {
    // Dépôt assez gros pour que la phase structure dure ; tué dès que le graphe est ouvrable.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let dir = repo.join("src/main/java/com/acme");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..150 {
        let mut s = format!("package com.acme;\npublic class C{i} {{\n");
        for m in 0..100 {
            s.push_str(&format!("    void m{m}() {{}}\n"));
        }
        s.push_str("}\n");
        fs::write(dir.join(format!("C{i}.java")), s).unwrap();
    }
    let absent = tmp.path().join("absent.toml");
    let nephila = |args: &[&str]| {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&repo)
            .args(args)
            .env("NEPHILA_CONFIG", &absent);
        c
    };
    let mut child = nephila(&["build"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Attend qu'au moins un fichier soit « en attente » : la phase structure est en cours.
    let start = Instant::now();
    loop {
        if let Ok(store) = Store::open_readonly(&repo) {
            if let Ok(files) = store.files() {
                if files.iter().any(|f| f.state == FileState::Pending) {
                    break;
                }
            }
        }
        assert!(start.elapsed() < Duration::from_secs(30), "build trop lent");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "build déjà terminé : le kill ne tombe pas pendant la phase structure"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    let store = Store::open_readonly(&repo).expect("graphe illisible après kill -9");
    let files = store.files().unwrap();
    assert_eq!(files.len(), 150);
    store.nodes().unwrap();
    store.edges().unwrap();
    let pending = paths_pending(&files);
    assert!(
        !pending.is_empty(),
        "plus aucun fichier en attente au moment du kill"
    );
    let status = nephila(&["status"]).output().unwrap();
    assert!(status.status.success(), "{}", stderr(&status));
    let text = String::from_utf8_lossy(&status.stdout).into_owned();
    for p in &pending {
        assert!(
            text.lines()
                .any(|l| l.contains(p.as_str()) && l.contains("en attente")),
            "fichier {p} non signalé « en attente » :\n{text}"
        );
    }
    let upd = nephila(&["update"]).output().unwrap();
    assert!(upd.status.success(), "{}", stderr(&upd));
    let store = Store::open_readonly(&repo).unwrap();
    assert!(store
        .files()
        .unwrap()
        .iter()
        .all(|f| f.state == FileState::Indexed));
    assert_eq!(
        store
            .nodes()
            .unwrap()
            .iter()
            .filter(|n| n.id.starts_with("type:com.acme.C"))
            .count(),
        150
    );
}

#[test]
fn req024_ac9_structure_is_searchable_while_enrichment_is_running() {
    let env = Env::new();
    let mut child = env.spawn_slow_build();
    // L'enrichissement a commencé (premier appel journalisé) et dure encore.
    assert!(child.try_wait().unwrap().is_none(), "build déjà terminé");
    let out = env
        .command("query", &[])
        .args(["search", "OrderService"])
        .output()
        .unwrap();
    let store = Store::open_readonly(&env.repo).unwrap();
    let files = store.files().unwrap();
    let nodes = store.nodes().unwrap();
    let java: Vec<_> = files.iter().filter(|f| f.path.ends_with(".java")).collect();
    assert!(!java.is_empty());
    for f in java {
        assert!(
            nodes.iter().any(|n| n.file.as_deref() == Some(&f.path)),
            "aucun nœud pour {}",
            f.path
        );
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("OrderService"));
}
