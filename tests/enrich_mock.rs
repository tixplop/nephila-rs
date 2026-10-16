//! Fournisseur simulé : enrichissement par fixtures version 2 (REQ-003, REQ-024).

use nephila::error::Error;
use nephila::llm::mock::{MockLlm, MockOptions};
use nephila::llm::Llm;

fn mock(dir: &std::path::Path, log: Option<&std::path::Path>) -> MockLlm {
    MockLlm::new(MockOptions {
        dir: Some(dir.to_path_buf()),
        log: log.map(|l| l.to_path_buf()),
        ..Default::default()
    })
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn req003_ac1_enrich_returns_fixture_per_file_and_chunk_else_empty() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/A.java.json"),
        r#"{"version":2,"summaries":[{"node":"type:A","summary":"général"}],"concepts":[]}"#,
    )
    .unwrap();
    std::fs::write(
        d.path().join("src/A.java.2.json"),
        r#"{"version":2,"summaries":[{"node":"type:A","summary":"morceau 2"}],"concepts":[]}"#,
    )
    .unwrap();
    let log = d.path().join("log");
    let m = mock(d.path(), Some(&log));
    let n = ids(&["type:A"]);
    let e1 = m.enrich("src/A.java", 1, "", &n).unwrap();
    assert_eq!(e1.summaries[0].1, "général");
    let e2 = m.enrich("src/A.java", 2, "", &n).unwrap();
    assert_eq!(e2.summaries[0].1, "morceau 2");
    let none = m.enrich("src/B.java", 1, "", &n).unwrap();
    assert!(none.summaries.is_empty() && none.concepts.is_empty() && none.rejected.is_empty());
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "src/A.java\t1\nsrc/A.java\t2\nsrc/B.java\t1\n"
    );
}

#[test]
fn req003_ac1_enrich_rejects_version_1_fixture_and_bad_paths() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("A.java.json"),
        r#"{"version":1,"declares":[],"references":[]}"#,
    )
    .unwrap();
    let m = mock(d.path(), None);
    assert!(m.enrich("A.java", 1, "", &[]).is_err());
    assert!(m.enrich("../A.java", 1, "", &[]).is_err());
}

#[test]
fn req003_ac6_summary_for_node_outside_list_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("A.java.json"),
        r#"{"version":2,"summaries":[{"node":"type:A","summary":"ok"},{"node":"type:Fantome","summary":"x"}],"concepts":[]}"#,
    )
    .unwrap();
    let e = mock(d.path(), None)
        .enrich("A.java", 1, "", &ids(&["type:A"]))
        .unwrap();
    assert_eq!(e.summaries.len(), 1);
    assert_eq!(e.rejected.len(), 1);
    assert!(e.rejected[0].contains("type:Fantome"));
}

#[test]
fn req003_enrich_shares_failure_switches_with_extract() {
    let d = tempfile::tempdir().unwrap();
    let m = MockLlm::new(MockOptions {
        dir: Some(d.path().to_path_buf()),
        fail_after: Some(1),
        timeout_files: vec!["T.java".into()],
        ..Default::default()
    });
    assert!(matches!(
        m.enrich("T.java", 1, "", &[]),
        Err(Error::Timeout(_))
    ));
    assert!(matches!(
        m.enrich("B.java", 1, "", &[]),
        Err(Error::Network(_))
    ));
}

fn lot_item(file: &str) -> nephila::llm::LotItem {
    nephila::llm::LotItem {
        file: file.into(),
        text: String::new(),
        node_ids: vec![format!("type:{}", file.trim_end_matches(".java"))],
    }
}

#[test]
fn req003_ac7_lot_is_one_call_one_log_line_with_v3_assembled() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("A.java.json"),
        r#"{"version":2,"summaries":[{"node":"type:A","summary":"un"}],"concepts":[]}"#,
    )
    .unwrap();
    std::fs::write(
        d.path().join("B.java.1.json"),
        r#"{"version":2,"summaries":[{"node":"type:B","summary":"deux"}],"concepts":[]}"#,
    )
    .unwrap();
    let log = d.path().join("log");
    let m = mock(d.path(), Some(&log));
    let r = m
        .enrich_lot(&[lot_item("A.java"), lot_item("B.java"), lot_item("C.java")])
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "A.java;B.java;C.java\t1\n"
    );
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].1.as_ref().unwrap().summaries[0].1, "un");
    assert_eq!(r[1].1.as_ref().unwrap().summaries[0].1, "deux");
    assert!(r[2].1.as_ref().unwrap().summaries.is_empty());
}

#[test]
fn req003_ac7_lot_non_conforming_fixture_fails_whole_lot() {
    for bad in [
        "pas du json",
        r#"{"version":1,"declares":[],"references":[]}"#,
        r#"{"version":2,"summaries":[],"concepts":[],"inconnu":1}"#,
    ] {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("B.java.json"), bad).unwrap();
        let m = mock(d.path(), None);
        assert!(m
            .enrich_lot(&[lot_item("A.java"), lot_item("B.java")])
            .is_err());
    }
}

#[test]
fn req003_ac7_lot_timeout_and_counters() {
    let d = tempfile::tempdir().unwrap();
    let m = MockLlm::new(MockOptions {
        dir: Some(d.path().to_path_buf()),
        timeout_files: vec!["B.java".into()],
        fail_after: Some(1),
        ..Default::default()
    });
    let lot = [lot_item("A.java"), lot_item("C.java")];
    assert!(m.enrich_lot(&lot).is_ok());
    assert!(matches!(m.enrich_lot(&lot), Err(Error::Network(_))));
    let m = MockLlm::new(MockOptions {
        timeout_files: vec!["B.java".into()],
        ..Default::default()
    });
    assert!(matches!(
        m.enrich_lot(&[lot_item("A.java"), lot_item("B.java")]),
        Err(Error::Timeout(_))
    ));
    let m = MockLlm::new(MockOptions {
        key_revoked_after: Some(1),
        ..Default::default()
    });
    assert!(m.enrich_lot(&lot).is_ok());
    assert!(matches!(m.enrich_lot(&lot), Err(Error::InvalidKey(_))));
}
