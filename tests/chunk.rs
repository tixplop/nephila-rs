//! Découpage des gros fichiers (REQ-007).

use assert_cmd::Command;
use nephila::chunk::{header, split};
use nephila::model::{EdgeKind, NodeKind};
use nephila::store::Store;
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
        // Une fixture par appel (jusqu'à 1000) : chacun résume une méthode propre.
        for n in 1..=1000 {
            let json = format!(
                r#"{{"version":2,"summaries":[{{"node":"method:com.acme.Big.m{n}","summary":"part{n}"}}],"concepts":[]}}"#
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
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        if let Some(f) = fail_after {
            c.env("NEPHILA_MOCK_FAIL_AFTER", f);
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
    assert!(n_calls <= 1000, "appels : {n_calls}");
    let store = env.store();
    let nodes = store.nodes().unwrap();
    let types: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Type).collect();
    assert_eq!(types.len(), 1);
    let methods: Vec<_> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .collect();
    assert_eq!(methods.len(), 20_000);
    let edges = store.edges().unwrap();
    for m in methods.iter().take(50) {
        assert!(
            edges.iter().any(|e| e.kind == EdgeKind::Contains
                && e.source == types[0].id
                && e.target == m.id),
            "pas de contains vers {}",
            m.id
        );
    }
    // Les résumés de tous les morceaux sont fusionnés : un par morceau.
    let summarized: Vec<_> = methods.iter().filter(|m| m.summary.is_some()).collect();
    assert_eq!(summarized.len(), n_calls);
    assert_eq!(store.count_files_to_enrich().unwrap(), 0);
}

#[test]
fn req007_ac3_small_file_is_one_call() {
    let env = Env::new(60_000, &big_java(50));
    assert!(env.build(None).status.success());
    assert_eq!(env.calls().len(), 1);
}

#[test]
fn req007_failure_on_a_chunk_keeps_structure_and_leaves_file_to_enrich() {
    let env = Env::new(100_000, &big_java(20_000));
    let out = env.build(Some("1"));
    assert!(!out.status.success());
    let store = env.store();
    let files = store.files().unwrap();
    assert!(files
        .iter()
        .all(|f| f.state == nephila::model::FileState::Indexed));
    assert_eq!(store.count_files_to_enrich().unwrap(), 1);
    let nodes = store.nodes().unwrap();
    assert_eq!(
        nodes.iter().filter(|n| n.kind == NodeKind::Method).count(),
        20_000
    );
    assert!(nodes.iter().all(|n| n.summary.is_none()));
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
