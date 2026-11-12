//! Commande `nephila update` de bout en bout avec le LLM simulé (REQ-011, REQ-008, REQ-010, REQ-019,
//! REQ-020).

use assert_cmd::cargo::cargo_bin;
use nephila::model::{EdgeKind, FileState};
use nephila::store::Store;
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
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nbatch_files = 1\nmodel = \"mock\"\n",
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
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .arg(sub)
            .env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
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

    fn ok(&self, sub: &str) -> Output {
        let out = self.run(sub, &[]);
        assert!(out.status.success(), "{sub} : {}", stderr(&out));
        out
    }

    /// `export --output -` : le JSON sur stdout.
    fn export_stdout(&self) -> Output {
        let out = self
            .command("export", &[])
            .args(["--output", "-"])
            .output()
            .unwrap();
        assert!(out.status.success(), "export : {}", stderr(&out));
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

/// Chemin d'un fichier de configuration inexistant : LLM inactif.
const NO_CONFIG: (&str, &str) = ("NEPHILA_CONFIG", "/nonexistent/nephila-absent.toml");

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
    }
    env.fixture(
        "src/p/C7.java",
        r#"{"version":2,"summaries":[{"node":"type:p.C7","summary":"Résumé de C7."}],"concepts":[]}"#,
    );
    env.ok("build");
    let before = env.store().nodes().unwrap();
    assert!(before.len() >= 500);
    assert!(before.iter().any(|n| n.summary.is_some()));
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
    env.clear_log();
    env.ok("update");
    // AC-011-4 révisé (REQ-257) : le nouveau chemin reprend l'enrichissement, rien n'est envoyé.
    assert!(env.log_lines().is_empty(), "{:?}", env.log_lines());
    let order = env
        .store()
        .nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.id == "type:com.acme.domain.Order")
        .unwrap();
    assert_eq!(order.file.as_deref(), Some(new));
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

const ORDER_SUMMARY: &str = r#"{"version":2,"summaries":[{"node":"type:com.acme.domain.Order","summary":"Une commande."}],"concepts":[]}"#;

fn summaries(env: &Env) -> std::collections::BTreeMap<String, String> {
    env.store()
        .nodes()
        .unwrap()
        .into_iter()
        .filter_map(|n| n.summary.map(|s| (n.id, s)))
        .collect()
}

fn to_enrich(env: &Env) -> u64 {
    let out = env.command("status", &[]).arg("--json").output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["to_enrich"].as_u64().unwrap()
}

fn rename_order(env: &Env) -> String {
    let new = "core/src/main/java/com/acme/domain/Order2.java".to_string();
    fs::rename(env.repo.join(ORDER), env.repo.join(&new)).unwrap();
    env.clear_log();
    new
}

#[test]
fn req257_ac1_renamed_java_file_keeps_its_enrichment() {
    let env = Env::maven();
    env.fixture(ORDER, ORDER_SUMMARY);
    env.ok("build");
    assert_eq!(
        summaries(&env)
            .get("type:com.acme.domain.Order")
            .map(String::as_str),
        Some("Une commande.")
    );
    let new = rename_order(&env);
    env.ok("update");
    assert!(env.log_lines().is_empty(), "{:?}", env.log_lines());
    let order = env
        .store()
        .nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.id == "type:com.acme.domain.Order")
        .unwrap();
    assert_eq!(order.file.as_deref(), Some(new.as_str()));
    assert_eq!(order.summary.as_deref(), Some("Une commande."));
    assert_eq!(to_enrich(&env), 0);
    let store = env.store();
    assert!(store.enrichment_entry(&new).unwrap().is_some());
    assert!(store.enrichment_entry(ORDER).unwrap().is_none());
}

#[test]
fn req257_ac1_verbose_counts_the_adopted_enrichment() {
    let env = Env::maven();
    env.fixture(ORDER, ORDER_SUMMARY);
    env.ok("build");
    rename_order(&env);
    let out = env
        .command("update", &[])
        .arg("--verbose")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let line = stderr(&out)
        .lines()
        .find(|l| l.contains("structure: fin"))
        .unwrap_or_default()
        .to_string();
    assert!(line.ends_with(", 1 enrichissements conservés"), "{line}");
}

const DOC_FIXTURE: &str = r#"{"version":2,"summaries":[{"node":"document:docs/a.md","summary":"Le document."},{"node":"section:docs/a.md#intro","summary":"L'intro."},{"node":"section:docs/a.md#détails","summary":"Les détails."}],"concepts":[{"name":"Idée","summary":"Une idée.","sections":["section:docs/a.md#intro","section:docs/a.md#détails"]}]}"#;

