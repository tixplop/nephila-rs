//! build_llm(Mock) lit l'environnement du processus (un seul test : l'environnement est global).

use rkg::config::Config;
use rkg::llm::build_llm;
use std::path::Path;

#[test]
fn req003_build_llm_mock_reads_process_env() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("A.java.json"),
        r#"{"version":1,"declares":[{"kind":"type","name":"A"}],"references":[]}"#,
    )
    .unwrap();
    let log = d.path().join("log");
    let cfg = Config::parse("provider='mock'", Path::new("/x")).unwrap();
    std::env::set_var("RKG_MOCK_DIR", d.path());
    std::env::set_var("RKG_MOCK_LOG", &log);
    std::env::set_var("RKG_MOCK_INVALID_KEY", "1");
    let llm = build_llm(&cfg).unwrap();
    assert_eq!(llm.extract("A.java", 1, "").unwrap().declares.len(), 1);
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "A.java\t1\n");
    assert!(llm.verify_key().is_err());
    std::env::set_var("RKG_MOCK_INVALID_KEY", "0");
    assert!(build_llm(&cfg).unwrap().verify_key().is_ok());
    std::env::set_var("RKG_MOCK_DELAY_MS", "x");
    assert!(build_llm(&cfg).is_err());
}
