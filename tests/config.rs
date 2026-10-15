use std::path::{Path, PathBuf};

use rkg::config::{config_path_with, Config, Provider};
use rkg::llm::mock::{MockLlm, MockOptions};
use rkg::llm::Llm;
use rkg::llm::{AskReply, Enrichment};

fn parse(t: &str) -> Result<Config, String> {
    Config::parse(t, Path::new("/x/config.toml")).map_err(|e| e.to_string())
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let v: Vec<(String, String)> = pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    move |k| v.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone())
}

#[test]
fn req002_ac1_missing_file_cites_path_and_keys() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("absent.toml");
    let msg = Config::load_from(&p).unwrap_err().to_string();
    assert!(msg.contains(&p.display().to_string()), "{msg}");
    for k in ["provider", "model", "api_key_env"] {
        assert!(msg.contains(k), "{msg}");
    }
}

#[test]
fn req002_config_path_precedence() {
    let p = config_path_with(&env_of(&[
        ("RKG_CONFIG", "/a.toml"),
        ("XDG_CONFIG_HOME", "/xdg"),
    ]));
    assert_eq!(p.unwrap(), PathBuf::from("/a.toml"));
    let p = config_path_with(&env_of(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/h")]));
    assert_eq!(p.unwrap(), PathBuf::from("/xdg/rkg/config.toml"));
    let p = config_path_with(&env_of(&[("HOME", "/h")]));
    assert_eq!(p.unwrap(), PathBuf::from("/h/.config/rkg/config.toml"));
    assert!(config_path_with(&env_of(&[])).is_err());
}

#[test]
fn req002_ac1_valid_config_with_defaults() {
    let c = parse("provider='anthropic'\nmodel='m'\napi_key_env='K'").unwrap();
    assert_eq!(c.provider, Provider::Anthropic);
    assert_eq!(c.max_chunk_chars, 60_000);
    assert!(c.concurrency >= 1);
    assert_eq!(c.base_url, None);
}

#[test]
fn req002_ac2_unknown_provider_names_key() {
    let m = parse("provider='gpt'\nmodel='m'").unwrap_err();
    assert!(m.contains("provider") && m.contains("gpt"), "{m}");
}

#[test]
fn req002_ac2_missing_keys_named() {
    assert!(parse("model='m'").unwrap_err().contains("provider"));
    assert!(parse("provider='openai'").unwrap_err().contains("model"));
    assert!(parse("provider='anthropic'\nmodel='m'")
        .unwrap_err()
        .contains("api_key_env"));
    assert!(parse("provider='openai'\nmodel='m'").is_ok());
    assert!(parse("provider='mock'").is_ok());
}

#[test]
fn req002_ac2_invalid_values_and_toml() {
    assert!(parse("provider='mock'\nconcurrency=0")
        .unwrap_err()
        .contains("concurrency"));
    assert!(parse("provider='mock'\nmax_chunk_chars=-5")
        .unwrap_err()
        .contains("max_chunk_chars"));
    assert!(parse("provider='mock'\nbogus=1")
        .unwrap_err()
        .contains("bogus"));
    assert!(parse("provider=").is_err());
    assert!(parse("provider='openai'\nmodel=''")
        .unwrap_err()
        .contains("model"));
}

#[test]
fn req002_ac4_missing_env_var_named() {
    let c = parse("provider='anthropic'\nmodel='m'\napi_key_env='MY_KEY_VAR'").unwrap();
    let e = c.api_key_with(&env_of(&[])).unwrap_err().to_string();
    assert!(e.contains("MY_KEY_VAR"), "{e}");
    assert!(c.api_key_with(&env_of(&[("MY_KEY_VAR", "")])).is_err());
    let k = c.api_key_with(&env_of(&[("MY_KEY_VAR", "s")])).unwrap();
    assert_eq!(k.as_deref(), Some("s"));
    let local = parse("provider='openai'\nmodel='m'").unwrap();
    assert_eq!(local.api_key_with(&env_of(&[])).unwrap(), None);
}

// ---- fixtures d'enrichissement (schéma version 2)

const GOOD: &str = r#"{"version":2,"summaries":[{"node":"type:A","summary":"un"},{"node":"type:Z","summary":"deux"}],"concepts":[]}"#;

fn ex(m: &MockLlm, file: &str, chunk: usize) -> rkg::error::Result<Enrichment> {
    m.enrich(
        file,
        chunk,
        "",
        &["type:A".to_string(), "type:Z".to_string()],
    )
}

// ---- LLM simulé (REQ-003)

fn mock_with(dir: &Path, f: impl FnOnce(&mut MockOptions)) -> MockLlm {
    let mut o = MockOptions {
        dir: Some(dir.join("fx")),
        log: Some(dir.join("log")),
        ..Default::default()
    };
    f(&mut o);
    std::fs::create_dir_all(dir.join("fx/src")).unwrap();
    MockLlm::new(o)
}

#[test]
fn req003_ac1_fixture_and_empty_default() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |_| {});
    std::fs::write(d.path().join("fx/src/A.java.json"), GOOD).unwrap();
    let e = ex(&m, "src/A.java", 1).unwrap();
    assert_eq!(e.summaries.len(), 2);
    let e = ex(&m, "src/B.java", 1).unwrap();
    assert_eq!(e, Enrichment::default());
}