fn doc_env() -> Env {
    let env = Env::empty();
    fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    env.write("docs/a.md", "# Intro\n\nTexte.\n\n## Détails\n\nSuite.\n");
    env.fixture("docs/a.md", DOC_FIXTURE);
    env
}

fn check_moved_document(env: &Env) {
    assert!(env.log_lines().is_empty(), "{:?}", env.log_lines());
    let s = summaries(env);
    assert_eq!(s["document:guide/a.md"], "Le document.");
    assert_eq!(s["section:guide/a.md#intro"], "L'intro.");
    assert_eq!(s["section:guide/a.md#détails"], "Les détails.");
    let ids = env.node_ids();
    assert!(ids.contains("concept:idée"));
    assert!(ids.iter().all(|i| !i.contains("docs/a.md")), "{ids:?}");
    for sec in ["intro", "détails"] {
        assert!(env.has_edge(
            &format!("section:guide/a.md#{sec}"),
            EdgeKind::Mentions,
            "concept:idée"
        ));
    }
}

#[test]
fn req257_ac2_moved_markdown_keeps_summaries_and_concepts_with_update() {
    let env = doc_env();
    env.ok("build");
    assert_eq!(summaries(&env)["document:docs/a.md"], "Le document.");
    fs::create_dir_all(env.repo.join("guide")).unwrap();
    fs::rename(env.repo.join("docs/a.md"), env.repo.join("guide/a.md")).unwrap();
    env.clear_log();
    env.ok("update");
    check_moved_document(&env);
}

#[test]
fn req257_ac2_moved_markdown_keeps_summaries_and_concepts_with_build() {
    let env = doc_env();
    env.ok("build");
    fs::create_dir_all(env.repo.join("guide")).unwrap();
    fs::rename(env.repo.join("docs/a.md"), env.repo.join("guide/a.md")).unwrap();
    env.clear_log();
    env.ok("build");
    check_moved_document(&env);
}

#[test]
fn req257_ac3_moved_json_and_typescript_keep_summaries() {
    let env = Env::empty();
    env.write("conf/x.json", "{\"a\":{\"b\":1},\"c\":2}\n");
    env.write("src/a.ts", "export class Foo {\n  run() { return 1; }\n}\n");
    env.fixture(
        "conf/x.json",
        r#"{"version":2,"summaries":[{"node":"document:conf/x.json","summary":"La conf."},{"node":"key:conf/x.json#a.b","summary":"La clé b."}],"concepts":[]}"#,
    );
    env.fixture(
        "src/a.ts",
        r#"{"version":2,"summaries":[{"node":"package:src/a","summary":"Le module."},{"node":"type:src/a.Foo","summary":"La classe."},{"node":"method:src/a.Foo.run","summary":"La méthode."}],"concepts":[]}"#,
    );
    env.ok("build");
    fs::create_dir_all(env.repo.join("etc")).unwrap();
    fs::create_dir_all(env.repo.join("lib")).unwrap();
    fs::rename(env.repo.join("conf/x.json"), env.repo.join("etc/x.json")).unwrap();
    fs::rename(env.repo.join("src/a.ts"), env.repo.join("lib/b.ts")).unwrap();
    env.clear_log();
    env.ok("update");
    assert!(env.log_lines().is_empty(), "{:?}", env.log_lines());
    let s = summaries(&env);
    assert_eq!(s["document:etc/x.json"], "La conf.", "{s:?}");
    // Les nœuds `key:` et `package:` n'ont pas de résumé : ils existent sous leur nouvel identifiant.
    let ids = env.node_ids();
    assert!(ids.contains("key:etc/x.json#a.b") && ids.contains("package:lib/b"));
    assert_eq!(s["type:lib/b.Foo"], "La classe.");
    assert_eq!(s["method:lib/b.Foo.run"], "La méthode.");
}

