//! Commandes `summary` et `export` (REQ-019, REQ-018, AC-001-4).

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven3");
        let repo = tmp.path().join("repo");
        copy_dir(&fx.join("repo"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("mock"), &mock);
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

    fn built() -> Env {
        let env = Env::new();
        env.build();
        env
    }

    fn build(&self) {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .arg("build")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    /// Commande de lecture : aucune configuration LLM, aucune fixture.
    fn read(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", self.repo.join("absent.toml"))
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    /// Comme `read`, depuis le dossier courant `cwd`.
    fn read_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .current_dir(cwd)
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", self.repo.join("absent.toml"))
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.read(args);
        assert!(out.status.success(), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("JSON valide sur stdout")
    }

    fn computed_at(&self) -> i64 {
        let c = rusqlite::Connection::open(self.repo.join(".nephila/graph.db")).unwrap();
        c.query_row("SELECT computed_at FROM summary WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn req019_ac1_summary_lists_modules_dependencies_nodes_and_themes() {
    let env = Env::built();
    let s = env.json(&["summary", "--json"]);
    let modules = s["modules"].as_array().unwrap();
    let dep = |name: &str| -> Vec<String> {
        let m = modules
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("module {name} absent : {s}"));
        strings(&m["depends_on"])
    };
    assert_eq!(modules.len(), 3, "{s}");
    assert_eq!(dep("api"), vec!["module:com.acme:core"]);
    assert_eq!(dep("store"), vec!["module:com.acme:core"]);
    assert_eq!(dep("core"), vec!["dependency:org.slf4j:slf4j-api"]);

    // Les 10 nœuds de plus haut degré, vérifiés contre l'export.
    let export = env.json(&["export", "--output", "-"]);
    let mut degree = std::collections::HashMap::<String, u64>::new();
    for e in export["edges"].as_array().unwrap() {
        *degree
            .entry(e["source"].as_str().unwrap().into())
            .or_default() += 1;
        *degree
            .entry(e["target"].as_str().unwrap().into())
            .or_default() += 1;
    }
    let mut all: Vec<u64> = degree.values().copied().collect();
    all.sort_by(|a, b| b.cmp(a));
    let top = s["top_nodes"].as_array().unwrap();
    assert_eq!(top.len(), 10, "{s}");
    let got: Vec<u64> = top.iter().map(|n| n["degree"].as_u64().unwrap()).collect();
    assert_eq!(got, all[..10].to_vec());
    for n in top {
        assert_eq!(
            degree[n["id"].as_str().unwrap()],
            n["degree"].as_u64().unwrap()
        );
    }

    // Thèmes : concepts les plus mentionnés.
    let themes = s["themes"].as_array().unwrap();
    assert_eq!(themes[0]["concept"], "concept:idempotence", "{s}");
    assert_eq!(themes[0]["mentions"], 2);
    assert_eq!(themes[1]["concept"], "concept:persistance");
    assert_eq!(themes[1]["mentions"], 1);
}

#[test]
fn req019_ac1_summary_text_output() {
    let env = Env::built();
    let out = env.read(&["summary"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = text(&out.stdout);
    for needle in [
        "api",
        "store",
        "core",
        "slf4j-api",
        "idempotence",
        "persistance",
    ] {
        assert!(t.contains(needle), "{needle} absent de :\n{t}");
    }
    // AC-019-6 : noms qualifiés, lignes exactes.
    for line in [
        "  com.acme:api -> module:com.acme:core",
        "  com.acme:core -> dependency:org.slf4j:slf4j-api",
        "  com.acme:store -> module:com.acme:core",
    ] {
        assert!(t.lines().any(|l| l == line), "{line:?} absent de :\n{t}");
    }
}

#[test]
fn req019_ac2_build_stores_timestamped_summary_and_rebuild_refreshes_it() {
    let env = Env::built();
    let first = env.computed_at();
    assert!(first > 0);
    let j = env.json(&["summary", "--json"]);
    assert_eq!(j["computed_at"].as_i64().unwrap(), first);
    std::thread::sleep(std::time::Duration::from_millis(20));
    env.build();
    assert!(env.computed_at() > first);
}

#[test]
fn req019_ac4_summary_without_graph_invites_to_build() {
    let env = Env::new();
    let out = env.read(&["summary"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("nephila build"),
        "{}",
        text(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req018_ac1_export_contains_all_nodes_and_edges() {
    let env = Env::built();
    let v = env.json(&["export", "--output", "-"]);
    assert_eq!(v["version"], 1);
    let nodes = v["nodes"].as_array().unwrap();
    let ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    for id in [
        "module:com.acme:api",
        "module:com.acme:store",
        "type:com.acme.core.OrderService",
        "method:com.acme.core.OrderService.place",
        "concept:idempotence",
        "dependency:org.slf4j:slf4j-api",
    ] {
        assert!(ids.contains(&id), "{id} absent");
    }
    let svc = nodes
        .iter()
        .find(|n| n["id"] == "type:com.acme.core.OrderService")
        .unwrap();
    assert_eq!(svc["kind"], "type");
    assert_eq!(svc["subkind"], "class");
    assert_eq!(svc["short"], "OrderService");
    assert_eq!(svc["qualified"], "com.acme.core.OrderService");
    assert!(svc["file"].as_str().unwrap().ends_with("OrderService.java"));
    let edges = v["edges"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["source"] == "module:com.acme:api"
        && e["target"] == "module:com.acme:core"
        && e["kind"] == "depends_on"));
    assert!(edges.iter().any(|e| e["kind"] == "mentions"));
    assert!(v["unresolved"].is_array());
}

#[test]
fn req018_ac2_output_file_has_same_content_and_stdout_is_its_path() {
    let env = Env::built();
    let expected = env.json(&["export", "--output", "-"]);
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export", "--output", "f.json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "f.json\n");
    let got: Value = serde_json::from_slice(&fs::read(cwd.path().join("f.json")).unwrap()).unwrap();
    assert_eq!(got, expected);
}

#[test]
fn req018_ac1_default_output_is_slug_json_in_current_dir() {
    let env = Env::built();
    let expected = env.json(&["export", "--output", "-"]);
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "repo.json\n");
    let got: Value =
        serde_json::from_slice(&fs::read(cwd.path().join("repo.json")).unwrap()).unwrap();
    assert_eq!(got, expected);
}

#[test]
fn req018_ac5_slug_file_from_another_dir_and_replaced_after_update() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("Repo_Knowledge.Graph");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("Main.java"),
        "package p;\npublic class A { void m() {} }\n",
    )
    .unwrap();
    let nephila = |cwd: &Path, args: &[&str]| {
        Command::new(cargo_bin("nephila"))
            .current_dir(cwd)
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("NEPHILA_CONFIG", tmp.path().join("absent.toml"))
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    };
    let out = nephila(tmp.path(), &["build", "--no-llm"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let before: Vec<_> = fs::read_dir(&repo)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let work = tmp.path().join("out");
    fs::create_dir_all(&work).unwrap();
    let out = nephila(&work, &["export"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "repo-knowledge-graph.json\n");
    let file = work.join("repo-knowledge-graph.json");
    let stdout = nephila(&work, &["export", "--output", "-"]);
    assert_eq!(fs::read(&file).unwrap(), stdout.stdout);
    let after: Vec<_> = fs::read_dir(&repo)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
    // Après un update, le fichier est remplacé.
    fs::write(repo.join("B.java"), "package p;\npublic class Bee {}\n").unwrap();
    let out = nephila(&work, &["update", "--no-llm"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!fs::read_to_string(&file).unwrap().contains("Bee"));
    let out = nephila(&work, &["export"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(fs::read_to_string(&file).unwrap().contains("Bee"));
}

#[test]
fn req018_ac6_default_file_without_dash_c_and_from_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("monrepo");
    fs::create_dir_all(repo.join("sub")).unwrap();
    fs::write(repo.join("A.java"), "package p;\npublic class A {}\n").unwrap();
    let run = |cwd: &Path, args: &[&str]| {
        let out = Command::new(cargo_bin("nephila"))
            .current_dir(cwd)
            .args(args)
            .env("NEPHILA_CONFIG", tmp.path().join("absent.toml"))
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?} {}", text(&out.stderr));
        out
    };
    run(&repo, &["build", "--no-llm"]);
    let out = run(&repo, &["export"]);
    assert_eq!(text(&out.stdout), "monrepo.json\n");
    assert!(repo.join("monrepo.json").is_file());
    let out = run(&repo.join("sub"), &["-C", "..", "export"]);
    assert_eq!(text(&out.stdout), "monrepo.json\n");
    assert!(repo.join("sub/monrepo.json").is_file());
    let out = run(&repo, &["status"]);
    assert!(
        text(&out.stdout).starts_with("graphe à jour"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn req018_ac2_unwritable_output_is_an_error() {
    let env = Env::built();
    let target = env.repo.join("absent/dir/out.json");
    let out = env.read(&["export", "--output", target.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).starts_with("erreur:"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn req018_ac3_unresolved_references_are_exported() {
    let env = Env::new();
    let f = env
        .repo
        .join("core/src/main/java/com/acme/core/OrderService.java");
    let src = fs::read_to_string(&f).unwrap();
    fs::write(
        &f,
        src.replace(
            "repo.save(new Order());",
            "repo.save(new Order());\n        Ghost.run();",
        ),
    )
    .unwrap();
    env.build();
    let v = env.json(&["export", "--output", "-"]);
    let un = v["unresolved"].as_array().unwrap();
    assert_eq!(un.len(), 1, "{un:?}");
    assert_eq!(un[0]["source"], "method:com.acme.core.OrderService.place");
    assert_eq!(un[0]["kind"], "calls");
    assert_eq!(un[0]["target"], "Ghost.run");
}

#[test]
fn req018_ac4_export_without_graph_invites_to_build() {
    let env = Env::new();
    let cwd = tempfile::tempdir().unwrap();
    for args in [
        vec!["export"],
        vec!["export", "--output", "-"],
        vec!["export", "--output", "out.json"],
    ] {
        let out = env.read_in(cwd.path(), &args);
        assert!(!out.status.success());
        assert!(
            text(&out.stderr).contains("nephila build"),
            "{}",
            text(&out.stderr)
        );
        assert!(out.stdout.is_empty());
    }
    assert_eq!(fs::read_dir(cwd.path()).unwrap().count(), 0);
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req001_ac4_summary_and_export_make_no_llm_call() {
    let env = Env::built();
    let before = fs::read_to_string(&env.log).unwrap();
    assert!(!before.is_empty());
    assert!(env.read(&["summary"]).status.success());
    assert!(env.read(&["summary", "--json"]).status.success());
    assert!(env.read(&["export", "--output", "-"]).status.success());
    // Même avec un journal actif et une configuration invalide, aucun appel n'est fait.
    let out = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&env.repo)
        .arg("summary")
        .env("NEPHILA_CONFIG", env.repo.join("absent.toml"))
        .env("NEPHILA_MOCK_DIR", &env.mock)
        .env("NEPHILA_MOCK_LOG", &env.log)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(&env.log).unwrap(), before);
}

#[test]
fn build_on_schema_v1_database_recreates_it() {
    let env = Env::new();
    let dir = env.repo.join(".nephila");
    fs::create_dir_all(&dir).unwrap();
    let c = rusqlite::Connection::open(dir.join("graph.db")).unwrap();
    c.execute_batch(
        "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL); PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(c);
    env.build();
    assert!(env.computed_at() > 0);
}

// ---- T-035 : bout en bout sur le dépôt Maven à 3 modules

impl Env {
    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Commande d'écriture avec ou sans configuration LLM.
    fn write_cmd(&self, args: &[&str]) -> Output {
        let out = Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        out
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[test]
fn req019_ac5_summary_without_llm_has_structure_and_no_themes() {
    let env = Env::new();
    env.write_cmd(&["build", "--no-llm"]);
    let s = env.json(&["summary", "--json"]);
    assert_eq!(s["themes"], serde_json::json!([]), "{s}");
    assert_eq!(s["modules"].as_array().unwrap().len(), 3, "{s}");
    let api = s["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "api")
        .unwrap();
    assert_eq!(strings(&api["depends_on"]), vec!["module:com.acme:core"]);
    assert!(!s["top_nodes"].as_array().unwrap().is_empty());
    let out = env.read(&["summary"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = text(&out.stdout);
    assert!(t.contains("aucun thème (graphe non enrichi)"), "{t}");
    assert!(t.contains("core"), "{t}");
    assert!(t.contains("slf4j-api"), "{t}");
}

#[test]
fn req019_ac2_ac3_summary_recomputed_only_on_change() {
    let env = Env::new();
    env.write_cmd(&["build", "--no-llm"]);
    let t0 = env.computed_at();
    std::thread::sleep(std::time::Duration::from_millis(20));
    // Aucun changement : résumé non recalculé.
    env.write_cmd(&["update", "--no-llm"]);
    assert_eq!(env.computed_at(), t0);
    // Enrichissement seul (aucun changement de fichier) : résumé recalculé, thèmes présents.
    env.write_cmd(&["update"]);
    let t1 = env.computed_at();
    assert!(t1 > t0);
    assert!(!env.json(&["summary", "--json"])["themes"]
        .as_array()
        .unwrap()
        .is_empty());
    std::thread::sleep(std::time::Duration::from_millis(20));
    // Changement de structure : résumé recalculé et stocké.
    env.write("extra/pom.xml",
        "<project><groupId>com.acme</groupId><artifactId>extra</artifactId>\
         <dependencies><dependency><groupId>com.acme</groupId><artifactId>core</artifactId></dependency></dependencies></project>");
    env.write_cmd(&["update"]);
    assert!(env.computed_at() > t1);
    let s = env.json(&["summary", "--json"]);
    assert_eq!(s["modules"].as_array().unwrap().len(), 4, "{s}");
    // Rien ne change plus.
    let t2 = env.computed_at();
    std::thread::sleep(std::time::Duration::from_millis(20));
    env.write_cmd(&["update"]);
    assert_eq!(env.computed_at(), t2);
}

const NOTES: &str = "docs/notes.md";

fn mentions_in(v: &Value) -> Vec<Value> {
    v["unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|u| u["kind"] == "mentions")
        .cloned()
        .collect()
}

#[test]
fn req008_ac7_candidate_mentions_are_not_unresolved_and_link_after_update() {
    let env = Env::new();
    env.write(NOTES, "# Notes\n\nVoir `Inexistant` et `config.toml`.\n");
    let out = env.write_cmd(&["build"]);
    let bilan = text(&out.stdout);
    let v = env.json(&["export", "--output", "-"]);
    let n = v["unresolved"].as_array().unwrap().len();
    assert!(
        bilan.contains(&format!("{n} références non résolues")),
        "{bilan}"
    );
    assert!(mentions_in(&v).is_empty(), "{:?}", v["unresolved"]);
    assert!(!v["unresolved"].to_string().contains("Inexistant"));
    assert!(!v["unresolved"].to_string().contains("config.toml"));
    let linked = |v: &Value| {
        v["edges"].as_array().unwrap().iter().any(|e| {
            e["kind"] == "mentions"
                && e["source"] == "section:docs/notes.md#notes"
                && e["target"] == "type:com.acme.core.Inexistant"
        })
    };
    assert!(!linked(&v));

    env.write(
        "core/src/main/java/com/acme/core/Inexistant.java",
        "package com.acme.core;\npublic class Inexistant { }\n",
    );
    let before = env.log().lines().count();
    let upd = env.write_cmd(&["update"]);
    assert!(
        text(&upd.stdout).contains("terminé : 1 fichiers traités,"),
        "{}",
        text(&upd.stdout)
    );
    let v = env.json(&["export", "--output", "-"]);
    assert!(linked(&v), "lien mentions absent");
    assert!(mentions_in(&v).is_empty());
    let new_log: Vec<String> = env.log().lines().skip(before).map(String::from).collect();
    assert!(
        new_log.iter().all(|l| !l.contains(NOTES)),
        "le .md a été renvoyé au LLM : {new_log:?}"
    );
}

fn has_mention(v: &Value, source: &str) -> bool {
    v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "mentions" && e["source"] == source)
}

#[test]
fn req023_ac4_only_backticked_mentions_outside_code_blocks_after_a_title() {
    let env = Env::new();
    env.write(
        "docs/plain.md",
        "OrderService avant le titre, `OrderService` aussi.\n\n# Titre\n\n\
         OrderService sans backticks.\n\n```\nOrderService\n`OrderService`\n```\n",
    );
    env.write_cmd(&["build", "--no-llm"]);
    let v = env.json(&["export", "--output", "-"]);
    assert!(
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "section:docs/plain.md#titre"),
        "section absente"
    );
    assert!(!has_mention(&v, "section:docs/plain.md#titre"), "{v}");
    assert!(!has_mention(&v, "document:docs/plain.md"), "{v}");
    let to_service = v["edges"].as_array().unwrap().iter().any(|e| {
        e["kind"] == "mentions"
            && e["source"].as_str().unwrap_or("").contains("docs/plain.md")
            && e["target"] == "type:com.acme.core.OrderService"
    });
    assert!(!to_service, "lien mentions vers OrderService : {v}");
}

#[test]
fn req023_ac5_duplicate_titles_get_numbered_section_ids() {
    let env = Env::new();
    env.write(
        "docs/dup.md",
        "# Doc\n\n## Exemple\n\npremier\n\n## Exemple\n\nsecond\n",
    );
    env.write_cmd(&["build", "--no-llm"]);
    let v = env.json(&["export", "--output", "-"]);
    let ids: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["id"].as_str())
        .collect();
    assert!(ids.contains(&"section:docs/dup.md#exemple"), "{ids:?}");
    assert!(ids.contains(&"section:docs/dup.md#exemple-1"), "{ids:?}");
}

/// REQ-147 : le résumé stocké porte la clé `communities` (total et au plus dix communautés, par
/// numéro croissant, de tailles non croissantes) ; le texte de `summary` ne change pas (T-232).
#[test]
fn req147_summary_json_carries_the_communities_key_and_the_text_is_unchanged() {
    let env = Env::built();
    let s: Value = serde_json::from_slice(&env.read(&["summary", "--json"]).stdout).unwrap();
    let c = &s["communities"];
    let total = c["total"].as_u64().unwrap();
    let top = c["top"].as_array().unwrap();
    assert!(total >= 2);
    assert_eq!(top.len() as u64, total.min(10));
    for (i, e) in top.iter().enumerate() {
        assert_eq!(e["id"], i);
        assert!(!e["label"].as_str().unwrap().is_empty());
        assert!(e["cohesion"].as_f64().unwrap() <= 1.0);
    }
    let sizes: Vec<u64> = top.iter().map(|e| e["size"].as_u64().unwrap()).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
    // T-232 : le texte de la 0.24.4 est suivi de la seule section des communautés.
    let text = text(&env.read(&["summary"]).stdout);
    let (before, section) = text.split_once("Communautés (").unwrap();
    assert!(
        before.ends_with('\n') && !before.contains("ommunaut"),
        "{text}"
    );
    assert!(section.starts_with(&format!("{total})\n")), "{text}");
}
