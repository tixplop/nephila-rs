//! Fournisseur simulé : enrichissement par fixtures version 2 (REQ-003, REQ-024).

use rkg::error::Error;
use rkg::llm::mock::{MockLlm, MockOptions};
use rkg::llm::Llm;

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
