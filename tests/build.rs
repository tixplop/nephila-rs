//! Commande `rkg build` de bout en bout (REQ-009, REQ-006, REQ-002) avec le LLM simulé.

use assert_cmd::Command;
use rkg::model::{EdgeKind, FileState, NodeKind};
use rkg::store::Store;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

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
    log: PathBuf,
    mock: PathBuf,
}

impl Env {
    /// Dépôt Maven d'exemple copié dans un dossier temporaire, configuration `mock`.
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixtures().join("maven2"), &repo);
        Env::with_repo(tmp, repo)
    }

    fn empty() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        Env::with_repo(tmp, repo)
    }

    fn with_repo(tmp: tempfile::TempDir, repo: PathBuf) -> Env {
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let mock = tmp.path().join("mock");
        copy_dir(&fixtures().join("mock"), &mock);
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            log,
            mock,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = self.raw();
        c.arg("-C").arg(&self.repo);
        c
    }

    /// Commande sans option `-C`.
    fn raw(&self) -> Command {
        let mut c = Command::cargo_bin("rkg").unwrap();
        c.env_remove("RKG_MOCK_FAIL_AFTER")
            .env_remove("RKG_MOCK_INVALID_KEY")
            .env_remove("RKG_MOCK_DELAY_MS")
            .env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log);
        c
    }

    fn build(&self) -> std::process::Output {
        self.cmd().arg("build").output().unwrap()
    }

    fn build_ok(&self) -> std::process::Output {
        let out = self.build();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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

    fn has_edge(&self, s: &str, k: EdgeKind, t: &str) -> bool {
        self.store()
            .edges()
            .unwrap()
            .iter()
            .any(|e| e.source == s && e.kind == k && e.target == t)
    }
}