#[test]
fn req257_ac4_no_adoption_when_content_or_type_differs() {
    // (a) renommé et modifié dans le même update : envoyé.
    let env = doc_env();
    env.ok("build");
    fs::create_dir_all(env.repo.join("guide")).unwrap();
    fs::remove_file(env.repo.join("docs/a.md")).unwrap();
    env.write("guide/a.md", "# Intro\n\nAutre texte.\n");
    env.clear_log();
    env.ok("update");
    assert_eq!(
        env.logged_paths(),
        BTreeSet::from(["guide/a.md".to_string()])
    );

    // (b) copié, l'original gardé : la copie est envoyée.
    let env = doc_env();
    env.ok("build");
    env.write("docs/b.md", "# Intro\n\nTexte.\n\n## Détails\n\nSuite.\n");
    env.clear_log();
    env.ok("update");
    assert_eq!(
        env.logged_paths(),
        BTreeSet::from(["docs/b.md".to_string()])
    );

    // (c) autre type de fichier, même contenu : envoyé.
    let env = Env::empty();
    env.write("conf/x.json", "{\"a\":1}\n");
    env.fixture(
        "conf/x.json",
        r#"{"version":2,"summaries":[{"node":"document:conf/x.json","summary":"La conf."}],"concepts":[]}"#,
    );
    env.ok("build");
    fs::rename(env.repo.join("conf/x.json"), env.repo.join("conf/x.yaml")).unwrap();
    env.clear_log();
    env.ok("update");
    assert_eq!(
        env.logged_paths(),
        BTreeSet::from(["conf/x.yaml".to_string()])
    );

    // (d) renommage avec un autre modèle : envoyé une seule fois.
    let env = doc_env();
    env.ok("build");
    fs::write(&env.config, "provider = \"mock\"\nmodel = \"autre\"\n").unwrap();
    fs::create_dir_all(env.repo.join("guide")).unwrap();
    fs::rename(env.repo.join("docs/a.md"), env.repo.join("guide/a.md")).unwrap();
    env.clear_log();
    env.ok("update");
    assert_eq!(env.log_lines().len(), 1, "{:?}", env.log_lines());
    env.clear_log();
    env.ok("update");
    assert!(env.log_lines().is_empty(), "{:?}", env.log_lines());
}

#[test]
fn req011_ac5_modified_file_replaces_nodes_and_refs() {
    let env = Env::maven();
    env.ok("build");
    assert!(env
        .node_ids()
        .contains("method:com.acme.core.OrderRepository.save"));
    env.write(
        ORDER_REPO,
        "package com.acme.core;\n\nimport com.acme.domain.Order;\n\npublic interface OrderRepository {\n    Order load(long id);\n}\n",
    );
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
    env.ok("build");
    let mut store = nephila::store::Store::open_existing(&env.repo).unwrap();
    let pending: Vec<_> = store
        .files()
        .unwrap()
        .into_iter()
        .filter(|f| f.path == ORDER_SERVICE || f.path == ORDER_REPO)
        .collect();
    assert_eq!(pending.len(), 2);
    store.insert_pending_files(&pending).unwrap();
    drop(store);
    env.clear_log();
    env.ok("update");
    // Traités (réanalysés, de nouveau « indexés ») ; leur contenu n'ayant pas changé, leurs
    // résumés sont réappliqués sans appel LLM (AC-049-9).
    assert!(env.logged_paths().is_empty(), "{:?}", env.logged_paths());
    assert!(env
        .store()
        .files()
        .unwrap()
        .iter()
        .all(|f| f.state == FileState::Indexed));
}

#[test]
fn req010_ac2_update_after_failure_sends_exactly_the_files_to_enrich() {
    let env = Env::maven();
    env.run("build", &[("NEPHILA_MOCK_FAIL_AFTER", "7")]);
    let to_enrich: BTreeSet<String> = env
        .store()
        .files_to_enrich()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(to_enrich.len(), 3);
    env.clear_log();
    env.ok("update");
    assert_eq!(env.logged_paths(), to_enrich);
    assert_eq!(env.log_lines().len(), 3);
}

#[test]
fn req011_failed_update_leaves_failed_file_to_enrich_and_exits_nonzero() {
    let env = Env::maven();
    env.ok("build");
    for f in [ORDER, ORDER_REPO] {
        let mut t = fs::read_to_string(env.repo.join(f)).unwrap();
        t.push_str("\n// x\n");
        env.write(f, &t);
    }
    let out = env.run("update", &[("NEPHILA_MOCK_FAIL_AFTER", "1")]);
    assert!(!out.status.success());
    let store = env.store();
    // Les deux fichiers modifiés sont réanalysés ; l'un d'eux n'a pas pu être enrichi.
    assert!(store
        .files()
        .unwrap()
        .iter()
        .all(|f| f.state == FileState::Indexed));
    assert_eq!(store.count_files_to_enrich().unwrap(), 1);
}

