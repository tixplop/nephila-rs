//! Commande `nephila build` de bout en bout (REQ-009, REQ-006, REQ-002, REQ-022, REQ-023, REQ-024).

use assert_cmd::Command;
use nephila::model::{EdgeKind, FileState, NodeKind};
use nephila::store::Store;
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
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
        )
        .unwrap();
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
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        c
    }

    /// Commande sans fichier de configuration LLM (LLM inactif).
    fn plain(&self) -> Command {
        let mut c = self.cmd();
        c.env("NEPHILA_CONFIG", self.config.with_file_name("absent.toml"));
        c
    }

    fn build_plain(&self) -> std::process::Output {
        self.plain().arg("build").output().unwrap()
    }

    fn build_plain_ok(&self) -> std::process::Output {
        let out = self.build_plain();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
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

    fn node(&self, id: &str) -> nephila::model::Node {
        self.store()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("nœud absent : {id}"))
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
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

const SVC: &str = "core/src/main/java/com/acme/core/OrderService.java";

/// Nœuds et liens de structure (sans concepts), triés : comparables d'un build à l'autre.
fn structure_key(e: &Env) -> (Vec<String>, Vec<String>) {
    let s = e.store();
    let mut ns: Vec<String> = s
        .nodes()
        .unwrap()
        .iter()
        .filter(|n| n.kind != NodeKind::Concept)
        .map(|n| n.id.clone())
        .collect();
    ns.sort();
    let mut es: Vec<String> = s
        .edges()
        .unwrap()
        .iter()
        .filter(|x| !x.target.starts_with("concept:"))
        .map(|x| format!("{} {} {}", x.source, x.kind, x.target))
        .collect();
    es.sort();
    (ns, es)
}

#[test]
fn req009_ac1_graph_has_documents_modules_types_methods_without_config() {
    let env = Env::new();
    let out = env.build_plain_ok();
    assert!(env.log_lines().is_empty());
    assert!(
        stdout(&out).contains("enrichissement : désactivé"),
        "{}",
        stdout(&out)
    );
    let nodes = env.store().nodes().unwrap();
    let count = |k: NodeKind| nodes.iter().filter(|n| n.kind == k).count();
    assert_eq!(count(NodeKind::Document), 3);
    assert_eq!(count(NodeKind::Module), 2);
    assert_eq!(count(NodeKind::Type), 5);
    assert_eq!(count(NodeKind::Method), 4);
    assert_eq!(count(NodeKind::Package), 3);
    for id in [
        "module:com.acme:api",
        "module:com.acme:core",
        "type:com.acme.core.OrderService",
        "method:com.acme.core.OrderService.place",
        "document:docs/archi.md",
    ] {
        assert!(nodes.iter().any(|n| n.id == id), "nœud absent : {id}");
    }
    assert_eq!(
        env.node("type:com.acme.core.OrderService").short,
        "OrderService"
    );
    assert_eq!(
        env.node("method:com.acme.core.OrderService.place").short,
        "place"
    );
    assert!(nodes.iter().all(|n| n.summary.is_none()));
}

#[test]
fn req009_ac1_module_contains_package_contains_type() {
    let env = Env::new();
    env.build_plain_ok();
    assert!(env.has_edge(
        "module:com.acme:core",
        EdgeKind::Contains,
        "package:com.acme.core"
    ));
    assert!(env.has_edge(
        "package:com.acme.core",
        EdgeKind::Contains,
        "type:com.acme.core.OrderService"
    ));
    assert!(env.has_edge(
        "module:com.acme:api",
        EdgeKind::Contains,
        "package:com.acme.api"
    ));
}

#[test]
fn req009_ac2_only_nephila_dir_is_written() {
    let env = Env::new();
    let before = tree(&env.repo);
    env.build_ok();
    let after = tree(&env.repo);
    let added: Vec<&String> = after.difference(&before).collect();
    assert!(!added.is_empty());
    for a in added {
        assert!(
            a == ".nephila" || a.starts_with(".nephila/"),
            "écrit hors .nephila : {a}"
        );
    }
    assert!(before.is_subset(&after));
    assert!(env.repo.join(".nephila/graph.db").is_file());
}

#[test]
fn req009_ac3_second_build_reanalyzes_everything_resends_nothing_and_drops_deleted_nodes() {
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
    // AC-009-3 modifié (REQ-049) : tout est réanalysé, seuls les fichiers sans enrichissement
    // conservé à jour sont envoyés ; ici, aucun.
    assert!(env.log_lines().is_empty(), "{:?}", env.log_lines());
    let store = env.store();
    assert_eq!(store.count_files_to_enrich().unwrap(), 0);
    assert!(store
        .nodes()
        .unwrap()
        .iter()
        .any(|n| n.id.contains("OrderService") && n.summary.is_some()));
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
fn req009_ac4_missing_repo_fails_without_creating_nephila() {
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
    assert!(!missing.join(".nephila").exists());
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req009_ac4_repo_is_a_file_fails_without_creating_nephila() {
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
    assert!(!file.join(".nephila").exists());
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req009_ac4_unreadable_repo_fails_without_creating_nephila() {
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
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req024_ac4_discarded_items_give_one_warning_per_file() {
    for concurrency in [1, 4] {
        let env = Env::new();
        // Un résumé hors liste et un concept dans la réponse d'un .java.
        fs::write(
            env.mock.join(format!("{SVC}.json")),
            r#"{"version":2,"summaries":[{"node":"type:com.acme.core.OrderService","summary":"Service."},{"node":"type:com.acme.Inexistant","summary":"x"}],"concepts":[{"name":"Intrus","sections":["section:docs/guide.md#guide"]}]}"#,
        )
        .unwrap();
        // Un concept citant une section d'un autre fichier.
        fs::write(
            env.mock.join("docs/guide.md.json"),
            r#"{"version":2,"summaries":[],"concepts":[{"name":"Facturation","sections":["section:docs/archi.md#architecture"]}]}"#,
        )
        .unwrap();
        fs::write(
            &env.config,
            format!("provider = \"mock\"\nenrich_code = true\nconcurrency = {concurrency}\n"),
        )
        .unwrap();
        let out = env.build_ok();
        let err = stderr(&out);
        let warns_of = |file: &str| -> Vec<String> {
            err.lines()
                .filter(|l| l.starts_with("avertissement:") && l.contains(file))
                .map(str::to_string)
                .collect()
        };
        let w = warns_of("OrderService.java");
        assert_eq!(w.len(), 1, "{err}");
        assert!(w[0].contains("2 élément(s)"), "{err}");
        let w = warns_of("docs/guide.md");
        assert_eq!(w.len(), 1, "{err}");
        assert!(w[0].contains("1 élément(s)"), "{err}");
        let store = env.store();
        assert!(store
            .nodes()
            .unwrap()
            .iter()
            .all(|n| n.kind != NodeKind::Concept));
        assert_eq!(
            env.node("type:com.acme.core.OrderService")
                .summary
                .as_deref(),
            Some("Service.")
        );
        assert_eq!(store.count_files_to_enrich().unwrap(), 0);
    }
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
    let u = nephila::resolve::unresolved(&store).unwrap().len();
    assert_eq!(u, 1);
    for expected in [
        "10 fichiers traités".to_string(),
        format!("{n} nœuds"),
        format!("{e} liens"),
        // `absent.md` (docs/archi.md) est le seul lien non résolu du dépôt exemple.
        format!("{u} références non résolues"),
        "0 fichiers en attente".to_string(),
        "0 fichiers à enrichir".to_string(),
    ] {
        assert!(
            line.contains(&expected),
            "« {expected} » absent de : {line}"
        );
    }
    assert!(!line.contains("désactivé"), "{line}");
}

#[test]
fn req009_ac6_line_without_llm_counts_files_to_enrich_and_says_disabled() {
    let env = Env::new();
    let out = env.build_plain_ok();
    let line = stdout(&out);
    assert!(line.contains("3 fichiers à enrichir"), "{line}");
    assert!(line.contains("enrichissement : désactivé"), "{line}");
}

#[test]
fn req009_ac6_summary_counts_unresolved_references() {
    let env = Env::new();
    env.write("docs/guide.md", "# Guide\n\nVoir [fantôme](Fantome.md).\n");
    let out = env.build_ok();
    assert!(
        stdout(&out).contains("2 références non résolues"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn req006_ac1_no_config_gives_structure_and_no_llm_with_config_gives_same() {
    let a = Env::new();
    a.build_plain_ok();
    assert!(a.log_lines().is_empty());
    let b = Env::new();
    let out = b.cmd().args(["build", "--no-llm"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(b.log_lines().is_empty());
    assert_eq!(structure_key(&a), structure_key(&b));
    // Chaque fichier JVM a ses nœuds dans le graphe.
    let nodes = a.store().nodes().unwrap();
    for t in [
        "ApiController",
        "OrderService",
        "OrderRepository",
        "Broken",
        "Order",
    ] {
        assert!(
            nodes
                .iter()
                .any(|n| n.kind == NodeKind::Type && n.short == t),
            "type {t} absent"
        );
    }
}

#[test]
fn req024_ac3_one_call_per_file_one_file_per_call() {
    let env = Env::new();
    env.build_ok();
    let lines = env.log_lines();
    assert_eq!(lines.len(), SOURCES.len());
    let calls: BTreeSet<String> = lines.into_iter().collect();
    let expected: BTreeSet<String> = SOURCES.iter().map(|s| format!("{s}\t1")).collect();
    assert_eq!(calls, expected);
}

#[test]
fn req024_ac3_summaries_concepts_visible_and_structure_unchanged() {
    let plain = Env::new();
    plain.build_plain_ok();
    let env = Env::new();
    env.build_ok();
    assert_eq!(structure_key(&plain), structure_key(&env));
    assert_eq!(
        env.node("type:com.acme.core.OrderService")
            .summary
            .as_deref(),
        Some("Place les commandes.")
    );
    let out = env
        .cmd()
        .args(["query", "search", "commandes"])
        .output()
        .unwrap();
    assert!(
        stdout(&out).contains("OrderService"),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    let ex = env
        .cmd()
        .args(["export", "--output", "-"])
        .output()
        .unwrap();
    assert!(stdout(&ex).contains("Place les commandes."));
    let c = env.node("concept:facturation");
    assert_eq!(c.kind, NodeKind::Concept);
    assert!(c.file.is_none());
    assert!(env.has_edge(
        "section:docs/guide.md#guide",
        EdgeKind::Mentions,
        "concept:facturation"
    ));
}

#[test]
fn req024_ac1_ac2_no_llm_option_ignores_the_configuration() {
    let reference = Env::new();
    reference.build_plain_ok();
    for config in [
        "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
        "provider = \"inconnu\"\n",
    ] {
        let env = Env::new();
        fs::write(&env.config, config).unwrap();
        let out = env.cmd().args(["build", "--no-llm"]).output().unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(env.log_lines().is_empty());
        assert!(stdout(&out).contains("enrichissement : désactivé"));
        assert_eq!(structure_key(&reference), structure_key(&env));
    }
}

#[test]
fn req006_ac1_concurrency_setting_gives_same_graph() {
    let a = Env::new();
    a.build_ok();
    let b = Env::new();
    fs::write(
        &b.config,
        "provider = \"mock\"\nenrich_code = true\nconcurrency = 1\n",
    )
    .unwrap();
    b.build_ok();
    let c = Env::new();
    fs::write(
        &c.config,
        "provider = \"mock\"\nenrich_code = true\nconcurrency = 8\n",
    )
    .unwrap();
    c.build_ok();
    assert_eq!(structure_key(&a), structure_key(&b));
    assert_eq!(structure_key(&a), structure_key(&c));
}

#[test]
fn req006_ac2_nodes_and_refs_stored_with_origin_file() {
    let env = Env::new();
    env.build_plain_ok();
    let store = env.store();
    let nodes = store.nodes().unwrap();
    assert_eq!(
        env.node("type:com.acme.core.OrderService").file.as_deref(),
        Some(SVC)
    );
    assert_eq!(
        env.node("method:com.acme.core.OrderService.place")
            .file
            .as_deref(),
        Some(SVC)
    );
    // Tout nœud type a pour fichier d'origine un fichier source existant du dépôt.
    let types: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
    assert!(!types.is_empty());
    for t in types {
        let f = t.file.as_deref().expect("fichier d'origine");
        assert!(env.repo.join(f).is_file(), "{f} absent du dépôt");
        let ext = Path::new(f).extension().and_then(|e| e.to_str()).unwrap();
        assert!(
            ["java", "kt", "kts", "groovy", "scala"].contains(&ext),
            "{f}"
        );
    }
    let decls = store.declarations().unwrap();
    assert!(decls.contains(&(
        SVC.to_string(),
        "type:com.acme.core.OrderService".to_string()
    )));
    let r = store
        .refs()
        .unwrap()
        .into_iter()
        .find(|r| r.target_name.ends_with("save") && r.source.contains("OrderService.place"))
        .expect("référence calls de place");
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
    env.build_plain_ok();
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
fn req023_ac3_markdown_mentions_type_and_method() {
    let env = Env::new();
    env.build_plain_ok();
    assert!(env.has_edge(
        "section:docs/archi.md#architecture",
        EdgeKind::Mentions,
        "type:com.acme.core.OrderService"
    ));
    assert!(env.has_edge(
        "section:docs/guide.md#guide",
        EdgeKind::Mentions,
        "method:com.acme.core.OrderService.place"
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
fn req023_ac1_ac2_ac3_sections_links_and_method_mentions_end_to_end() {
    let env = Env::new();
    env.build_plain_ok();
    for s in ["architecture", "détails"] {
        let id = format!("section:docs/archi.md#{s}");
        assert_eq!(env.node(&id).kind, NodeKind::Section);
        assert!(env.has_edge("document:docs/archi.md", EdgeKind::Contains, &id));
    }
    assert!(env.has_edge(
        "document:docs/archi.md",
        EdgeKind::LinksTo,
        "document:README.md"
    ));
    let links: Vec<_> = env
        .store()
        .edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EdgeKind::LinksTo && e.source == "document:docs/archi.md")
        .collect();
    assert_eq!(links.len(), 2, "{links:?}");
    assert!(env.has_edge(
        "section:docs/archi.md#détails",
        EdgeKind::Mentions,
        "method:com.acme.core.OrderRepository.save"
    ));
    let mentions = env
        .store()
        .edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EdgeKind::Mentions && e.source == "section:docs/archi.md#détails")
        .count();
    assert_eq!(mentions, 1);
    let ex = env
        .cmd()
        .args(["export", "--output", "-"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout(&ex)).unwrap();
    let unresolved = v["unresolved"].as_array().unwrap();
    assert!(
        unresolved
            .iter()
            .any(|u| u.to_string().contains("absent.md")),
        "{unresolved:?}"
    );
    assert!(
        !unresolved.iter().any(|u| u.to_string().contains("x.org")),
        "{unresolved:?}"
    );
}

#[test]
fn req022_ac1_ac2_module_dependencies_and_short_name_in_export() {
    let env = Env::new();
    env.build_plain_ok();
    assert!(env.has_edge(
        "module:com.acme:api",
        EdgeKind::DependsOn,
        "module:com.acme:core"
    ));
    let dep = env.node("dependency:org.slf4j:slf4j-api");
    assert_eq!(dep.kind, NodeKind::Dependency);
    assert_eq!(dep.short, "slf4j-api");
    assert!(env.has_edge(
        "module:com.acme:core",
        EdgeKind::DependsOn,
        "dependency:org.slf4j:slf4j-api"
    ));
    let ex = env
        .cmd()
        .args(["export", "--output", "-"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout(&ex)).unwrap();
    let api = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "module:com.acme:api")
        .unwrap();
    assert_eq!(api["short"], "api");
    assert_eq!(api["file"], "api/pom.xml");
}

#[test]
fn req022_ac7_truncated_pom_is_read_with_a_warning_and_is_not_pending() {
    let env = Env::new();
    env.write(
        "core/pom.xml",
        "<project>\n  <groupId>com.acme</groupId>\n  <artifactId>core</artifactId>\n  <dependencies>\n    <dependency>\n      <groupId>org.slf4j</groupId>\n      <artifactId>slf4j-api</artifactId>\n    </dependency>\n  </dependencies>\n",
    );
    let out = env.build_plain_ok();
    let err = stderr(&out);
    let warns: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("avertissement:") && l.contains("core/pom.xml"))
        .collect();
    assert_eq!(warns.len(), 1, "{err}");
    env.node("module:com.acme:core");
    assert!(env.has_edge(
        "module:com.acme:core",
        EdgeKind::DependsOn,
        "dependency:org.slf4j:slf4j-api"
    ));
    assert!(env
        .store()
        .files()
        .unwrap()
        .iter()
        .all(|f| f.state == FileState::Indexed));
}

#[test]
fn req022_gradle_settings_table_reaches_the_parser() {
    let env = Env::empty();
    env.write(
        "settings.gradle",
        "rootProject.name = 'shop'\ninclude 'api'\n",
    );
    env.write(
        "build.gradle",
        "dependencies {\n  implementation 'org.slf4j:slf4j-api:2.0.9'\n}\n",
    );
    env.write(
        "api/build.gradle",
        "dependencies {\n  implementation project(':')\n}\n",
    );
    env.build_plain_ok();
    let root = env.node("module::");
    assert_eq!(root.short, "shop");
    env.node("module::api");
    assert!(env.has_edge(
        "module::",
        EdgeKind::DependsOn,
        "dependency:org.slf4j:slf4j-api"
    ));
}

#[test]
fn req006_ac6_broken_file_is_processed_like_others() {
    let env = Env::new();
    let out = env.build_plain_ok();
    assert!(out.status.success());
    let nodes = env.store().nodes().unwrap();
    assert!(nodes.iter().any(|n| n.id == "type:com.acme.core.Broken"));
    assert!(nodes
        .iter()
        .any(|n| n.id == "method:com.acme.core.Broken.oops"));
    assert!(env
        .store()
        .files()
        .unwrap()
        .iter()
        .all(|f| f.state == FileState::Indexed));
}

#[test]
fn req024_ac5_invalid_response_keeps_structure_and_leaves_file_to_enrich() {
    for (concurrency, bad_json) in [
        (1, "pas du json"),
        (4, "pas du json"),
        (1, r#"{"version":1,"declares":[],"references":[]}"#),
        (4, r#"{"version":1,"declares":[],"references":[]}"#),
    ] {
        let env = Env::new();
        let bad = "core/src/main/java/com/acme/core/Broken.java";
        fs::create_dir_all(env.mock.join(bad).parent().unwrap()).unwrap();
        fs::write(env.mock.join(format!("{bad}.json")), bad_json).unwrap();
        fs::write(
            &env.config,
            format!("provider = \"mock\"\nenrich_code = true\nconcurrency = {concurrency}\nformat_retries = 0\n"),
        )
        .unwrap();
        let out = env.build();
        assert!(!out.status.success());
        assert!(stderr(&out).contains("erreur:"), "{}", stderr(&out));
        assert!(stderr(&out).contains(bad), "{}", stderr(&out));
        assert!(
            stdout(&out).contains("1 fichiers à enrichir"),
            "{}",
            stdout(&out)
        );
        let store = env.store();
        let files = store.files().unwrap();
        assert!(files.iter().all(|f| f.state == FileState::Indexed));
        let to_enrich: Vec<String> = store
            .files_to_enrich()
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(to_enrich, vec![bad.to_string()]);
        // La structure du fichier en échec est là, ses voisins sont enrichis.
        assert!(store
            .nodes()
            .unwrap()
            .iter()
            .any(|n| n.id == "type:com.acme.core.Broken"));
        assert!(env
            .node("type:com.acme.core.OrderService")
            .summary
            .is_some());
        assert_eq!(env.log_lines().len(), files.len());
    }
}

#[test]
fn req002_ac1_missing_config_builds_without_llm_and_ask_names_the_path() {
    let env = Env::new();
    let missing = env.config.with_file_name("absent.toml");
    let out = env.build_plain_ok();
    assert!(stdout(&out).contains("enrichissement : désactivé"));
    assert!(env.log_lines().is_empty());
    let out = env
        .plain()
        .args(["query", "ask", "que fait OrderService ?"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.starts_with("erreur:"), "{err}");
    assert!(err.contains(&missing.display().to_string()), "{err}");
    for key in ["provider", "model", "api_key_env"] {
        assert!(err.contains(key), "{key} absent de : {err}");
    }
}

#[test]
fn req002_ac2_unknown_provider_names_faulty_key_unless_no_llm() {
    let env = Env::new();
    fs::write(&env.config, "provider = \"gpt\"\nmodel = \"x\"\n").unwrap();
    let out = env.build();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("provider"), "{}", stderr(&out));
    assert!(!env.repo.join(".nephila").exists());
    let out = env.cmd().args(["build", "--no-llm"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn req002_ac2_missing_required_key_names_it() {
    let env = Env::new();
    fs::write(
        &env.config,
        "provider = \"anthropic\"\nenrich_code = true\nmodel = \"x\"\n",
    )
    .unwrap();
    let out = env.build();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("api_key_env"), "{}", stderr(&out));
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req002_ac4_missing_key_variable_is_named() {
    let env = Env::new();
    fs::write(
        &env.config,
        "provider = \"mock\"\nenrich_code = true\napi_key_env = \"NEPHILA_TEST_ABSENT_KEY_VAR\"\n",
    )
    .unwrap();
    let out = env
        .cmd()
        .env_remove("NEPHILA_TEST_ABSENT_KEY_VAR")
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("NEPHILA_TEST_ABSENT_KEY_VAR"),
        "{}",
        stderr(&out)
    );
    assert!(!env.repo.join(".nephila").exists());
    assert!(env.log_lines().is_empty());
}

#[test]
fn req009_ordering_invalid_repo_reported_before_config_error() {
    let env = Env::new();
    fs::write(&env.config, "provider = \"gpt\"\n").unwrap();
    let out = env
        .raw()
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

fn full_graph(e: &Env) -> (Vec<String>, Vec<String>) {
    let s = e.store();
    let mut ns: Vec<String> = s
        .nodes()
        .unwrap()
        .iter()
        .map(|n| format!("{n:?}"))
        .collect();
    ns.sort();
    let mut es: Vec<String> = s
        .edges()
        .unwrap()
        .iter()
        .map(|x| format!("{x:?}"))
        .collect();
    es.sort();
    (ns, es)
}

const DUP_A: &str = "a/src/main/java/com/acme/Dup.java";
const DUP_B: &str = "b/src/main/java/com/acme/Dup.java";

fn dup_env(concurrency: usize, with_a: bool) -> Env {
    let e = Env::empty();
    fs::write(
        &e.config,
        format!("provider = \"mock\"\nenrich_code = true\nconcurrency = {concurrency}\n"),
    )
    .unwrap();
    if with_a {
        e.write(DUP_A, "package com.acme;\npublic class Dup {}\n");
    }
    e.write(DUP_B, "package com.acme;\npublic interface Dup {}\n");
    e
}

#[test]
fn req009_ac8_shared_node_row_is_deterministic_and_follows_declarations() {
    let one = dup_env(1, true);
    one.build_ok();
    let four = dup_env(4, true);
    four.build_ok();
    let n = one.node("type:com.acme.Dup");
    assert_eq!(n.subkind, Some(nephila::model::SubKind::Class));
    assert_eq!(n.file.as_deref(), Some(DUP_A));
    let dups = |e: &Env| {
        e.store()
            .nodes()
            .unwrap()
            .iter()
            .filter(|n| n.id == "type:com.acme.Dup")
            .count()
    };
    assert_eq!(dups(&one), 1);
    assert_eq!(full_graph(&one), full_graph(&four));

    fs::remove_file(one.repo.join(DUP_A)).unwrap();
    let out = one.cmd().arg("update").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let n = one.node("type:com.acme.Dup");
    assert_eq!(n.subkind, Some(nephila::model::SubKind::Interface));
    assert_eq!(n.file.as_deref(), Some(DUP_B));
    let fresh = dup_env(1, false);
    fresh.build_ok();
    assert_eq!(full_graph(&one), full_graph(&fresh));
}
