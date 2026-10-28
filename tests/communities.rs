//! Communautés calculées et stockées par chaque passe qui écrit le résumé (REQ-147, REQ-146, DEC-120).

mod support;

use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

const SERVICE: &str = "core/src/main/java/com/acme/core/OrderService.java";
const HELPER: &str = "core/src/main/java/com/acme/core/Helper.java";
const HELPER_SRC: &str =
    "package com.acme.core;\n\npublic class Helper {\n    public void run() {\n    }\n}\n";
const MOCK_CONFIG: &str = "provider = \"mock\"\nmodel = \"mock\"\n";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: Option<PathBuf>,
}

/// Contenu des tables des communautés : lignes de `communities` (cohésion en bits) et couples
/// (nœud, communauté).
type Tables = (Vec<(i64, String, String, i64, u64)>, Vec<(String, i64)>);

impl Env {
    fn new(repo: &str, mock: Option<&str>, config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let dir = tmp.path().join("repo");
        support::copy_dir(&fx.join(repo), &dir);
        let mock = mock.map(|m| {
            let to = tmp.path().join("mock");
            support::copy_dir(&fx.join(m), &to);
            to
        });
        let cfg = tmp.path().join("config.toml");
        fs::write(&cfg, config).unwrap();
        Env {
            _tmp: tmp,
            repo: dir,
            config: cfg,
            mock,
        }
    }

    fn maven3() -> Env {
        Env::new("maven3/repo", None, MOCK_CONFIG)
    }

