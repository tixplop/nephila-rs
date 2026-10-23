//! Option `--check-stale` des requêtes (REQ-017) : sans l'option, aucun parcours du dépôt ni
//! avertissement ; avec, comparaison des tailles et dates avec la table `files`.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const SERVICE: &str = "core/OrderService.java";
const REPO_JAVA: &str = "core/OrderRepository.java";
const API: &str = "api/ApiController.java";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    fn new() -> Env {
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