fn mini_ab(env: &Env) {
    env.write("A.java", "class A { void m() { B.foo(); } }\n");
    env.write("B.java", "class B { }\n");
}

#[test]
fn req008_ac2_new_method_in_b_links_unchanged_a() {
    let env = Env::empty();
    mini_ab(&env);
    env.ok("build");
    assert!(!env.has_edge("method:A.m", EdgeKind::Calls, "method:B.foo"));
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
    env.write("B.java", "class B { void foo() {} }\n");
    env.ok("build");
    assert!(env.has_edge("method:A.m", EdgeKind::Calls, "method:B.foo"));
    env.write("B.java", "class B { void bar() {} }\n");
    env.clear_log();
    env.ok("update");
    assert!(!env.has_edge("method:A.m", EdgeKind::Calls, "method:B.foo"));
    let out = env.export_stdout();
    assert!(stdout(&out).contains("B.foo"), "{}", stdout(&out));
    let unresolved = nephila::resolve::unresolved(&env.store()).unwrap();
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
        .command("build", &[("NEPHILA_MOCK_DELAY_MS", "400")])
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
        .command("update", &[("NEPHILA_MOCK_DELAY_MS", "400")])
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
fn req009_ac4_update_on_missing_root_fails_without_creating_nephila() {
    let env = Env::maven();
    let missing = env.repo.join("absent");
    let out = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&missing)
        .arg("update")
        .env("NEPHILA_CONFIG", &env.config)
        .env("NEPHILA_MOCK_DIR", &env.mock)
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
    let db = env.repo.join(".nephila/graph.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }
    let size = fs::metadata(&db).unwrap().len();
    let out = env.run("update", &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("nephila build"), "{}", stderr(&out));
    assert_eq!(fs::metadata(&db).unwrap().len(), size);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 10);
}

#[test]
fn req054_update_refused_key_reported_at_first_call() {
    let env = Env::maven();
    env.ok("build");
    let mut t = fs::read_to_string(env.repo.join(ORDER)).unwrap();
    t.push_str("\n// x\n");
    env.write(ORDER, &t);
    env.clear_log();
    let out = env.run("update", &[("NEPHILA_MOCK_INVALID_KEY", "1")]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("clé d'API refusée"),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("envoi interrompu"),
        "{}",
        stderr(&out)
    );
    assert_eq!(env.log_lines().len(), 1);
}