#[test]
fn req003_ac1_chunk_fixture_preferred() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |_| {});
    std::fs::write(d.path().join("fx/src/A.java.json"), GOOD).unwrap();
    std::fs::write(
        d.path().join("fx/src/A.java.2.json"),
        r#"{"version":2,"summaries":[{"node":"type:Z","summary":"Z"}],"concepts":[]}"#,
    )
    .unwrap();
    assert_eq!(ex(&m, "src/A.java", 1).unwrap().summaries.len(), 2);
    assert_eq!(ex(&m, "src/A.java", 2).unwrap().summaries[0].0, "type:Z");
}

#[test]
fn req003_ac1_invalid_fixture_is_call_error() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |_| {});
    std::fs::write(d.path().join("fx/src/A.java.json"), "{\"nope\":1}").unwrap();
    assert!(ex(&m, "src/A.java", 1).is_err());
}

#[test]
fn req003_rejects_escaping_paths() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |_| {});
    assert!(ex(&m, "../x", 1).is_err());
    assert!(ex(&m, "/etc/passwd", 1).is_err());
}

#[test]
fn req003_ac2_calls_are_logged() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |_| {});
    ex(&m, "src/A.java", 1).unwrap();
    ex(&m, "src/A.java", 2).unwrap();
    ex(&m, "docs/x.md", 1).unwrap();
    let log = std::fs::read_to_string(d.path().join("log")).unwrap();
    assert_eq!(log, "src/A.java\t1\nsrc/A.java\t2\ndocs/x.md\t1\n");
}

#[test]
fn req003_ac3_fail_after_and_invalid_key() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |o| o.fail_after = Some(2));
    assert!(ex(&m, "a", 1).is_ok());
    assert!(ex(&m, "b", 1).is_ok());
    let e = ex(&m, "c", 1).unwrap_err().to_string();
    assert!(e.contains("réseau"), "{e}");
    assert!(ex(&m, "d", 1).is_err());
    assert!(m.verify_key().is_ok());

    let m = mock_with(d.path(), |o| o.invalid_key = true);
    assert!(m.verify_key().is_err());
}

#[test]
fn req003_delay_is_applied() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |o| o.delay_ms = 80);
    let t = std::time::Instant::now();
    ex(&m, "a", 1).unwrap();
    assert!(t.elapsed().as_millis() >= 80);
}

