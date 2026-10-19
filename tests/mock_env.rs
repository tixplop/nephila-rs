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

// ---- AC-003-8 : NEPHILA_MOCK_MALFORMED (environnement fourni, sans état global) ----

mod malformed {
    use nephila::error::Error;
    use nephila::llm::mock::MockLlm;
    use nephila::llm::{Llm, LotItem};
    use std::path::Path;

    const FIXTURE: &str =
        r#"{"version":2,"summaries":[{"node":"type:A","summary":"s"}],"concepts":[]}"#;

    fn mock(dir: &Path, malformed: &str, extra: &[(&str, &str)]) -> MockLlm {
        let mut pairs: Vec<(String, String)> = vec![
            ("NEPHILA_MOCK_DIR".into(), dir.display().to_string()),
            (
                "NEPHILA_MOCK_LOG".into(),
                dir.join("log").display().to_string(),
            ),
            ("NEPHILA_MOCK_MALFORMED".into(), malformed.into()),
        ];
        for (k, v) in extra {
            pairs.push((k.to_string(), v.to_string()));
        }
        MockLlm::from_env_with(&move |k| pairs.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone()))
            .unwrap()
    }

    fn setup() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.java.json"), FIXTURE).unwrap();
        std::fs::write(
            d.path().join("b.java.json"),
            FIXTURE.replace("type:A", "type:B"),
        )
        .unwrap();
        std::fs::write(
            d.path().join("_ask.json"),
            r#"{"found":true,"answer":"ok","nodes":[]}"#,
        )
        .unwrap();
        d
    }

    fn ids(n: &str) -> Vec<String> {
        vec![format!("type:{n}")]
    }

    fn log_lines(d: &Path) -> usize {
        std::fs::read_to_string(d.join("log"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn is_format(r: &Result<impl std::fmt::Debug, Error>) -> bool {
        match r {
            Err(Error::Format(m)) => {
                m.starts_with("réponse LLM non conforme au schéma : ")
                    && m.ends_with("(NEPHILA_MOCK_MALFORMED)")
            }
            _ => false,
        }
    }

    #[test]
    fn req003_ac8_file_count_then_fixture() {
        let d = setup();
        let m = mock(d.path(), "a.java:2", &[]);
        assert!(is_format(&m.enrich("a.java", 1, "", &ids("A"))));
        assert!(is_format(&m.enrich("a.java", 1, "", &ids("A"))));
        assert_eq!(
            m.enrich("a.java", 1, "", &ids("A"))
                .unwrap()
                .summaries
                .len(),
            1
        );
        assert_eq!(log_lines(d.path()), 3);
        // Un autre fichier n'est pas touché.
        assert!(m.enrich("b.java", 1, "", &ids("B")).is_ok());
    }

    #[test]
    fn req003_ac8_batch_counts_all_files_it_carries() {
        let d = setup();
        let m = mock(d.path(), "b.java:1", &[]);
        let items = vec![
            LotItem {
                file: "a.java".into(),
                text: String::new(),
                node_ids: ids("A"),
            },
            LotItem {
                file: "b.java".into(),
                text: String::new(),
                node_ids: ids("B"),
            },
        ];
        assert!(is_format(&m.enrich_lot(&items)));
        let r = m.enrich_lot(&items).unwrap();
        assert!(r.iter().all(|(_, e)| e.is_ok()));
        assert_eq!(log_lines(d.path()), 2);
    }

    #[test]
    fn req003_ac8_ask() {
        let d = setup();
        let m = mock(d.path(), "_ask:1", &[]);
        assert!(is_format(&m.ask("q", "c")));
        assert!(m.ask("q", "c").is_ok());
        assert_eq!(log_lines(d.path()), 2);
    }

    #[test]
    fn req003_ac8_invalid_values_name_the_variable() {
        for bad in ["a.java", "a.java:0", "a.java:x", "a.java:1,", ""] {
            if bad.is_empty() {
                continue;
            }
            let r = MockLlm::from_env_with(&|k| {
                (k == "NEPHILA_MOCK_MALFORMED").then(|| bad.to_string())
            });
            let e = r.unwrap_err().to_string();
            assert!(e.contains("NEPHILA_MOCK_MALFORMED"), "{bad}: {e}");
        }
    }

    #[test]
    fn req003_ac8_fail_after_applies_after_the_malformed_call() {
        let d = setup();
        let m = mock(d.path(), "a.java:1", &[("NEPHILA_MOCK_FAIL_AFTER", "1")]);
        assert!(is_format(&m.enrich("a.java", 1, "", &ids("A"))));
        assert!(matches!(
            m.enrich("a.java", 1, "", &ids("A")),
            Err(Error::Network(_))
        ));
    }
}