#[test]
fn req011_ac2_up_to_date_without_llm_config_even_with_files_to_enrich() {
    let env = Env::maven();
    let out = env.run("build", &[NO_CONFIG]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(env.store().count_files_to_enrich().unwrap(), 3);
    let before = env.store().summary().unwrap().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let out = env.run("update", &[NO_CONFIG]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("graphe à jour"), "{}", stdout(&out));
    assert_eq!(env.store().summary().unwrap().unwrap(), before);
}

#[test]
fn req024_ac1_update_without_config_after_a_modification_is_ok_and_not_enriched() {
    let env = Env::maven();
    env.run("build", &[NO_CONFIG]);
    let mut t = fs::read_to_string(env.repo.join(ORDER)).unwrap();
    t.push_str("\n// x\n");
    env.write(ORDER, &t);
    let out = env.run("update", &[NO_CONFIG]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("enrichissement : désactivé"),
        "{}",
        stdout(&out)
    );
    assert!(env.log_lines().is_empty());
    assert!(env
        .store()
        .nodes()
        .unwrap()
        .iter()
        .all(|n| n.summary.is_none()));
}

#[test]
fn req024_ac6_update_after_no_llm_build_sends_each_file_once_without_reanalysis() {
    let env = Env::maven();
    let out = env.command("build", &[]).arg("--no-llm").output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(env.log_lines().is_empty());
    // Un fichier réanalysé voit ses références réinsérées (nouveaux identifiants).
    let max_parse_ref = |env: &Env| -> i64 {
        rusqlite::Connection::open(env.repo.join(".nephila/graph.db"))
            .unwrap()
            .query_row("SELECT MAX(id) FROM refs WHERE origin = 'parse'", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let before = max_parse_ref(&env);
    let out = env.ok("update");
    assert!(!stdout(&out).contains("graphe à jour"));
    let lines = env.log_lines();
    assert_eq!(lines.len(), 10, "{lines:?}");
    assert_eq!(env.logged_paths().len(), 10);
    assert_eq!(
        before,
        max_parse_ref(&env),
        "aucun fichier ne doit être réanalysé"
    );
    assert_eq!(env.store().count_files_to_enrich().unwrap(), 0);
    env.clear_log();
    let out = env.ok("update");
    assert!(stdout(&out).contains("graphe à jour"));
    assert!(env.log_lines().is_empty());
}

#[test]
fn req024_ac7_update_no_llm_after_modification_drops_stale_summaries_only_for_that_file() {
    let env = Env::maven();
    env.ok("build");
    let summary = |id: &str| {
        env.store()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.id == id)
            .and_then(|n| n.summary)
    };
    assert!(summary("type:com.acme.core.OrderService").is_some());
    assert!(summary("type:com.acme.core.OrderRepository").is_some());
    let mut t = fs::read_to_string(env.repo.join(ORDER_SERVICE)).unwrap();
    t.push_str("\n// modifié\n");
    env.write(ORDER_SERVICE, &t);
    env.clear_log();
    let out = env.command("update", &[]).arg("--no-llm").output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(env.log_lines().is_empty());
    assert!(summary("type:com.acme.core.OrderService").is_none());
    assert!(summary("method:com.acme.core.OrderService.place").is_none());
    assert!(summary("type:com.acme.core.OrderRepository").is_some());
    let to_enrich: Vec<String> = env
        .store()
        .files_to_enrich()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(to_enrich, vec![ORDER_SERVICE.to_string()]);
}

#[test]
fn req022_modified_settings_reanalyzes_build_scripts_of_its_tree() {
    let env = Env::empty();
    env.write(
        "settings.gradle",
        "rootProject.name = 'alpha'\ninclude 'api'\n",
    );
    env.write("build.gradle", "dependencies {\n}\n");
    env.write(
        "api/build.gradle",
        "dependencies {\n  implementation project(':')\n}\n",
    );
    env.write("lib/Util.java", "class Util { }\n");
    let out = env.run("build", &[NO_CONFIG]);
    assert!(out.status.success(), "{}", stderr(&out));
    let short = |env: &Env| {
        env.store()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.id == "module::")
            .map(|n| n.short)
    };
    assert_eq!(short(&env).as_deref(), Some("alpha"));
    // Seul le settings change : le build.gradle racine est réanalysé quand même.
    env.write(
        "settings.gradle",
        "rootProject.name = 'beta'\ninclude 'api'\n",
    );
    let out = env.run("update", &[NO_CONFIG]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("graphe à jour"));
    assert_eq!(short(&env).as_deref(), Some("beta"));
    // Suppression du settings : les scripts sont réanalysés, la racine reprend le nom du dossier.
    fs::remove_file(env.repo.join("settings.gradle")).unwrap();
    let out = env.run("update", &[NO_CONFIG]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_ne!(short(&env).as_deref(), Some("beta"));
}

#[test]
fn req008_ac6_removed_dependency_node_disappears_after_update() {
    let env = Env::maven();
    env.ok("build");
    let dep = "dependency:org.slf4j:slf4j-api";
    assert!(env.node_ids().contains(dep));
    let pom = fs::read_to_string(env.repo.join("core/pom.xml")).unwrap();
    let start = pom.find("<dependencies>").unwrap();
    let end = pom.find("</dependencies>").unwrap() + "</dependencies>".len();
    env.write("core/pom.xml", &format!("{}{}", &pom[..start], &pom[end..]));
    env.ok("update");
    assert!(!env.node_ids().contains(dep));
    let out = env.export_stdout();
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(!v["unresolved"].to_string().contains("slf4j"));
}

// ---- REQ-289 / REQ-291 : enrichissement des seuls nœuds manquants (DEC-223) ----

#[allow(dead_code)]
#[path = "support/delta_java.rs"]
mod delta_java;
#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;

/// Dépôt Gradle (`settings.gradle` à la racine, `a/b/build.gradle`) enrichi par un faux serveur
/// `openai` ; les réponses désignent les nœuds par leur numéro dans la liste demandée.
struct GradleEnv {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: fake_server::FakeServer,
}

const SCRIPT: &str = "a/b/build.gradle";

fn numbered_reply(summaries: &[(usize, &str)]) -> (u16, String) {
    let items: Vec<serde_json::Value> = summaries
        .iter()
        .map(|(n, s)| serde_json::json!({"n": n, "summary": s}))
        .collect();
    let text = serde_json::json!({"summaries": items, "concepts": []}).to_string();
    let body = serde_json::json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]});
    (200, body.to_string())
}

