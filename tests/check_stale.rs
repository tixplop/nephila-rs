//! Option `--check-stale` des requêtes (REQ-017) : sans l'option, aucun parcours du dépôt ni
//! avertissement ; avec, comparaison des tailles et dates avec la table `files`.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const SERVICE: &str = "core/OrderService.java";
const REPO_JAVA: &str = "core/OrderRepository.java";
const API: &str = "api/ApiController.java";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

fn set_mtime(p: &std::path::Path, t: SystemTime) {
    let f = fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(t).unwrap();
}

fn first_line(b: &[u8]) -> String {
    text(b).lines().next().unwrap_or("").to_string()
}

fn warn_count(b: &[u8]) -> usize {
    text(b).lines().filter(|l| l.contains("pas à jour")).count()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    fn new() -> Env {
        Env::create(false)
    }

    /// Fixture dont les fichiers sont reculés de 60 s avant le `build` : l'indexation tombe à
    /// plus de 2 s de leur date (hors fenêtre « racy »).
    fn new_aged() -> Env {
        Env::create(true)
    }

    fn create(aged: bool) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            mock: tmp.path().join("mock"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
        };
        fs::create_dir_all(&env.mock).unwrap();
        fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        env.file(
            SERVICE,
            "package com.acme.core;\n\npublic class OrderService {\n    OrderRepository repo;\n}\n",
        );
        env.file(
            REPO_JAVA,
            "package com.acme.core;\n\npublic interface OrderRepository {\n}\n",
        );
        env.file(
            API,
            "package com.acme.api;\n\nimport com.acme.core.OrderRepository;\n\npublic class ApiController {\n    OrderRepository repo;\n}\n",
        );
        fs::write(
            env.mock.join("_ask.json"),
            r#"{"found":true,"answer":"ok","nodes":["OrderService"]}"#,
        )
        .unwrap();
        if aged {
            let old = SystemTime::now() - Duration::from_secs(60);
            for p in [SERVICE, REPO_JAVA, API] {
                set_mtime(&env.repo.join(p), old);
            }
        }
        let out = env.cmd(&["build", "--no-llm"]).output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        env
    }

    fn file(&self, path: &str, content: &str) {
        let p = self.repo.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env_remove("NEPHILA_MOCK_FAIL_AFTER");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Les quatre requêtes, avec ou sans `--check-stale` et `--json`.
    fn queries(&self, check: bool, json: bool) -> Vec<Output> {
        let sets: [&[&str]; 4] = [
            &["query", "search", "OrderService"],
            &["query", "neighbors", "OrderService"],
            &["query", "path", "ApiController", "OrderRepository"],
            &["query", "ask", "Que fait OrderService ?"],
        ];
        sets.iter()
            .map(|s| {
                let mut a: Vec<&str> = s.to_vec();
                if json {
                    a.push("--json");
                }
                if check {
                    a.push("--check-stale");
                }
                self.run(&a)
            })
            .collect()
    }

    fn modify_three(&self) {
        for p in [SERVICE, REPO_JAVA, API] {
            let mut c = fs::read_to_string(self.repo.join(p)).unwrap();
            c.push_str("// modifié\n");
            fs::write(self.repo.join(p), c).unwrap();
        }
    }
}

#[test]
fn req017_ac1_check_stale_warns_on_all_queries() {
    let env = Env::new();
    env.modify_three();
    for json in [false, true] {
        for (i, o) in env.queries(true, json).iter().enumerate() {
            assert!(o.status.success(), "{i} {}", text(&o.stderr));
            assert!(
                text(&o.stderr).contains("graphe pas à jour : 3 fichiers modifiés"),
                "{i} {}",
                text(&o.stderr)
            );
        }
    }
}

#[test]
fn req017_ac2_up_to_date_graph_has_no_warning() {
    let env = Env::new();
    for check in [false, true] {
        for o in env.queries(check, false) {
            assert!(o.status.success(), "{}", text(&o.stderr));
            assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
        }
    }
}