#[test]
fn req003_ac4_ask_fixture() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |_| {});
    assert_eq!(m.ask("q", "c").unwrap(), AskReply::NotFound);
    std::fs::write(
        d.path().join("fx/_ask.json"),
        r#"{"found":true,"answer":"Voir OrderService","nodes":["type:OrderService"]}"#,
    )
    .unwrap();
    assert_eq!(
        m.ask("q", "c").unwrap(),
        AskReply::Found {
            answer: "Voir OrderService".into(),
            nodes: vec!["type:OrderService".into()]
        }
    );
    std::fs::write(d.path().join("fx/_ask.json"), r#"{"found":false}"#).unwrap();
    assert_eq!(m.ask("q", "c").unwrap(), AskReply::NotFound);
    std::fs::write(d.path().join("fx/_ask.json"), r#"{"found":true}"#).unwrap();
    assert!(m.ask("q", "c").is_err());
}

#[test]
fn req006_ac7_ask_found_false_with_answer_rejected() {
    assert!(AskReply::parse(r#"{"found":false,"answer":"x"}"#).is_err());
    assert!(AskReply::parse(r#"{"found":false,"nodes":[]}"#).is_err());
}

#[test]
fn req002_empty_base_url_and_api_key_env_rejected() {
    let e = parse("provider='openai'\nmodel='m'\nbase_url=''").unwrap_err();
    assert!(e.contains("base_url"), "{e}");
    let e = parse("provider='openai'\nmodel='m'\napi_key_env=''").unwrap_err();
    assert!(e.contains("api_key_env"), "{e}");
}

#[test]
fn req002_ac2_ac4_load_with_env() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("absent.toml");
    let missing_s = missing.display().to_string();
    let e = Config::load_with(&env_of(&[("RKG_CONFIG", &missing_s)]))
        .unwrap_err()
        .to_string();
    assert!(e.contains(&missing_s), "{e}");

    let f = d.path().join("c.toml");
    std::fs::write(&f, "provider='anthropic'\nmodel='m'\napi_key_env='MY_KEY'").unwrap();
    let fs = f.display().to_string();
    let e = Config::load_with(&env_of(&[("RKG_CONFIG", &fs)]))
        .unwrap_err()
        .to_string();
    assert!(e.contains("MY_KEY"), "{e}");
    let e = Config::load_with(&env_of(&[("RKG_CONFIG", &fs), ("MY_KEY", "")]))
        .unwrap_err()
        .to_string();
    assert!(e.contains("MY_KEY"), "{e}");
    let c = Config::load_with(&env_of(&[("RKG_CONFIG", &fs), ("MY_KEY", "secret")])).unwrap();
    assert_eq!(c.provider, Provider::Anthropic);
}

#[test]
fn req003_from_env_with_reads_all_variables() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("fx")).unwrap();
    std::fs::write(
        d.path().join("fx/A.java.json"),
        r#"{"version":2,"summaries":[{"node":"type:A","summary":"s"}],"concepts":[]}"#,
    )
    .unwrap();
    let dir = d.path().join("fx").display().to_string();
    let log = d.path().join("log.txt").display().to_string();
    let env = env_of(&[
        ("RKG_MOCK_DIR", &dir),
        ("RKG_MOCK_LOG", &log),
        ("RKG_MOCK_FAIL_AFTER", "1"),
        ("RKG_MOCK_INVALID_KEY", "1"),
        ("RKG_MOCK_DELAY_MS", "50"),
    ]);
    let m = MockLlm::from_env_with(&env).unwrap();
    let t0 = std::time::Instant::now();
    assert_eq!(ex(&m, "A.java", 1).unwrap().summaries.len(), 1);
    assert!(t0.elapsed() >= std::time::Duration::from_millis(50));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "A.java\t1\n");
    assert!(ex(&m, "A.java", 1).is_err());
    assert!(m.verify_key().is_err());

    for v in ["0", ""] {
        let m = MockLlm::from_env_with(&env_of(&[("RKG_MOCK_INVALID_KEY", v)])).unwrap();
        assert!(m.verify_key().is_ok(), "INVALID_KEY={v:?}");
    }
    let m = MockLlm::from_env_with(&env_of(&[])).unwrap();
    assert!(ex(&m, "A.java", 1).unwrap().summaries.is_empty());
}

#[test]
fn req003_non_numeric_numeric_variables_fail() {
    for var in ["RKG_MOCK_FAIL_AFTER", "RKG_MOCK_DELAY_MS"] {
        let e = MockLlm::from_env_with(&env_of(&[(var, "abc")]))
            .unwrap_err()
            .to_string();
        assert!(e.contains(var), "{e}");
    }
}

#[test]
fn req002_ac7_timeout_secs_default_and_value() {
    assert_eq!(parse("provider='mock'").unwrap().timeout_secs, 300);
    let c = Config::parse("provider='mock'\ntimeout_secs=7", Path::new("/x")).unwrap();
    assert_eq!(c.timeout_secs, 7);
}

#[test]
fn req002_ac7_timeout_secs_invalid_names_key() {
    for bad in ["0", "-3", "1.5", "'abc'"] {
        let m = parse(&format!("provider='mock'\ntimeout_secs={bad}")).unwrap_err();
        assert!(m.contains("timeout_secs"), "{bad}: {m}");
    }
}

#[test]
fn req003_ac5_timeout_files_fail_with_timeout_and_are_logged() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |o| {
        o.timeout_files = vec!["a".into(), "src/b".into()]
    });
    assert!(matches!(ex(&m, "a", 1), Err(rkg::error::Error::Timeout(_))));
    assert!(matches!(
        ex(&m, "src/b", 1),
        Err(rkg::error::Error::Timeout(_))
    ));
    assert!(ex(&m, "c", 1).is_ok());
    let log = std::fs::read_to_string(d.path().join("log")).unwrap();
    assert_eq!(log, "a\t1\nsrc/b\t1\nc\t1\n");
}

#[test]
fn req003_ac5_key_revoked_after_n_extractions() {
    let d = tempfile::tempdir().unwrap();
    let m = mock_with(d.path(), |o| o.key_revoked_after = Some(2));
    assert!(ex(&m, "a", 1).is_ok());
    assert!(ex(&m, "b", 1).is_ok());
    assert!(matches!(
        ex(&m, "c", 1),
        Err(rkg::error::Error::InvalidKey(_))
    ));
    assert!(matches!(
        ex(&m, "d", 1),
        Err(rkg::error::Error::InvalidKey(_))
    ));
    assert!(m.verify_key().is_ok());
}