fn stdout(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn tree(root: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            out.insert(p.strip_prefix(root).unwrap().display().to_string());
            if p.is_dir() {
                walk(&p, root, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

const SOURCES: [&str; 10] = [
    "README.md",
    "docs/archi.md",
    "docs/guide.md",
    "api/pom.xml",
    "core/pom.xml",
    "api/src/main/java/com/acme/api/ApiController.java",
    "core/src/main/java/com/acme/core/OrderService.java",
    "core/src/main/java/com/acme/core/OrderRepository.java",
    "core/src/main/java/com/acme/core/Broken.java",
    "core/src/main/java/com/acme/domain/Order.java",
];

#[test]
fn req009_ac1_graph_has_documents_modules_types_methods() {
    let env = Env::new();
    env.build_ok();
    let nodes = env.store().nodes().unwrap();
    let count = |k: NodeKind| nodes.iter().filter(|n| n.kind == k).count();
    assert_eq!(count(NodeKind::Document), 3);
    assert_eq!(count(NodeKind::Module), 2);
    assert_eq!(count(NodeKind::Type), 5);
    assert_eq!(count(NodeKind::Method), 4);
    for id in [
        "module:api",
        "module:core",
        "type:com.acme.core.OrderService",
        "method:com.acme.core.OrderService.place",
        "document:docs/archi.md",
    ] {
        assert!(nodes.iter().any(|n| n.id == id), "nœud absent : {id}");
    }
    let n = nodes
        .iter()
        .find(|n| n.id == "type:com.acme.core.OrderService")
        .unwrap();
    assert_eq!(n.short, "OrderService");
    assert_eq!(
        nodes
            .iter()
            .find(|n| n.id == "method:com.acme.core.OrderService.place")
            .unwrap()
            .short,
        "place"
    );
}

#[test]
fn req009_ac1_module_contains_package_contains_type() {
    let env = Env::new();
    env.build_ok();
    assert!(env.has_edge("module:core", EdgeKind::Contains, "package:com.acme.core"));
    assert!(env.has_edge(
        "package:com.acme.core",
        EdgeKind::Contains,
        "type:com.acme.core.OrderService"
    ));
    assert!(env.has_edge("module:api", EdgeKind::Contains, "package:com.acme.api"));
}

#[test]
fn req009_ac2_only_rkg_dir_is_written() {
    let env = Env::new();
    let before = tree(&env.repo);
    env.build_ok();
    let after = tree(&env.repo);
    let added: Vec<&String> = after.difference(&before).collect();
    assert!(!added.is_empty());
    for a in added {
        assert!(
            a == ".rkg" || a.starts_with(".rkg/"),
            "écrit hors .rkg : {a}"
        );
    }
    assert!(before.is_subset(&after));
    assert!(env.repo.join(".rkg/graph.db").is_file());
}

#[test]
fn req009_ac3_second_build_resends_every_file_and_drops_deleted_nodes() {
    let env = Env::new();
    env.build_ok();
    assert_eq!(env.log_lines().len(), SOURCES.len());
    fs::remove_file(
        env.repo
            .join("core/src/main/java/com/acme/domain/Order.java"),
    )
    .unwrap();
    fs::remove_file(&env.log).unwrap();
    env.build_ok();
    let lines = env.log_lines();
    assert_eq!(lines.len(), SOURCES.len() - 1);
    let sent: BTreeSet<String> = lines
        .iter()
        .map(|l| l.split('\t').next().unwrap().to_string())
        .collect();
    assert!(sent.contains("README.md"));
    assert!(sent.contains("core/src/main/java/com/acme/core/OrderService.java"));
    assert!(!sent.contains("core/src/main/java/com/acme/domain/Order.java"));
    let store = env.store();
    assert!(!store
        .nodes()
        .unwrap()
        .iter()
        .any(|n| n.id == "type:com.acme.domain.Order"));
    assert!(!store
        .files()
        .unwrap()
        .iter()
        .any(|f| f.path.ends_with("domain/Order.java")));
}

#[test]
fn req009_ac4_missing_repo_fails_without_creating_rkg() {
    let env = Env::new();
    let missing = env.repo.join("nope");
    let out = env
        .raw()
        .args(["-C"])
        .arg(&missing)
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).starts_with("erreur:"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
    assert!(!missing.join(".rkg").exists());
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req009_ac4_repo_is_a_file_fails_without_creating_rkg() {
    let env = Env::new();
    let file = env.repo.join("README.md");
    let out = env
        .raw()
        .args(["-C"])
        .arg(&file)
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).starts_with("erreur:"));
    assert!(!file.join(".rkg").exists());
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req009_ac4_unreadable_repo_fails_without_creating_rkg() {
    use std::os::unix::fs::PermissionsExt;
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if is_root {
        return;
    }
    let env = Env::new();
    fs::set_permissions(&env.repo, fs::Permissions::from_mode(0o300)).unwrap();
    let out = env.build();
    fs::set_permissions(&env.repo, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).starts_with("erreur:"), "{}", stderr(&out));
    assert!(stderr(&out).contains("lisible"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req006_ac2_reference_from_undeclared_source_is_dropped_with_warning() {
    let env = Env::new();
    let svc = "core/src/main/java/com/acme/core/OrderService.java";
    fs::write(
        env.mock.join(format!("{svc}.json")),
        r#"{"version":1,"declares":[{"kind":"type","name":"com.acme.core.OrderService","subkind":"class"}],"references":[{"source":"method:com.acme.core.Ghost.run","kind":"calls","target":"OrderRepository.save"},{"source":"type:com.acme.core.OrderService","kind":"imports","target":"Order"}]}"#,
    )
    .unwrap();
    let out = env.build_ok();
    assert!(stderr(&out).contains("avertissement:"), "{}", stderr(&out));
    assert!(stderr(&out).contains("ignorée"), "{}", stderr(&out));
    let refs = env.store().refs().unwrap();
    assert!(!refs.iter().any(|r| r.source.contains("Ghost")));
    assert!(refs
        .iter()
        .any(|r| r.source == "type:com.acme.core.OrderService" && r.target_name == "Order"));
}

#[test]
fn req009_ac5_empty_repo_gives_empty_graph_and_warning() {
    let env = Env::empty();
    fs::write(env.repo.join("script.py"), "print(1)").unwrap();
    fs::write(env.repo.join("notes.txt"), "x").unwrap();
    let out = env.build_ok();
    assert!(stderr(&out).contains("avertissement:"), "{}", stderr(&out));
    let store = env.store();
    assert!(store.nodes().unwrap().is_empty());
    assert!(store.files().unwrap().is_empty());
    assert!(env.log_lines().is_empty());
}

#[test]
fn req009_ac5_rebuild_on_emptied_repo_clears_previous_graph() {
    let env = Env::new();
    env.build_ok();
    for s in SOURCES {
        fs::remove_file(env.repo.join(s)).unwrap();
    }
    env.build_ok();
    assert!(env.store().nodes().unwrap().is_empty());
}

#[test]
fn req009_ac6_summary_line_on_stdout() {
    let env = Env::new();
    let out = env.build_ok();
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let line = lines[0];
    let store = env.store();
    let n = store.nodes().unwrap().len();
    let e = store.edges().unwrap().len();
    for expected in [
        "10 fichiers traités".to_string(),
        format!("{n} nœuds"),
        format!("{e} liens"),
        "0 références non résolues".to_string(),
        "0 fichiers en attente".to_string(),
    ] {
        assert!(
            line.contains(&expected),
            "« {expected} » absent de : {line}"
        );
    }
}

#[test]
fn req009_ac6_summary_counts_unresolved_references() {
    let env = Env::new();
    fs::write(
        env.mock.join("docs/guide.md.json"),
        r#"{"version":1,"declares":[{"kind":"document","name":"docs/guide.md"},{"kind":"section","name":"docs/guide.md#guide"}],"references":[{"source":"section:docs/guide.md#guide","kind":"mentions","target":"Fantome"}]}"#,
    )
    .unwrap();
    let out = env.build_ok();
    assert!(
        stdout(&out).contains("1 références non résolues"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn req006_ac1_one_call_per_file_one_file_per_call() {
    let env = Env::new();
    env.build_ok();
    let lines = env.log_lines();
    assert_eq!(lines.len(), SOURCES.len());
    let calls: BTreeSet<String> = lines.into_iter().collect();
    let expected: BTreeSet<String> = SOURCES.iter().map(|s| format!("{s}\t1")).collect();
    assert_eq!(calls, expected);
}

#[test]
fn req006_ac1_concurrency_setting_gives_same_graph() {
    let a = Env::new();
    a.build_ok();
    let b = Env::new();
    fs::write(&b.config, "provider = \"mock\"\nconcurrency = 1\n").unwrap();
    b.build_ok();
    let c = Env::new();
    fs::write(&c.config, "provider = \"mock\"\nconcurrency = 8\n").unwrap();
    c.build_ok();
    let key = |e: &Env| {
        let s = e.store();
        let mut ns: Vec<String> = s.nodes().unwrap().iter().map(|n| n.id.clone()).collect();
        ns.sort();
        let mut es: Vec<String> = s
            .edges()
            .unwrap()
            .iter()
            .map(|x| format!("{} {} {}", x.source, x.kind, x.target))
            .collect();
        es.sort();
        (ns, es)
    };
    assert_eq!(key(&a), key(&b));
    assert_eq!(key(&a), key(&c));
}

#[test]
fn req006_ac2_nodes_and_refs_stored_with_origin_file() {
    let env = Env::new();
    env.build_ok();
    let store = env.store();
    let svc = "core/src/main/java/com/acme/core/OrderService.java";
    let nodes = store.nodes().unwrap();
    let t = nodes
        .iter()
        .find(|n| n.id == "type:com.acme.core.OrderService")
        .unwrap();
    assert_eq!(t.file.as_deref(), Some(svc));
    let m = nodes
        .iter()
        .find(|n| n.id == "method:com.acme.core.OrderService.place")
        .unwrap();
    assert_eq!(m.file.as_deref(), Some(svc));
    let decls = store.declarations().unwrap();
    assert!(decls.contains(&(
        svc.to_string(),
        "type:com.acme.core.OrderService".to_string()
    )));
    let r = store
        .refs()
        .unwrap()
        .into_iter()
        .find(|r| r.target_name == "OrderRepository.save")
        .unwrap();
    assert_eq!(r.source, "method:com.acme.core.OrderService.place");
    let ctx = r.context.unwrap();
    assert!(
        ctx.contains("package com.acme.core") && ctx.contains("import com.acme.domain.Order"),
        "{ctx}"
    );
    assert!(store
        .files()
        .unwrap()
        .iter()
        .all(|f| f.state == FileState::Indexed));
}

#[test]
fn req006_ac3_order_service_edges() {
    let env = Env::new();
    env.build_ok();
    assert!(env.has_edge(
        "type:com.acme.core.OrderService",
        EdgeKind::Imports,
        "type:com.acme.domain.Order"
    ));
    assert!(env.has_edge(
        "method:com.acme.core.OrderService.place",
        EdgeKind::Calls,
        "method:com.acme.core.OrderRepository.save"
    ));
    assert!(env.has_edge(
        "method:com.acme.api.ApiController.create",
        EdgeKind::Calls,
        "method:com.acme.core.OrderService.place"
    ));
}

#[test]
fn req006_ac4_markdown_mentions_type() {
    let env = Env::new();
    env.build_ok();
    assert!(env.has_edge(
        "section:docs/archi.md#architecture",
        EdgeKind::Mentions,
        "type:com.acme.core.OrderService"
    ));
    assert!(env.has_edge(
        "document:docs/archi.md",
        EdgeKind::Contains,
        "section:docs/archi.md#architecture"
    ));
    assert!(env.has_edge(
        "document:docs/archi.md",
        EdgeKind::LinksTo,
        "document:docs/guide.md"
    ));
}

#[test]
fn req006_ac5_module_dependencies() {
    let env = Env::new();
    env.build_ok();
    assert!(env.has_edge("module:api", EdgeKind::DependsOn, "module:core"));
    let nodes = env.store().nodes().unwrap();
    let dep = nodes
        .iter()
        .find(|n| n.id == "dependency:org.slf4j:slf4j-api")
        .unwrap();
    assert_eq!(dep.kind, NodeKind::Dependency);
    assert_eq!(dep.short, "slf4j-api");
    assert!(env.has_edge(
        "module:core",
        EdgeKind::DependsOn,
        "dependency:org.slf4j:slf4j-api"
    ));
}

#[test]
fn req006_ac6_broken_file_is_processed_like_others() {
    let env = Env::new();
    let out = env.build_ok();
    assert!(out.status.success());
    let broken = "core/src/main/java/com/acme/core/Broken.java";
    assert!(env.log_lines().iter().any(|l| l.starts_with(broken)));
    let nodes = env.store().nodes().unwrap();
    assert!(nodes.iter().any(|n| n.id == "type:com.acme.core.Broken"));
    assert!(nodes
        .iter()
        .any(|n| n.id == "method:com.acme.core.Broken.oops"));
}

#[test]
fn req006_ac7_invalid_response_leaves_file_pending_and_graph_intact() {
    let env = Env::new();
    let bad = "core/src/main/java/com/acme/core/Broken.java";
    fs::write(
        env.mock.join(format!("{bad}.json")),
        r#"{"version":1,"declares":[{"kind":"bogus","name":"X"}],"references":[]}"#,
    )
    .unwrap();
    fs::write(&env.config, "provider = \"mock\"\nconcurrency = 1\n").unwrap();
    let out = env.build();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("erreur:"), "{}", stderr(&out));
    assert!(stdout(&out).contains("fichiers en attente"));
    let store = env.store();
    let files = store.files().unwrap();
    let f = files.iter().find(|f| f.path == bad).unwrap();
    assert_eq!(f.state, FileState::Pending);
    let nodes = store.nodes().unwrap();
    assert!(!nodes.iter().any(|n| n.id.contains("Broken")));
    // Le graphe reste lisible : les fichiers traités avant l'échec sont indexés.
    assert!(files.iter().any(|f| f.state == FileState::Indexed));
    assert_eq!(files.len(), SOURCES.len());
}

#[test]
fn req006_ac7_unreadable_json_is_also_a_pending_file() {
    let env = Env::new();
    let bad = "README.md";
    fs::write(env.mock.join("README.md.json"), "pas du json").unwrap();
    fs::write(&env.config, "provider = \"mock\"\nconcurrency = 1\n").unwrap();
    let out = env.build();
    assert!(!out.status.success());
    let files = env.store().files().unwrap();
    assert_eq!(
        files.iter().find(|f| f.path == bad).unwrap().state,
        FileState::Pending
    );
}

#[test]
fn req002_ac1_missing_config_fails_before_scan_and_leaves_rkg_alone() {
    let env = Env::new();
    let missing = env.repo.join("..").join("absent.toml");
    // Sans .rkg : rien n'est créé.
    let out = env
        .cmd()
        .env("RKG_CONFIG", &missing)
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.starts_with("erreur:"), "{err}");
    assert!(err.contains("absent.toml"));
    for key in ["provider", "model", "api_key_env"] {
        assert!(err.contains(key), "{key} absent de : {err}");
    }
    assert!(stdout(&out).is_empty());
    assert!(!env.repo.join(".rkg").exists());
    assert!(env.log_lines().is_empty());
    // Avec un .rkg existant : ni modifié ni touché.
    env.build_ok();
    let snapshot = snapshot_rkg(&env.repo);
    let out = env
        .cmd()
        .env("RKG_CONFIG", &missing)
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(snapshot, snapshot_rkg(&env.repo));
}

fn snapshot_rkg(repo: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    let mut v = Vec::new();
    for e in fs::read_dir(repo.join(".rkg")).unwrap() {
        let p = e.unwrap().path();
        v.push((
            p.file_name().unwrap().to_string_lossy().into_owned(),
            fs::read(&p).unwrap(),
            fs::metadata(&p).unwrap().modified().unwrap(),
        ));
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn req002_ac2_unknown_provider_names_faulty_key() {
    let env = Env::new();
    fs::write(&env.config, "provider = \"gpt\"\nmodel = \"x\"\n").unwrap();
    let out = env.build();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("provider"), "{}", stderr(&out));
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req002_ac2_missing_required_key_names_it() {
    let env = Env::new();
    fs::write(&env.config, "provider = \"anthropic\"\nmodel = \"x\"\n").unwrap();
    let out = env.build();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("api_key_env"), "{}", stderr(&out));
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req002_ac3_refused_key_stops_before_any_extraction() {
    let env = Env::new();
    let out = env
        .cmd()
        .env("RKG_MOCK_INVALID_KEY", "1")
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).starts_with("erreur:"));
    assert!(env.log_lines().is_empty());
    assert!(!env.repo.join(".rkg").exists());
    // Graphe existant inchangé.
    env.build_ok();
    let snapshot = snapshot_rkg(&env.repo);
    let out = env
        .cmd()
        .env("RKG_MOCK_INVALID_KEY", "1")
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(snapshot, snapshot_rkg(&env.repo));
}

#[test]
fn req002_ac4_missing_key_variable_is_named() {
    let env = Env::new();
    fs::write(
        &env.config,
        "provider = \"mock\"\napi_key_env = \"RKG_TEST_ABSENT_KEY_VAR\"\n",
    )
    .unwrap();
    let out = env
        .cmd()
        .env_remove("RKG_TEST_ABSENT_KEY_VAR")
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("RKG_TEST_ABSENT_KEY_VAR"),
        "{}",
        stderr(&out)
    );
    assert!(!env.repo.join(".rkg").exists());
    assert!(env.log_lines().is_empty());
}

#[test]
fn req009_ordering_invalid_repo_reported_before_config_error() {
    let env = Env::new();
    let out = env
        .raw()
        .env("RKG_CONFIG", "/nonexistent/c.toml")
        .args(["-C", "/nonexistent/repo"])
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("/nonexistent/repo"),
        "{}",
        stderr(&out)
    );
}