#[test]
fn req017_ac3_no_graph_asks_for_build_with_and_without_option() {
    let env = Env::new();
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    for check in [false, true] {
        for o in env.queries(check, false) {
            assert!(!o.status.success());
            assert!(o.stdout.is_empty());
            assert!(
                text(&o.stderr).contains("nephila build"),
                "{}",
                text(&o.stderr)
            );
        }
    }
}

#[test]
fn req017_ac4_default_queries_ignore_changes_in_the_repository() {
    for json in [false, true] {
        let env = Env::new();
        let before = env.queries(false, json);
        env.modify_three();
        env.file("core/New.java", "package com.acme.core;\n\nclass New {}\n");
        fs::remove_file(env.repo.join("core/OrderRepository.java")).unwrap();
        let after = env.queries(false, json);
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(b.stdout, a.stdout);
            assert_eq!(b.status.code(), a.status.code());
            assert!(
                !text(&a.stderr).contains("pas à jour"),
                "{}",
                text(&a.stderr)
            );
        }
    }
}

#[test]
fn req017_ac5_no_walk_when_everything_but_graph_is_deleted() {
    let env = Env::new();
    let sets: [&[&str]; 3] = [
        &["query", "search", "OrderService"],
        &["query", "neighbors", "OrderService"],
        &["query", "path", "ApiController", "OrderRepository"],
    ];
    let before: Vec<Output> = sets.iter().map(|s| env.run(s)).collect();
    for entry in fs::read_dir(&env.repo).unwrap() {
        let p = entry.unwrap().path();
        if p.file_name().unwrap() == ".nephila" {
            continue;
        }
        if p.is_dir() {
            fs::remove_dir_all(&p).unwrap();
        } else {
            fs::remove_file(&p).unwrap();
        }
    }
    for (s, b) in sets.iter().zip(&before) {
        let o = env.run(s);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(o.stdout, b.stdout);
        assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
        let mut a = s.to_vec();
        a.push("--check-stale");
        let o = env.run(&a);
        assert_eq!(o.stdout, b.stdout);
        assert!(
            text(&o.stderr).contains("graphe pas à jour : 3 fichiers modifiés"),
            "{}",
            text(&o.stderr)
        );
    }
}

#[test]
fn req017_ac14_non_utf8_file_outside_graph_is_not_counted_as_added() {
    let env = Env::new();
    let p = env.repo.join("core/Latin1.java");
    fs::write(&p, b"package com.acme.core;\n// caf\xe9\nclass Latin1 {}\n").unwrap();
    let q = ["query", "search", "OrderService", "--check-stale"];
    let o = env.run(&q);
    assert!(o.status.success());
    assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
    let u = env.run(&["update", "--no-llm"]);
    assert!(u.status.success(), "{}", text(&u.stderr));
    let o = env.run(&q);
    assert!(o.status.success());
    assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
    env.file(
        "core/Utf8.java",
        "package com.acme.core;\n\nclass Utf8 {}\n",
    );
    let o = env.run(&q);
    assert!(o.status.success());
    assert_eq!(
        text(&o.stderr).lines().next(),
        Some("avertissement: graphe pas à jour : 1 fichier modifié")
    );
}

#[test]
fn req017_ac6_same_size_content_with_original_date_is_not_counted() {
    let env = Env::new_aged();
    let p = env.repo.join(SERVICE);
    let orig = fs::metadata(&p).unwrap().modified().unwrap();
    let mut c = fs::read_to_string(&p).unwrap();
    c = c.replace("OrderRepository repo", "OrderRepository rep2");
    fs::write(&p, &c).unwrap();
    set_mtime(&p, orig);
    for o in env.queries(true, false) {
        assert!(o.status.success(), "{}", text(&o.stderr));
        assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
    }
}

#[test]
fn req017_ac6_date_only_change_is_counted_while_status_says_up_to_date() {
    let env = Env::new_aged();
    set_mtime(&env.repo.join(SERVICE), SystemTime::now());
    let o = env.run(&["query", "search", "OrderService", "--check-stale"]);
    assert!(o.status.success());
    assert_eq!(
        first_line(&o.stderr),
        "avertissement: graphe pas à jour : 1 fichier modifié"
    );
    let s = env.run(&["status"]);
    assert!(s.status.success());
    assert_eq!(first_line(&s.stdout), "graphe à jour");
}