    fn run(&self, args: &[&str]) -> Output {
        support::run(&self.repo, &self.config, self.mock.as_deref(), args)
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "{args:?} : {}",
            String::from_utf8_lossy(&o.stderr)
        );
        o
    }

    fn db(&self) -> Connection {
        Connection::open(self.repo.join(".nephila/graph.db")).unwrap()
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn tables(&self) -> Tables {
        let db = self.db();
        let rows = db
            .prepare("SELECT id, label, pivot, size, cohesion FROM communities ORDER BY id")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, f64>(4)?.to_bits(),
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let members = db
            .prepare("SELECT node, community FROM node_communities ORDER BY node")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        (rows, members)
    }

    fn node_ids(&self) -> Vec<String> {
        self.db()
            .prepare("SELECT id FROM nodes ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn summary_at(&self) -> i64 {
        self.db()
            .query_row("SELECT computed_at FROM summary", [], |r| r.get(0))
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.ok(args).stdout).unwrap()
    }

    /// Marque le graphe `à résoudre` : le prochain `update` fait une passe complète.
    fn mark_unresolved(&self) {
        self.db()
            .execute(
                "UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'",
                [],
            )
            .unwrap();
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Ligne `résolution:` de la passe, puis la ligne qui la suit.
fn resolution_and_next(o: &Output) -> (String, String) {
    let text = stderr(o);
    let lines: Vec<&str> = text.lines().collect();
    let i = lines
        .iter()
        .position(|l| l.contains("résolution: "))
        .unwrap_or_else(|| panic!("pas de ligne résolution : {text}"));
    (
        lines[i].to_string(),
        lines.get(i + 1).copied().unwrap_or("").to_string(),
    )
}

/// Ajoute à `OrderService` un appel à une classe nouvelle, `Helper`.
fn add_helper(env: &Env) {
    let src = fs::read_to_string(env.repo.join(SERVICE)).unwrap();
    let src = src.replace(
        "        repo.save(new Order());\n    }\n",
        "        repo.save(new Order());\n    }\n\n    public void help() {\n        new Helper().run();\n    }\n",
    );
    env.write(SERVICE, &src);
    env.write(HELPER, HELPER_SRC);
}

#[test]
fn req147_ac1_every_node_has_exactly_one_community_numbered_without_gap() {
    let env = Env::maven3();
    env.ok(&["build", "--no-llm"]);
    let (rows, members) = env.tables();
    let nodes = env.node_ids();
    assert!(nodes.len() > 10, "{} nœuds", nodes.len());
    // Une ligne par nœud, une seule (clé primaire), et aucune autre.
    let ids: Vec<String> = members.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(ids, nodes);
    // Numéros de 0 à n - 1 sans trou ; taille = nombre de nœuds de la communauté.
    let numbers: Vec<i64> = rows.iter().map(|r| r.0).collect();
    assert_eq!(numbers, (0..rows.len() as i64).collect::<Vec<_>>());
    assert!(rows.len() >= 2, "{} communautés", rows.len());
    let mut count: BTreeMap<i64, i64> = BTreeMap::new();
    for (_, c) in &members {
        *count.entry(*c).or_default() += 1;
    }
    for r in &rows {
        assert_eq!(
            count.get(&r.0),
            Some(&r.3),
            "taille de la communauté {}",
            r.0
        );
        assert!(!r.1.is_empty());
        assert!(ids.contains(&r.2), "pivot {} inconnu", r.2);
        assert!((0.0..=1.0).contains(&f64::from_bits(r.4)));
    }
    assert_eq!(count.len(), rows.len());
}

#[test]
fn req147_ac2_update_without_change_writes_nothing() {
    let env = Env::maven3();
    env.ok(&["build", "--no-llm"]);
    let before = env.tables();
    let at = env.summary_at();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let o = env.ok(&["update", "--no-llm", "--verbose"]);
    assert_eq!(env.tables(), before);
    assert_eq!(env.summary_at(), at);
    assert!(!stderr(&o).contains("communautés"), "{}", stderr(&o));
}

#[test]
fn req147_ac2_added_class_gives_the_communities_of_a_build() {
    let env = Env::maven3();
    env.ok(&["build", "--no-llm"]);
    let before = env.tables();
    add_helper(&env);
    let o = env.ok(&["update", "--no-llm", "--verbose"]);
    let (res, _) = resolution_and_next(&o);
    assert!(
        res.contains("(incrémentale"),
        "passe incrémentale attendue : {res}"
    );
    let got = env.tables();
    assert_ne!(got, before);
    assert!(got.1.iter().any(|(n, _)| n == "type:com.acme.core.Helper"));

    let fresh = Env::maven3();
    add_helper(&fresh);
    fresh.ok(&["build", "--no-llm"]);
    assert_eq!(got, fresh.tables());
}

#[test]
fn req147_ac3_deleted_file_leaves_no_row_and_gives_the_communities_of_a_build() {
    const GONE: &str = "store/src/main/java/com/acme/store/JdbcOrderRepository.java";
    let env = Env::maven3();
    env.ok(&["build", "--no-llm"]);
    assert!(env
        .tables()
        .1
        .iter()
        .any(|(n, _)| n == "type:com.acme.store.JdbcOrderRepository"));
    fs::remove_file(env.repo.join(GONE)).unwrap();
    let o = env.ok(&["update", "--no-llm", "--verbose"]);
    let (res, _) = resolution_and_next(&o);
    assert!(
        res.contains("(incrémentale"),
        "passe incrémentale attendue : {res}"
    );
    let got = env.tables();
    assert!(got
        .1
        .iter()
        .all(|(n, _)| !n.contains("JdbcOrderRepository")));
    let nodes: BTreeSet<String> = env.node_ids().into_iter().collect();
    let members: BTreeSet<String> = got.1.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(nodes, members);

    let fresh = Env::maven3();
    fs::remove_file(fresh.repo.join(GONE)).unwrap();
    fresh.ok(&["build", "--no-llm"]);
    assert_eq!(got, fresh.tables());
}

#[test]
fn req147_ac5_mock_concepts_have_a_community_and_the_stored_ones_are_those_of_the_final_graph() {
    let env = Env::new("maven3/repo", Some("maven3/mock"), MOCK_CONFIG);
    let o = env.ok(&["build", "--verbose"]);
    let (_, members) = env.tables();
    let concepts: Vec<String> = env
        .node_ids()
        .into_iter()
        .filter(|n| n.starts_with("concept:"))
        .collect();
    assert!(!concepts.is_empty(), "le mock doit produire des concepts");
    for c in &concepts {
        assert!(members.iter().any(|(n, _)| n == c), "{c} sans communauté");
    }
    let nodes: Vec<String> = env.node_ids();
    assert_eq!(
        members.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        nodes
    );
    // La seconde passe (après l'enrichissement) est celle dont le calcul est stocké : une passe
    // complète sur le graphe final redonne les mêmes tables.
    assert!(stderr(&o).contains("(incrémentale"), "{}", stderr(&o));
    let stored = env.tables();
    env.mark_unresolved();
    let o = env.ok(&["update", "--verbose"]);
    let (res, _) = resolution_and_next(&o);
    assert!(res.contains("(complète)"), "{res}");
    assert_eq!(env.tables(), stored);
}

fn summary_keys_from_export(export: &Value) -> Value {
    let nodes = export["nodes"].as_array().unwrap();
    let mut degree: BTreeMap<String, u64> = BTreeMap::new();
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut mentions: BTreeMap<String, u64> = BTreeMap::new();
    for e in export["edges"].as_array().unwrap() {
        let (s, t, k) = (
            e["source"].as_str().unwrap(),
            e["target"].as_str().unwrap(),
            e["kind"].as_str().unwrap(),
        );
        *degree.entry(s.to_string()).or_default() += 1;
        *degree.entry(t.to_string()).or_default() += 1;
        if k == "depends_on" {
            deps.entry(s.to_string()).or_default().push(t.to_string());
        }
        if k == "mentions" {
            *mentions.entry(t.to_string()).or_default() += 1;
        }
    }
    let mut modules: Vec<Value> = Vec::new();
    for n in nodes
        .iter()
        .filter(|n| n["id"].as_str().unwrap().starts_with("module:"))
    {
        let id = n["id"].as_str().unwrap();
        let mut d = deps.get(id).cloned().unwrap_or_default();
        d.sort();
        modules.push(serde_json::json!({"id": id, "name": n["short"], "depends_on": d}));
    }
    modules.sort_by_key(|m| m["id"].as_str().unwrap().to_string());
    let mut ranked: Vec<(String, u64)> = degree.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top: Vec<Value> = ranked
        .iter()
        .take(10)
        .map(|(id, d)| serde_json::json!({"id": id, "degree": d}))
        .collect();
    let mut themes: Vec<(String, u64)> = nodes
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .filter(|id| id.starts_with("concept:"))
        .filter_map(|id| mentions.get(id).map(|c| (id.to_string(), *c)))
        .collect();
    themes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let themes: Vec<Value> = themes
        .iter()
        .take(10)
        .map(|(id, c)| serde_json::json!({"concept": id, "mentions": c}))
        .collect();
    serde_json::json!({"modules": modules, "top_nodes": top, "themes": themes})
}

/// AC-147-6 : sur les dépôts de REQ-065, les clés `modules`, `top_nodes` et `themes` du résumé
/// sont celles que donnent les nœuds et les liens de l'export, et la clé `communities` décrit
/// les tables. Les nœuds, liens et non résolues de l'export normalisé sont figés par les oracles
/// de `tests/sqlite_lot3.rs` (0.20.0) et de `tests/incremental.rs` (0.21.0), restés verts.
#[test]
fn req147_ac6_summary_keys_and_graph_are_unchanged_on_the_req065_repos() {
    for c in support::CASES {
        let mock = c.mock;
        for llm in [false, true] {
            if llm && mock.is_none() {
                continue;
            }
            let env = Env::new(c.repo, mock, MOCK_CONFIG);
            env.ok(&["build", if llm { "--verbose" } else { "--no-llm" }]);
            let export = env.json(&["export", "--output", "-"]);
            let summary = env.json(&["summary", "--json"]);
            let want = summary_keys_from_export(&export);
            for key in ["modules", "top_nodes", "themes"] {
                assert_eq!(summary[key], want[key], "{} (llm {llm}) : {key}", c.name);
            }
            let (rows, members) = env.tables();
            assert_eq!(summary["communities"]["total"], rows.len(), "{}", c.name);
            assert_eq!(
                members.len(),
                export["nodes"].as_array().unwrap().len(),
                "{}",
                c.name
            );
            let top = summary["communities"]["top"].as_array().unwrap();
            assert_eq!(top.len(), rows.len().min(10), "{}", c.name);
            for (t, r) in top.iter().zip(&rows) {
                assert_eq!(t["id"], r.0);
                assert_eq!(t["label"], r.1.as_str());
                assert_eq!(t["size"], r.3);
                assert_eq!(t["cohesion"].as_f64().unwrap().to_bits(), r.4);
            }
        }
    }
}

#[test]
fn req146_ac6_communities_do_not_depend_on_jobs_or_on_the_run() {
    let one = Env::maven3();
    one.ok(&["build", "--no-llm", "--jobs", "1"]);
    let four = Env::maven3();
    four.ok(&["build", "--no-llm", "--jobs", "4"]);
    assert_eq!(one.tables(), four.tables());
    let first = four.tables();
    four.ok(&["build", "--no-llm", "--jobs", "4"]);
    assert_eq!(four.tables(), first);
    one.ok(&["build", "--no-llm", "--jobs", "1"]);
    assert_eq!(one.tables(), first);
}

#[test]
fn req147_summary_stored_communities_key_equals_the_tables_after_an_incremental_pass() {
    let env = Env::maven3();
    env.ok(&["build", "--no-llm"]);
    add_helper(&env);
    env.ok(&["update", "--no-llm"]);
    let s = env.json(&["summary", "--json"]);
    let (rows, _) = env.tables();
    assert_eq!(s["communities"]["total"], rows.len());
    let first = &s["communities"]["top"][0];
    assert_eq!(first["id"], 0);
    assert_eq!(first["size"], rows[0].3);
    let keys: Vec<&str> = first
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["cohesion", "id", "label", "size"]);
}
