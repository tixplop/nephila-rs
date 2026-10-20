//! Extraction multi-langages de bout en bout (REQ-006 AC-006-9 à 006-11 et 006-13,
//! REQ-005 AC-005-3 et 005-4, REQ-014 AC-014-1, REQ-015 AC-015-1) sur un projet Maven
//! mêlant Java, Kotlin, Scala, Groovy et Markdown, sans configuration LLM.

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
    build: Output,
}

impl Env {
    fn run(repo: &Path, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("NEPHILA_CONFIG", repo.join("absent.toml"))
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn built() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/langs/mixed");
        copy_dir(&fx, &repo);
        let build = Env::run(&repo, &["build"]);
        Env {
            _tmp: tmp,
            repo,
            build,
        }
    }

    fn cmd(&self, args: &[&str]) -> Output {
        Env::run(&self.repo, args)
    }

    fn export(&self) -> Value {
        let out = self.cmd(&["export", "--output", "-"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
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

fn assert_type(v: &Value, id: &str, subkind: &str) {
    let n = node(v, &format!("type:{id}")).unwrap_or_else(|| panic!("{id} absent"));
    assert_eq!(n["subkind"], subkind, "{id}");
}

#[test]
fn req006_ac1_build_without_config_succeeds_with_nothing_pending() {
    let env = Env::built();
    assert!(env.build.status.success(), "{}", text(&env.build.stderr));
    assert!(text(&env.build.stderr).is_empty());
    assert!(text(&env.build.stdout).contains("0 fichiers en attente"));
    assert!(text(&env.cmd(&["status"]).stdout).starts_with("graphe à jour"));
}

#[test]
fn req006_ac9_java_types_and_calls() {
    let v = Env::built().export();
    assert_type(&v, "com.acme.core.Order", "record");
    assert_type(&v, "com.acme.core.OrderRepository", "interface");
    assert_type(&v, "com.acme.core.BaseRepository", "class");
    assert!(has_edge(
        &v,
        "type:com.acme.core.BaseRepository",
        "implements",
        "type:com.acme.core.OrderRepository"
    ));
    assert_type(&v, "com.acme.core.Kinds", "enum");
    assert_type(&v, "com.acme.core.Marker", "annotation");
    // sealed ... permits : les sous-types déclarés implémentent l'interface scellée.
    assert_type(&v, "com.acme.core.Shape", "interface");
    for t in ["Square", "Round"] {
        assert!(has_edge(
            &v,
            &format!("type:com.acme.core.{t}"),
            "implements",
            "type:com.acme.core.Shape"
        ));
    }
    // sealed class ... permits (sous-genre class).
    assert_type(&v, "com.acme.core.Vehicle", "class");
    assert!(has_edge(
        &v,
        "type:com.acme.core.Car",
        "extends",
        "type:com.acme.core.Vehicle"
    ));
    // Classe imbriquée Externe.Interne et ses méthodes.
    assert_type(&v, "com.acme.core.Externe", "class");
    assert_type(&v, "com.acme.core.Externe.Interne", "class");
    assert!(node(&v, "method:com.acme.core.Externe.Interne.ping").is_some());
    // Appel dans une lambda attribué à la méthode englobante.
    assert!(has_edge(
        &v,
        "method:com.acme.core.Externe.viaLambda",
        "calls",
        "method:com.acme.core.OrderRepository.save"
    ));
    // Switch expression et bloc de texte : les méthodes sont extraites.
    assert!(node(&v, "method:com.acme.core.Externe.pick").is_some());
    assert!(node(&v, "method:com.acme.core.Externe.text").is_some());
}

#[test]
fn req006_ac10_kotlin_subkinds_top_level_function_and_extends_java() {
    let v = Env::built().export();
    assert_type(&v, "com.acme.kt.Stats", "class");
    assert_type(&v, "com.acme.kt.Registry", "object");
    assert!(node(&v, "method:com.acme.kt.FastRepositoryKt.helperTop").is_some());
    // Lien entre langages : classe Kotlin qui étend une classe Java.
    assert!(has_edge(
        &v,
        "type:com.acme.kt.FastRepository",
        "extends",
        "type:com.acme.core.BaseRepository"
    ));
    assert_type(&v, "com.acme.core.Auditable", "interface");
    assert_type(&v, "com.acme.core.Level", "enum");
    assert_type(&v, "com.acme.core.S", "class");
    assert_type(&v, "com.acme.core.S.Companion", "object");
    assert!(node(&v, "method:com.acme.kt.UtilKt.f").is_some());
    assert!(has_edge(
        &v,
        "type:com.acme.core.S",
        "extends",
        "type:com.acme.core.Base"
    ));
    assert!(has_edge(
        &v,
        "type:com.acme.core.S",
        "implements",
        "type:com.acme.core.Auditable"
    ));
    // Paramètre de constructeur `private val repo: OrderRepository`.
    assert!(has_edge(
        &v,
        "method:com.acme.core.S.m",
        "calls",
        "method:com.acme.core.OrderRepository.save"
    ));
}

#[test]
fn req006_ac11_scala_subkinds_and_extends() {
    let v = Env::built().export();
    assert_type(&v, "com.acme.sc.Pricing", "trait");
    assert_type(&v, "com.acme.sc.Discount", "class");
    assert_type(&v, "com.acme.sc.Defaults", "object");
    assert!(has_edge(
        &v,
        "type:com.acme.sc.Discount",
        "extends",
        "type:com.acme.sc.Pricing"
    ));
    assert_type(&v, "com.acme.sc.Plain", "class");
    assert!(node(&v, "method:com.acme.sc.Plain.hello").is_some());
    assert_type(&v, "com.acme.sc.Mode", "enum");
    // `extends A with B` : extends pour le premier, implements pour les suivants.
    assert!(has_edge(
        &v,
        "type:com.acme.sc.C",
        "extends",
        "type:com.acme.sc.A"
    ));
    assert!(has_edge(
        &v,
        "type:com.acme.sc.C",
        "implements",
        "type:com.acme.sc.B"
    ));
}

#[test]
fn req006_ac11_groovy_interface_class_and_implements() {
    let v = Env::built().export();
    assert_type(&v, "com.acme.gv.Shape", "interface");
    assert_type(&v, "com.acme.gv.Circle", "class");
    assert!(node(&v, "method:com.acme.gv.Circle.area").is_some());
    assert!(has_edge(
        &v,
        "type:com.acme.gv.Circle",
        "implements",
        "type:com.acme.gv.Shape"
    ));
    assert_type(&v, "com.acme.gv.Color", "enum");
    assert!(node(&v, "method:com.acme.gv.Derived.compute").is_some());
    assert!(has_edge(
        &v,
        "type:com.acme.gv.Derived",
        "extends",
        "type:com.acme.gv.Base"
    ));
}

#[test]
fn req006_ac13_groovy_trait_is_a_known_limit_not_a_failure() {
    let env = Env::built();
    assert!(env.build.status.success(), "{}", text(&env.build.stderr));
    assert!(text(&env.build.stderr).is_empty());
    let v = env.export();
    for id in [
        "type:com.acme.gv.Greeter",
        "method:com.acme.gv.Greeter.greet",
        "type:com.acme.gv.Shapes",
        "method:com.acme.gv.Shapes.run",
    ] {
        assert!(node(&v, id).is_none(), "{id}");
    }
    assert!(node(&v, "type:com.acme.gv.Shape").is_some());
    for u in v["unresolved"].as_array().unwrap() {
        assert!(u["target"] != "trait" && u["target"] != "String", "{u}");
    }
    // Ni en attente ni en échec : le fichier est indexé.
    let status = text(&env.cmd(&["status"]).stdout);
    assert!(status.starts_with("graphe à jour"), "{status}");
    // Voisins du type Circle : aucune trace du trait.
    let out = env.cmd(&["query", "neighbors", "Circle"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!text(&out.stdout).contains("Greeter"));
}

#[test]
fn req005_ac3_ac4_markdown_mentions_scala_and_java_types() {
    let v = Env::built().export();
    assert!(has_edge(
        &v,
        "section:docs/guide.md#tarifs",
        "mentions",
        "type:com.acme.sc.Pricing"
    ));
    assert!(has_edge(
        &v,
        "section:docs/guide.md#tarifs",
        "mentions",
        "type:com.acme.core.OrderService"
    ));
    let subs: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["subkind"].as_str())
        .collect();
    for s in [
        "record",
        "interface",
        "object",
        "trait",
        "enum",
        "annotation",
    ] {
        assert!(subs.contains(&s), "{s} absent de {subs:?}");
    }
}

#[test]
fn req014_ac1_neighbors_from_analysis_calls() {
    let env = Env::built();
    let out = env.cmd(&["query", "neighbors", "OrderService"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = text(&out.stdout);
    let used_by = t.find("utilisé par").expect("section utilisé par");
    let uses = t.find("utilise").expect("section utilise");
    assert!(uses < used_by, "{t}");
    let (up, down) = t.split_at(used_by);
    assert!(
        up.lines()
            .any(|l| l.contains("com.acme.core.OrderRepository.save") && l.contains("calls")),
        "{t}"
    );
    assert!(
        up.lines()
            .any(|l| l.contains("com.acme.core.Order ") && l.contains("imports")),
        "{t}"
    );
    assert!(
        down.lines()
            .any(|l| l.contains("com.acme.api.ApiController.create") && l.contains("calls")),
        "{t}"
    );
}

#[test]
fn req015_ac1_path_from_analysis_calls() {
    let env = Env::built();
    let out = env.cmd(&["query", "path", "ApiController", "OrderRepository"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = text(&out.stdout);
    let order = [
        "ApiController",
        "contains",
        "ApiController.create",
        "calls",
        "OrderService.place",
        "calls",
        "OrderRepository.save",
        "OrderRepository",
    ];
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines.len(), 4, "{t}");
    assert!(lines[3].contains("contenu par"), "{t}");
    let mut at = 0;
    for p in order {
        at += t[at..]
            .find(p)
            .unwrap_or_else(|| panic!("« {p} » absent : {t}"));
    }
}

fn big_java(terms: usize) -> String {
    let tail: String = (1..terms).map(|i| format!(" + \"s{i}\"")).collect();
    format!(
        "class Big {{\n  String helper() {{ return \"\"; }}\n  String m() {{\n    return helper(){tail};\n  }}\n}}\n"
    )
}

fn big_repo(terms: usize) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/Big.java"), big_java(terms)).unwrap();
    (tmp, repo)
}

fn assert_big_graph(repo: &Path) {
    let st = Env::run(repo, &["status"]);
    assert!(
        text(&st.stdout).starts_with("graphe à jour"),
        "{}",
        text(&st.stdout)
    );
    let out = Env::run(repo, &["export", "--output", "-"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(node(&v, "type:Big").is_some());
    assert!(node(&v, "method:Big.m").is_some());
    assert!(has_edge(&v, "method:Big.m", "calls", "method:Big.helper"));
}

#[test]
fn req044_ac1_java_concatenation_de_10000_termes_indexee() {
    let (_tmp, repo) = big_repo(10_000);
    let out = Env::run(&repo, &["build"]);
    assert!(
        out.status.success(),
        "{:?} {}",
        out.status,
        text(&out.stderr)
    );
    assert!(text(&out.stderr).is_empty(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("0 fichiers en attente"));
    assert_big_graph(&repo);
}

#[test]
fn req044_ac4_update_apres_modification() {
    let (_tmp, repo) = big_repo(10_000);
    let out = Env::run(&repo, &["build"]);
    assert!(
        out.status.success(),
        "{:?} {}",
        out.status,
        text(&out.stderr)
    );
    fs::write(repo.join("src/Big.java"), big_java(10_001)).unwrap();
    let out = Env::run(&repo, &["update"]);
    assert!(
        out.status.success(),
        "{:?} {}",
        out.status,
        text(&out.stderr)
    );
    assert!(
        text(&out.stdout).contains("0 fichiers en attente"),
        "{}",
        text(&out.stdout)
    );
    assert_big_graph(&repo);
}
