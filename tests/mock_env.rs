//! build_llm(Mock) lit l'environnement du processus (un seul test : l'environnement est global).

use nephila::config::Config;
use nephila::llm::build_llm;
use std::path::Path;

#[test]
fn req003_build_llm_mock_reads_process_env() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("A.java.json"),
        r#"{"version":2,"summaries":[{"node":"type:A","summary":"s"}],"concepts":[]}"#,
    )
    .unwrap();
    let ids = vec!["type:A".to_string()];
    let log = d.path().join("log");
    let cfg = Config::parse("provider='mock'", Path::new("/x")).unwrap();
    std::env::set_var("NEPHILA_MOCK_DIR", d.path());
    std::env::set_var("NEPHILA_MOCK_LOG", &log);
    std::env::set_var("NEPHILA_MOCK_INVALID_KEY", "1");
    let llm = build_llm(&cfg).unwrap();
    assert_eq!(
        llm.enrich("A.java", 1, "", &ids).unwrap().summaries.len(),
        1
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "A.java\t1\n");
    assert!(llm.verify_key().is_err());
    std::env::set_var("NEPHILA_MOCK_INVALID_KEY", "0");
    assert!(build_llm(&cfg).unwrap().verify_key().is_ok());
    std::env::set_var("NEPHILA_MOCK_TIMEOUT_FILES", "A.java, B.java");
    std::env::set_var("NEPHILA_MOCK_KEY_REVOKED_AFTER", "1");
    let llm = build_llm(&cfg).unwrap();
    assert!(matches!(
        llm.enrich("B.java", 1, "", &ids),
        Err(nephila::error::Error::Timeout(_))
    ));
    assert!(matches!(
        llm.enrich("C.java", 1, "", &ids),
        Err(nephila::error::Error::InvalidKey(_))
    ));
    std::env::remove_var("NEPHILA_MOCK_TIMEOUT_FILES");
    std::env::remove_var("NEPHILA_MOCK_KEY_REVOKED_AFTER");
    std::env::set_var("NEPHILA_MOCK_DELAY_MS", "x");
    assert!(build_llm(&cfg).is_err());
}
