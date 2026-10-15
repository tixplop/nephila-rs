//! Dépôts Gradle (Groovy, Kotlin DSL) et Ivy de bout en bout, sans configuration LLM
//! (REQ-022, REQ-001 AC-001-4, REQ-017 AC-017-1 à 017-3, REQ-018).

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
}

impl Env {
    fn new(fixture: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        copy_dir(&fx.join(fixture), &repo);
        Env { _tmp: tmp, repo }
    }

    /// Toute commande s'exécute sans configuration LLM.
    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("rkg"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("RKG_CONFIG", self.repo.join("absent.toml"))
            .env_remove("RKG_MOCK_DIR")
            .env_remove("RKG_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.cmd(args);
        assert!(out.status.success(), "{}", text(&out.stderr));
        out
    }

    fn built(fixture: &str) -> Env {
        let env = Env::new(fixture);
        let out = env.ok(&["build"]);
        assert!(text(&out.stderr).is_empty(), "{}", text(&out.stderr));
        env
    }

    fn export(&self) -> Value {
        serde_json::from_slice(&self.ok(&["export", "--output", "-"]).stdout).unwrap()
    }

    fn write(&self, rel: &str, content: &str) {
        fs::write(self.repo.join(rel), content).unwrap();
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn node<'a>(v: &'a Value, id: &str) -> Option<&'a Value> {
    v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id)
}

fn has_edge(v: &Value, source: &str, kind: &str, target: &str) -> bool {
    v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["source"] == source && e["kind"] == kind && e["target"] == target)
}

fn depends(v: &Value, source: &str) -> Vec<String> {
    let mut d: Vec<String> = v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["source"] == source && e["kind"] == "depends_on")
        .map(|e| e["target"].as_str().unwrap().to_string())
        .collect();
    d.sort();
    d
}

fn assert_gradle_links(v: &Value, root_short: &str) {
    assert_eq!(node(v, "module::").unwrap()["short"], root_short);
    assert!(node(v, "module::api").is_some(), "{v}");
    assert!(node(v, "module::core").is_some());
    let d = depends(v, "module::api");
    assert!(
        d.contains(&"dependency:org.slf4j:slf4j-api".to_string()),
        "{d:?}"
    );
    assert!(d.contains(&"module::core".to_string()), "{d:?}");
}

#[test]
fn req022_ac4_gradle_groovy_modules_and_dependencies() {
    let env = Env::built("gradle/groovy");
    let v = env.export();
    assert_gradle_links(&v, "shop");
    // `testImplementation group: ..., name: ...` (AC-022-4)
    assert_eq!(
        depends(&v, "module::api"),
        vec![
            "dependency:junit:junit".to_string(),
            "dependency:org.slf4j:slf4j-api".to_string(),
            "module::core".to_string()
        ]
    );
    assert!(has_edge(
        &v,
        "module::core",
        "depends_on",
        "dependency:com.google.guava:guava"
    ));
    assert_eq!(node(&v, "module::api").unwrap()["short"], "api");
}

#[test]
fn req022_ac5_gradle_kotlin_dsl_gives_the_same_links() {
    let env = Env::built("gradle/kotlin");
    let v = env.export();
    assert_gradle_links(&v, "shopkts");
    assert_eq!(
        depends(&v, "module::api"),
        vec![
            "dependency:org.slf4j:slf4j-api".to_string(),
            "module::core".to_string()
        ]
    );
    assert!(has_edge(
        &v,
        "module::core",
        "depends_on",
        "dependency:com.google.guava:guava"
    ));
}

#[test]
fn req022_ac6_ivy_module_and_dependency_with_short_name() {
    let env = Env::built("ivy");
    let v = env.export();
    let m = node(&v, "module:com.acme:legacy").expect("module Ivy");
    assert_eq!(m["short"], "legacy");
    assert!(has_edge(
        &v,
        "module:com.acme:legacy",
        "depends_on",
        "dependency:commons-lang:commons-lang"
    ));
    assert!(node(&v, "dependency:commons-lang:commons-lang").is_some());
}

#[test]
fn req022_ac8_gradle_types_are_contained_by_package_then_module() {
    let env = Env::built("gradle/groovy");
    let v = env.export();
    assert!(has_edge(
        &v,
        "package:com.acme.api",
        "contains",
        "type:com.acme.api.Api"
    ));
    assert!(has_edge(
        &v,
        "module::api",
        "contains",
        "package:com.acme.api"
    ));
    assert!(has_edge(
        &v,
        "module::core",
        "contains",
        "package:com.acme.core"
    ));
}