#[test]
fn req017_ac9_first_stderr_line_is_the_only_warning() {
    let env = Env::new_aged();
    env.modify_three();
    let o = env.run(&["query", "search", "OrderService", "--check-stale"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        first_line(&o.stderr),
        "avertissement: graphe pas à jour : 3 fichiers modifiés"
    );
    assert_eq!(warn_count(&o.stderr), 1);

    let j = env.run(&["query", "search", "OrderService", "--json", "--check-stale"]);
    assert_eq!(
        first_line(&j.stderr),
        "avertissement: graphe pas à jour : 3 fichiers modifiés"
    );
    let v: serde_json::Value = serde_json::from_slice(&j.stdout).unwrap();
    assert!(!v.to_string().contains("pas à jour"));
    assert!(!text(&j.stdout).contains("pas à jour"));

    let n = env.run(&["query", "search", "zzzinexistant", "--check-stale"]);
    assert!(!n.status.success());
    let err = text(&n.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(
        lines[0],
        "avertissement: graphe pas à jour : 3 fichiers modifiés"
    );
    assert!(
        lines[1..].iter().any(|l| l.contains("aucun résultat")),
        "{err}"
    );

    let plain = env.run(&["query", "ask", "Que fait OrderService ?"]);
    let ask = env.run(&["query", "ask", "Que fait OrderService ?", "--check-stale"]);
    assert!(ask.status.success(), "{}", text(&ask.stderr));
    assert_eq!(ask.stdout, plain.stdout);
    assert_eq!(
        first_line(&ask.stderr),
        "avertissement: graphe pas à jour : 3 fichiers modifiés"
    );
}

#[test]
fn req017_ac9_singular_for_one_file() {
    let env = Env::new_aged();
    let mut c = fs::read_to_string(env.repo.join(API)).unwrap();
    c.push_str("// x\n");
    fs::write(env.repo.join(API), c).unwrap();
    let o = env.run(&["query", "search", "OrderService", "--check-stale"]);
    assert_eq!(
        first_line(&o.stderr),
        "avertissement: graphe pas à jour : 1 fichier modifié"
    );
}

#[test]
fn req017_ac10_up_to_date_outputs_are_byte_identical_with_option() {
    let env = Env::new_aged();
    let sets: [&[&str]; 3] = [
        &["query", "search", "OrderService"],
        &["query", "neighbors", "OrderService"],
        &["query", "path", "ApiController", "OrderRepository"],
    ];
    for s in sets {
        let a = env.run(s);
        let mut w = s.to_vec();
        w.push("--check-stale");
        let b = env.run(&w);
        assert_eq!(a.stdout, b.stdout);
        assert_eq!(a.stderr, b.stderr);
        assert_eq!(a.status.code(), b.status.code());
        assert!(b.stderr.is_empty(), "{}", text(&b.stderr));
    }
}

#[test]
fn req017_ac11_update_records_new_date_and_clears_warning() {
    let env = Env::new_aged();
    set_mtime(&env.repo.join(SERVICE), SystemTime::now());
    let q = ["query", "search", "OrderService", "--check-stale"];
    let o = env.run(&q);
    assert_eq!(
        first_line(&o.stderr),
        "avertissement: graphe pas à jour : 1 fichier modifié"
    );
    let u = env.run(&["update", "--no-llm"]);
    assert_eq!(u.status.code(), Some(0), "{}", text(&u.stderr));
    assert_eq!(text(&u.stdout).trim_end(), "graphe à jour");
    let o = env.run(&q);
    assert!(o.status.success());
    assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
}

#[test]
fn req017_ac12_option_is_refused_outside_query() {
    let env = Env::new();
    for cmd in ["status", "summary"] {
        let o = env.run(&[cmd, "--check-stale"]);
        assert_eq!(o.status.code(), Some(2), "{cmd}");
        assert!(o.stdout.is_empty(), "{cmd}");
        assert!(text(&o.stderr).contains("--check-stale"), "{cmd}");
    }
}