impl GradleEnv {
    fn new(replies: Vec<(u16, String)>) -> GradleEnv {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("a/b")).unwrap();
        fs::write(
            repo.join("settings.gradle"),
            "rootProject.name = 'shop'\ninclude 'a:b'\n",
        )
        .unwrap();
        fs::write(
            repo.join(SCRIPT),
            "dependencies {\n    implementation 'org.slf4j:slf4j-api:2.0.9'\n}\n",
        )
        .unwrap();
        let server = fake_server::FakeServer::start(replies);
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\napi_key_env = \"NEPHILA_T396_KEY\"\nconcurrency = 1\nbase_url = \"{}\"\n",
                server.base_url
            ),
        )
        .unwrap();
        GradleEnv {
            _tmp: tmp,
            repo,
            config,
            server,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T396_KEY", "sk-test")
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> (String, String) {
        let o = self.run(args);
        let (out, err) = (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        );
        assert!(o.status.success(), "{args:?} : {err}");
        (out, err)
    }

    /// Enrichissement initial, avant l'ajout de `a/settings.gradle` ; rend les nœuds enrichissables
    /// de `SCRIPT` avec leur résumé, lus dans la base.
    fn first_build(&self) -> Vec<(String, Option<String>)> {
        self.ok(&["build", "--enrich-code", "--batch-files", "1"]);
        assert_eq!(self.posts().len(), 1);
        self.enrichable(SCRIPT)
    }

    /// Ajoute `a/settings.gradle` : mêmes octets pour `SCRIPT`, autres identifiants.
    fn add_nested_settings(&self) {
        fs::write(
            self.repo.join("a/settings.gradle"),
            "rootProject.name = 'a'\n",
        )
        .unwrap();
    }

    fn posts(&self) -> Vec<String> {
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == "/v1/chat/completions")
            .map(|r| r.body)
            .collect()
    }

    /// Nœuds enrichissables d'un fichier (hors `package:` et `key:`) et leur résumé, lus dans
    /// la base, triés.
    fn enrichable(&self, path: &str) -> Vec<(String, Option<String>)> {
        let mut v: Vec<(String, Option<String>)> = Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .filter(|n| n.file.as_deref() == Some(path))
            .filter(|n| !n.id.starts_with("package:") && !n.id.starts_with("key:"))
            .map(|n| (n.id, n.summary))
            .collect();
        v.sort();
        v
    }
}

/// Identifiants listés (`[<n> — <id>]`) dans le prompt d'une requête d'enrichissement.
fn requested_ids(body: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    let prompt = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_str())
        .find(|c| c.contains("Nœuds du fichier :"))
        .unwrap_or_else(|| panic!("aucun prompt d'enrichissement dans {body}"));
    let list = prompt.split("Nœuds du fichier :\n").nth(1).unwrap();
    list.lines()
        .map_while(|l| l.strip_prefix('[').and_then(|r| r.split_once(" — ")))
        .map(|(_, id)| id.to_string())
        .collect()
}

fn structure_end(err: &str) -> &str {
    err.lines()
        .find(|l| l.contains("structure: fin"))
        .unwrap_or_else(|| panic!("aucune ligne structure: fin : {err}"))
}