#[test]
fn req022_ac9_non_literal_gradle_dependencies_are_ignored_without_warning() {
    let env = Env::new("gradle/edge");
    let out = env.ok(&["build"]);
    assert!(
        !text(&out.stderr).contains("avertissement"),
        "{}",
        text(&out.stderr)
    );
    let v = env.export();
    assert_eq!(
        depends(&v, "module::app"),
        vec![
            "dependency:org.slf4j:slf4j-api".to_string(),
            "dependency:org.springframework.boot:spring-boot-dependencies".to_string(),
            "module::core".to_string(),
        ]
    );
    assert!(node(&v, "dependency:com.acme:plugin").is_none());
}

#[test]
fn req022_modified_settings_then_update_renames_root() {
    let env = Env::built("gradle/groovy");
    env.write(
        "settings.gradle",
        "rootProject.name = 'renamed'\ninclude 'api', 'core'\n",
    );
    let out = env.ok(&["update"]);
    assert!(!text(&out.stdout).contains("graphe à jour"));
    let v = env.export();
    assert_eq!(node(&v, "module::").unwrap()["short"], "renamed");
    assert_gradle_links(&v, "renamed");
    assert!(text(&env.ok(&["status"]).stdout).starts_with("graphe à jour"));
}

#[test]
fn req001_ac4_and_req017_on_gradle_repo_without_llm_configuration() {
    let env = Env::built("gradle/groovy");
    // AC-017-2 : graphe à jour, aucun avertissement.
    for args in [
        vec!["query", "search", "Api"],
        vec!["query", "neighbors", "type:com.acme.api.Api"],
        vec!["status"],
        vec!["summary"],
        vec!["export", "--output", "-"],
    ] {
        let out = env.ok(&args);
        assert!(!text(&out.stderr).contains("pas à jour"), "{args:?}");
    }
    // AC-017-1 : 3 fichiers modifiés sans update.
    env.write("api/build.gradle", "dependencies {\n}\n");
    env.write("core/build.gradle", "dependencies {\n}\n");
    env.write("build.gradle", "// modifié\n");
    for args in [
        vec!["query", "search", "Api"],
        vec!["query", "neighbors", "type:com.acme.api.Api"],
        vec!["query", "path", "type:com.acme.api.Api", "module::core"],
    ] {
        let out = env.cmd(&args);
        assert!(
            text(&out.stderr).contains("graphe pas à jour : 3 fichiers modifiés"),
            "{args:?} {}",
            text(&out.stderr)
        );
    }
    // AC-017-3 : aucun graphe.
    let empty = Env::new("gradle/groovy");
    for args in [
        vec!["query", "search", "Api"],
        vec!["query", "neighbors", "type:com.acme.api.Api"],
        vec!["query", "path", "a", "b"],
    ] {
        let out = empty.cmd(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(text(&out.stderr).contains("rkg build"), "{args:?}");
    }
}

fn rkg_in(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new(cargo_bin("rkg"))
        .current_dir(dir)
        .args(args)
        .env("RKG_CONFIG", dir.join("absent.toml"))
        .env_remove("RKG_MOCK_DIR")
        .env_remove("RKG_MOCK_LOG")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?} {}", text(&out.stderr));
    out
}

fn assert_monrepo_module(dir: &Path, args: &[&str]) {
    let v: Value = serde_json::from_slice(&rkg_in(dir, args).stdout).unwrap();
    assert_eq!(node(&v, "module::").unwrap()["short"], "monrepo", "{v}");
    assert!(
        has_edge(
            &v,
            "module::",
            "depends_on",
            "dependency:org.slf4j:slf4j-api"
        ),
        "{v}"
    );
}

#[test]
fn req022_ac10_root_module_named_after_folder_without_dash_c() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("monrepo");
    fs::create_dir_all(repo.join("sub")).unwrap();
    let script = "dependencies {\n    implementation 'org.slf4j:slf4j-api:2.0.9'\n}\n";
    fs::write(repo.join("build.gradle"), script).unwrap();

    rkg_in(&repo, &["build"]);
    assert_monrepo_module(&repo, &["export", "--output", "-"]);

    rkg_in(&repo.join("sub"), &["-C", "..", "build"]);
    assert_monrepo_module(&repo, &["export", "--output", "-"]);

    fs::write(
        repo.join("build.gradle"),
        "dependencies {\n    implementation 'org.slf4j:slf4j-api:2.0.9'\n    // modifié\n}\n",
    )
    .unwrap();
    rkg_in(&repo, &["update"]);
    assert_monrepo_module(&repo, &["export", "--output", "-"]);
}
