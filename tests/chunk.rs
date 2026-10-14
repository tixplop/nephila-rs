//! Découpage des gros fichiers (REQ-007).

use assert_cmd::Command;
use rkg::chunk::{header, merge, split};
use rkg::llm::Extraction;
use rkg::model::{EdgeKind, NodeKind};
use rkg::store::Store;
use std::fs;
use std::path::{Path, PathBuf};

const PKG: &str =
    "package com.acme;\n\nimport java.util.List;\nimport com.acme.Other;\n\npublic class Big {\n";

fn big_java(lines: usize) -> String {
    let mut s = String::from(PKG);
    for i in 0..lines {
        s.push_str(&format!("    void m{i}() {{}}\n"));
    }
    s.push_str("}\n");
    s
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new(max_chunk: usize, java: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let dir = repo.join("src/main/java/com/acme");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Big.java"), java).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!("provider = \"mock\"\nmodel = \"mock\"\nmax_chunk_chars = {max_chunk}\n"),
        )
        .unwrap();
        let mock = tmp.path().join("mock");
        let mdir = mock.join("src/main/java/com/acme");
        fs::create_dir_all(&mdir).unwrap();
        // Une fixture par morceau (jusqu'à 40) : chacun déclare le type et une méthode propre.
        for n in 1..=40 {
            let json = format!(
                r#"{{"version":1,"context":"package com.acme;","declares":[
                {{"kind":"package","name":"com.acme"}},
                {{"kind":"type","name":"com.acme.Big","subkind":"class"}},
                {{"kind":"method","name":"com.acme.Big.part{n}"}}],
                "references":[{{"source":"method:com.acme.Big.part{n}","kind":"calls","target":"Other.go"}}]}}"#
            );
            fs::write(mdir.join(format!("Big.java.{n}.json")), json).unwrap();
        }
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            mock,
            log,
        }
    }

    fn build(&self, fail_after: Option<&str>) -> std::process::Output {
        let mut c = Command::cargo_bin("rkg").unwrap();
        c.env_remove("RKG_MOCK_FAIL_AFTER")
            .env_remove("RKG_MOCK_INVALID_KEY")
            .env_remove("RKG_MOCK_DELAY_MS")
            .env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log);
        if let Some(f) = fail_after {
            c.env("RKG_MOCK_FAIL_AFTER", f);
        }
        c.arg("-C").arg(&self.repo).arg("build").output().unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn store(&self) -> Store {
        Store::open_readonly(Path::new(&self.repo)).unwrap()
    }
}

#[test]
fn req007_ac1_big_class_is_sent_in_several_calls() {
    let env = Env::new(100_000, &big_java(20_000));
    let out = env.build(None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = env.calls();
    assert!(calls.len() > 1, "appels : {calls:?}");
    for (i, l) in calls.iter().enumerate() {
        assert_eq!(l, &format!("src/main/java/com/acme/Big.java\t{}", i + 1));
    }
}

#[test]
fn req007_ac2_merged_into_one_type_with_all_methods_contained() {
    let env = Env::new(100_000, &big_java(20_000));
    assert!(env.build(None).status.success());
    let n_calls = env.calls().len();
    let store = env.store();
    let nodes = store.nodes().unwrap();
    let types: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
    assert_eq!(types.len(), 1);
    let methods: Vec<_> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(methods.len(), n_calls);
    let edges = store.edges().unwrap();
    for m in &methods {
        assert!(
            edges
                .iter()
                .any(|e| e.kind == EdgeKind::Contains && e.source == types[0].id && &e.target == m),
            "pas de contains vers {m}"
        );
    }
    // Références dédupliquées : une par méthode (cible identique dans chaque morceau).
    assert_eq!(store.refs().unwrap().len(), n_calls);
}

#[test]
fn req007_ac3_small_file_is_one_call() {
    let env = Env::new(60_000, &big_java(50));
    assert!(env.build(None).status.success());
    assert_eq!(env.calls().len(), 1);
}

#[test]
fn req007_failure_on_a_chunk_leaves_whole_file_pending() {
    let env = Env::new(100_000, &big_java(20_000));
    let out = env.build(Some("1"));
    assert!(!out.status.success());
    let store = env.store();
    let files = store.files().unwrap();
    assert!(files
        .iter()
        .all(|f| f.state == rkg::model::FileState::Pending));
    assert!(store
        .nodes()
        .unwrap()
        .iter()
        .all(|n| n.kind != NodeKind::Method));
}

#[test]
fn req007_split_small_text_is_untouched() {
    let t = big_java(3);
    assert_eq!(split(&t, 60_000), vec![t]);
}

#[test]
fn req007_split_on_line_ends_and_lossless() {
    let t = big_java(2_000);
    let parts = split(&t, 5_000);
    assert!(parts.len() > 3);
    let h = header(&t);
    let mut rebuilt = String::new();
    for (i, p) in parts.iter().enumerate() {
        assert!(p.ends_with('\n'));
        assert!(p.chars().count() <= 5_000, "morceau {i} trop gros");
        if i == 0 {
            rebuilt.push_str(p);
        } else {
            assert!(p.starts_with(&h), "en-tête absent du morceau {i}");
            rebuilt.push_str(&p[h.len()..]);
        }
    }
    assert_eq!(rebuilt, t);
}

#[test]
fn req007_header_has_package_imports_and_enclosing_type() {
    let h = header(&big_java(10));
    assert!(h.contains("package com.acme;"));
    assert!(h.contains("import java.util.List;"));
    assert!(h.contains("import com.acme.Other;"));
    assert!(h.contains("public class Big {"));
    assert!(!h.contains("void m0"));
    assert!(h.ends_with('\n'));
}

#[test]
fn req007_header_of_headerless_text_is_empty() {
    assert_eq!(header("juste du texte\nsans rien\n"), "");
}

#[test]
fn req007_split_handles_unbroken_long_line_and_tiny_limit() {
    let t = format!("{}\nb\nc\n", "x".repeat(500));
    let parts = split(&t, 10);
    assert_eq!(parts.concat(), t);
    assert!(split("", 10).len() <= 1);
}

#[test]
fn req007_split_multibyte_counts_characters() {
    let t = "é\n".repeat(100);
    let parts = split(&t, 50);
    assert!(parts.len() >= 4);
    assert_eq!(parts.concat(), t);
}

#[test]
fn req007_merge_dedups_nodes_and_references() {
    let a = Extraction::parse(
        r#"{"version":1,"declares":[{"kind":"type","name":"a.T","subkind":"class"},{"kind":"method","name":"a.T.f"}],
        "references":[{"source":"method:a.T.f","kind":"calls","target":"X.y"}]}"#,
    )
    .unwrap();
    let b = Extraction::parse(
        r#"{"version":1,"context":"package a;","declares":[{"kind":"type","name":"a.T","summary":"s"},{"kind":"method","name":"a.T.g"}],
        "references":[{"source":"method:a.T.f","kind":"calls","target":"X.y"},{"source":"method:a.T.g","kind":"calls","target":"X.y"}]}"#,
    )
    .unwrap();
    let m = merge(vec![a, b]);
    assert_eq!(m.declares.len(), 3);
    assert_eq!(m.references.len(), 2);
    assert_eq!(m.context.as_deref(), Some("package a;"));
    let t = m.declares.iter().find(|d| d.qualified == "a.T").unwrap();
    assert!(t.subkind.is_some());
    assert_eq!(t.summary.as_deref(), Some("s"));
}