#[test]
fn req289_ac3_req290_ac7_gradle_nested_settings_counts_discarded_and_leaves_the_script_to_enrich() {
    let env = GradleEnv::new(vec![numbered_reply(&[(1, "premier")])]);
    let before = env.first_build();
    assert!(before.iter().all(|(_, s)| s.is_some()), "{before:?}");
    env.add_nested_settings();
    let (out, err) = env.ok(&["update", "--no-llm", "--verbose"]);
    let after = env.enrichable(SCRIPT);
    // Précondition : l'ajout change les identifiants du script à contenu égal.
    let ids = |v: &[(String, Option<String>)]| v.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>();
    assert_ne!(
        ids(&before),
        ids(&after),
        "l'ajout de a/settings.gradle doit changer des identifiants de {SCRIPT} : choisir un autre script"
    );
    // AC-289-3 : résumés de l'entrée dont le nœud a disparu, lus dans la base.
    let discarded = before
        .iter()
        .filter(|(id, s)| s.is_some() && !after.iter().any(|(a, _)| a == id))
        .count();
    assert!(discarded > 0, "{before:?} -> {after:?}");
    assert!(
        structure_end(&err).ends_with(&format!(", {discarded} résumés écartés")),
        "{err}"
    );
    // AC-289-3 : stdout identique à celui d'un `update` sans `--verbose`.
    let plain = GradleEnv::new(vec![numbered_reply(&[(1, "premier")])]);
    plain.first_build();
    plain.add_nested_settings();
    let (plain_out, plain_err) = plain.ok(&["update", "--no-llm"]);
    assert_eq!(plain_out, out);
    assert!(!plain_err.contains("résumés écartés"), "{plain_err}");
    // AC-290-7 : aucune requête ; `status` compte le script et `a/settings.gradle`, nouveau
    // fichier sans entrée d'enrichissement.
    assert_eq!(env.posts().len(), 1, "aucune requête de l'update");
    let n = 2;
    let (status, _) = env.ok(&["status"]);
    let lines: Vec<&str> = status.lines().collect();
    assert_eq!(lines[0], "graphe à jour", "{status}");
    assert!(
        !lines.iter().any(|l| l.contains(&format!("\t{SCRIPT}"))),
        "{status}"
    );
    assert_eq!(
        *lines.last().unwrap(),
        format!("{n} fichiers à enrichir (résumés et concepts)"),
        "{status}"
    );
    let (json, _) = env.ok(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["to_enrich"], n, "{json}");
}

// ---- REQ-291 : scénario Java, montée de version simulée (seule une partie des identifiants change) ----

/// `p/A.java` et `p/B.java` enrichis par `build` ; `max_chunk_chars` = 200 : `m4` et `m5` seules
/// dans le morceau 2 de `p/A.java`.
struct JavaEnv {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: fake_server::FakeServer,
}

impl JavaEnv {
    /// `delta` : réponses de la requête de rattrapage (`answered` résumés numérotés de 1).
    fn new(answered: usize) -> JavaEnv {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        delta_java::write_repo(&repo, 200);
        let mut replies: Vec<(u16, String)> = (1..=3).map(|k| delta_java::reply(k, 8)).collect();
        replies.push(delta_java::reply(4, answered));
        let server = fake_server::FakeServer::start(replies);
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\napi_key_env = \"NEPHILA_T396_KEY\"\nconcurrency = 1\nbase_url = \"{}\"\n",
                server.base_url
            ),
        )
        .unwrap();
        JavaEnv {
            _tmp: tmp,
            repo,
            config,
            server,
        }
    }

    fn ok(&self, args: &[&str]) -> (String, String) {
        let o = Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T396_KEY", "sk-test")
            .output()
            .unwrap();
        let (out, err) = (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        );
        assert!(o.status.success(), "{args:?} : {err}");
        (out, err)
    }

    fn posts(&self) -> Vec<String> {
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == "/v1/chat/completions")
            .map(|r| r.body)
            .collect()
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.repo.join(".nephila/graph.db")).unwrap()
    }

    /// Nœuds enrichissables de `p/A.java` (déclarations d'analyse sans doc, hors `package:` et
    /// `key:`), triés, lus dans la base.
    fn enrichable(&self) -> Vec<String> {
        let mut st = self
            .db()
            .prepare(
                "SELECT node FROM declarations WHERE file = ?1 AND origin = 'parse' AND doc IS NULL
                 AND node NOT LIKE 'package:%' AND node NOT LIKE 'key:%' ORDER BY node",
            )
            .unwrap()
            .query_map([delta_java::A], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        st.sort();
        st
    }

    /// Résumés de l'entrée de `p/A.java` : (identifiant, texte) et `nodes_hash`.
    fn entry(&self) -> (Vec<(String, String)>, Option<String>) {
        let (payload, hash): (String, Option<String>) = self
            .db()
            .query_row(
                "SELECT payload, nodes_hash FROM enrichments WHERE path = ?1",
                [delta_java::A],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let sums = v["summaries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into()))
            .collect();
        (sums, hash)
    }

    fn node_summary(&self, id: &str) -> Option<String> {
        self.db()
            .query_row("SELECT summary FROM nodes WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// Premier `build` (précondition : 2 requêtes pour `p/A.java`), puis montée de version
    /// simulée. Rend les textes des résumés de l'entrée avant et les nœuds manquants après.
    fn prepared(&self) -> (Vec<(String, String)>, Vec<String>) {
        self.ok(&["build", "--enrich-code", "--batch-files", "1"]);
        let posts = self.posts();
        assert_eq!(posts.len(), 3, "{posts:?}");
        assert_eq!(
            posts
                .iter()
                .filter(|b| requested_ids(b).iter().any(|i| i.contains("p.A")))
                .count(),
            2,
            "précondition : p/A.java part en 2 requêtes"
        );
        let first = self.entry().0;
        delta_java::simulate_upgrade(&self.repo);
        let kept: Vec<String> = self.entry().0.into_iter().map(|(i, _)| i).collect();
        let missing: Vec<String> = self
            .enrichable()
            .into_iter()
            .filter(|i| !kept.contains(i))
            .collect();
        (first, missing)
    }
}

#[test]
fn req291_ac1_ac7_java_delta_asks_only_missing_nodes_and_reports_the_whole_file() {
    let env = JavaEnv::new(8);
    let (first, missing) = env.prepared();
    let t = env.enrichable().len();
    assert_eq!(missing.len(), 2, "{missing:?}");
    assert!(missing.len() < t, "{t}");
    let (out, err) = env.ok(&["build", "--verbose", "--enrich-code", "--batch-files", "1"]);
    // AC-291-1 : une seule requête de plus, pour le morceau 2 de `p/A.java`, liste = nœuds manquants.
    let posts = env.posts();
    assert_eq!(posts.len(), 4, "{err}");
    assert_eq!(requested_ids(&posts[3]), missing);
    assert!(
        !posts[3].contains("p/B.java") && !posts[3].contains("p.B"),
        "{}",
        posts[3]
    );
    // Résumés : `m4` et `m5` de la nouvelle requête, les autres ceux du premier `build`.
    let (sums, hash) = env.entry();
    for id in env.enrichable() {
        let text = env
            .node_summary(&id)
            .unwrap_or_else(|| panic!("{id} sans résumé"));
        if missing.contains(&id) {
            assert_eq!(text, "R4", "{id}");
        } else {
            let old = first.iter().find(|(i, _)| *i == id).unwrap();
            assert_eq!(text, old.1, "{id}");
        }
    }
    assert!(sums.iter().all(|(i, _)| !i.ends_with("(int)")), "{sums:?}");
    let expected = blake3::hash(env.enrichable().join("\n").as_bytes())
        .to_hex()
        .to_string();
    assert_eq!(hash, Some(expected));
    let (status, _) = env.ok(&["status"]);
    assert!(!status.contains("fichiers à enrichir"), "{status}");
    env.ok(&["update", "--enrich-code", "--batch-files", "1"]);
    env.ok(&["build", "--enrich-code", "--batch-files", "1"]);
    assert_eq!(env.posts().len(), 4, "aucune requête de plus");
    // AC-291-7 : bilan du fichier entier, trace des résumés écartés, lignes d'appel.
    assert!(
        out.contains(&format!(", 0 fichiers à enrichir, {t}/{t} nœuds résumés")),
        "{out}"
    );
    assert!(
        structure_end(&err).ends_with(", 2 résumés écartés"),
        "{err}"
    );
    let ks: Vec<usize> = err
        .lines()
        .filter(|l| l.contains(&format!("llm enrich fin {} ", delta_java::A)))
        .map(|l| {
            let tail = l.split(" nœuds résumés)").next().unwrap();
            let seg = tail.rsplit(", ").next().unwrap();
            let (k, m) = seg.split_once('/').unwrap();
            assert_eq!(k, m, "{l}");
            k.parse().unwrap()
        })
        .collect();
    assert_eq!(ks.iter().sum::<usize>(), 2, "{err}");
    assert!(
        !err.contains("delta") && !out.contains("delta"),
        "{err}{out}"
    );
}

#[test]
fn req291_ac7_java_omitted_node_counts_as_tried_and_is_not_asked_again() {
    let env = JavaEnv::new(1);
    let (_, missing) = env.prepared();
    let t = env.enrichable().len();
    let (out, _) = env.ok(&["build", "--enrich-code", "--batch-files", "1"]);
    assert_eq!(env.posts().len(), 4);
    assert!(
        out.contains(&format!(
            ", 0 fichiers à enrichir, {}/{t} nœuds résumés",
            t - 1
        )),
        "{out}"
    );
    assert!(env.node_summary(&missing[1]).is_none(), "m5 omis");
    let (status, _) = env.ok(&["status"]);
    assert!(!status.contains("fichiers à enrichir"), "{status}");
    env.ok(&["build", "--enrich-code", "--batch-files", "1"]);
    assert_eq!(env.posts().len(), 4, "aucune requête de plus");
}
