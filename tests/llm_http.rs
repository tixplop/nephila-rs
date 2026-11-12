//! Adaptateurs HTTP Anthropic et compatible OpenAI contre un faux serveur (REQ-002).

#[path = "support/fake_server.rs"]
mod fake_server;

use fake_server::FakeServer;
use nephila::error::Error;
use nephila::llm::anthropic::AnthropicLlm;
use nephila::llm::openai::OpenAiLlm;
use nephila::llm::AskReply;
use nephila::llm::Generation;
use nephila::llm::Llm;
use nephila::llm::LotItem;
use nephila::llm::Retry;
use nephila::llm::Tls;

const EXTRACTION: &str =
    r#"{"version":2,"summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]}"#;
const VERSION_1: &str =
    r#"{"version":1,"declares":[{"kind":"type","name":"A","subkind":"class"}],"references":[]}"#;

fn default_generation() -> Generation {
    Generation {
        temperature: None,
        effort: nephila::config::DEFAULT_EFFORT,
        effort_explicit: false,
        thinking: false,
        max_output_tokens: nephila::config::DEFAULT_MAX_OUTPUT_TOKENS,
    }
}

fn a_ids() -> Vec<String> {
    vec!["type:A".to_string()]
}

fn anthropic_reply(text: &str) -> String {
    serde_json::json!({"content":[{"type":"text","text":text}]}).to_string()
}

fn openai_reply(text: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string()
}

fn anthropic(s: &FakeServer) -> AnthropicLlm {
    AnthropicLlm::new(
        Some(s.base_url.clone()),
        "claude-x".into(),
        "sk-test".into(),
        30,
        &Tls::default(),
        1,
        default_generation(),
    )
    .with_retry(Retry::from_millis(1))
}

fn openai(s: &FakeServer, key: Option<&str>) -> OpenAiLlm {
    OpenAiLlm::new(
        Some(s.base_url.clone()),
        "gpt-x".into(),
        key.map(String::from),
        30,
        &Tls::default(),
        1,
        default_generation(),
    )
    .with_retry(Retry::from_millis(1))
}

// ---- AC-002-3 : 401/403 -> erreur de clé, distincte d'une erreur réseau ----

#[test]
fn req002_ac3_anthropic_401_is_invalid_key() {
    let s = FakeServer::start(vec![(401, "{}".into())]);
    assert!(matches!(
        anthropic(&s).enrich("A.java", 1, "x", &a_ids(), &a_ids()),
        Err(Error::InvalidKey(_))
    ));
}

#[test]
fn req002_ac3_openai_403_is_invalid_key() {
    let s = FakeServer::start(vec![(403, "{}".into())]);
    assert!(matches!(
        openai(&s, Some("k")).enrich("A.java", 1, "x", &a_ids(), &a_ids()),
        Err(Error::InvalidKey(_))
    ));
}

#[test]
fn req002_ac3_server_error_is_network_not_key() {
    // 5 envois par appel (REQ-052) : cinq 500 pour le premier, cinq 429 pour le second.
    let mut replies: Vec<(u16, String)> = (0..5).map(|_| (500, "{}".to_string())).collect();
    replies.extend((0..5).map(|_| (429, "{}".to_string())));
    let s = FakeServer::start(replies);
    assert!(matches!(
        anthropic(&s).enrich("A.java", 1, "x", &a_ids(), &a_ids()),
        Err(Error::Network(_))
    ));
    // Un 429 définitif est une saturation (REQ-093, DEC-099), comptée comme une erreur réseau.
    assert!(matches!(
        openai(&s, None).enrich("A.java", 1, "x", &a_ids(), &a_ids()),
        Err(Error::Saturated(_))
    ));
}

#[test]
fn req002_ac3_connection_refused_is_network() {
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let llm = AnthropicLlm::new(
        Some(url),
        "m".into(),
        "k".into(),
        30,
        &Tls::default(),
        1,
        default_generation(),
    )
    .with_retry(Retry::from_millis(1));
    assert!(matches!(
        llm.enrich("A.java", 1, "x", &a_ids(), &a_ids()),
        Err(Error::Network(_))
    ));
}

#[test]
fn req002_ac3_extract_401_is_invalid_key() {
    let s = FakeServer::start(vec![(401, "{}".into())]);
    assert!(matches!(
        anthropic(&s).enrich("A.java", 1, "class A {}", &a_ids(), &a_ids()),
        Err(Error::InvalidKey(_))
    ));
}

// ---- AC-002-5 : requêtes conformes au protocole ----

#[test]
fn req002_ac5_anthropic_extract_request_and_response() {
    let s = FakeServer::start(vec![(200, anthropic_reply(EXTRACTION))]);
    let ex = anthropic(&s)
        .enrich("src/A.java", 2, "class A {}", &a_ids(), &a_ids())
        .unwrap();
    assert_eq!(ex.summaries.len(), 1);
    let r = &s.requests()[0];
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("POST", "/v1/messages")
    );
    assert_eq!(r.headers["x-api-key"], "sk-test");
    assert!(r.headers.contains_key("anthropic-version"));
    assert!(r.headers["content-type"].starts_with("application/json"));
    let b: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(b["model"], "claude-x");
    assert!(b["max_tokens"].as_u64().unwrap() > 0);
    assert!(b["system"].as_str().unwrap().contains("JSON"));
    assert_eq!(b["messages"][0]["role"], "user");
    assert!(b["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("class A {}"));
}

#[test]
fn req002_ac5_openai_extract_request_and_response() {
    let s = FakeServer::start(vec![(200, openai_reply(EXTRACTION))]);
    let ex = openai(&s, Some("sk-o"))
        .enrich("A.java", 1, "class A {}", &a_ids(), &a_ids())
        .unwrap();
    assert_eq!(ex.summaries[0].0, "type:A");
    let r = &s.requests()[0];
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("POST", "/v1/chat/completions")
    );
    assert_eq!(r.headers["authorization"], "Bearer sk-o");
    let b: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(b["model"], "gpt-x");
    assert_eq!(b["messages"][0]["role"], "system");
    assert_eq!(b["messages"][1]["role"], "user");
    assert!(b["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("class A {}"));
}

#[test]
fn req002_ac5_openai_key_is_optional_for_local_model() {
    let s = FakeServer::start(vec![(200, openai_reply(EXTRACTION))]);
    openai(&s, None)
        .enrich("A.java", 1, "x", &a_ids(), &a_ids())
        .unwrap();
    assert!(!s.requests()[0].headers.contains_key("authorization"));
}

#[test]
fn req002_ac5_base_url_trailing_slash_is_tolerated() {
    let s = FakeServer::start(vec![(200, anthropic_reply(EXTRACTION))]);
    let llm = AnthropicLlm::new(
        Some(format!("{}/", s.base_url)),
        "m".into(),
        "k".into(),
        30,
        &Tls::default(),
        1,
        default_generation(),
    );
    llm.enrich("A.java", 1, "x", &a_ids(), &a_ids()).unwrap();
    assert_eq!(s.requests()[0].path, "/v1/messages");
}

#[test]
fn req002_ac5_json_is_extracted_from_fenced_text() {
    let fenced = format!("Voici :\n```json\n{EXTRACTION}\n```\n");
    let s = FakeServer::start(vec![(200, anthropic_reply(&fenced))]);
    assert_eq!(
        anthropic(&s)
            .enrich("A.java", 1, "x", &a_ids(), &a_ids())
            .unwrap()
            .summaries
            .len(),
        1
    );
}

#[test]
fn req002_ac5_ask_both_providers() {
    let reply = r#"{"found":true,"answer":"oui","nodes":["type:A"]}"#;
    let s = FakeServer::start(vec![(200, anthropic_reply(reply))]);
    let a = anthropic(&s).ask("q ?", "ctx").unwrap();
    assert_eq!(
        a,
        AskReply::Found {
            answer: "oui".into(),
            nodes: vec!["type:A".into()]
        }
    );
    let s = FakeServer::start(vec![(200, openai_reply(r#"{"found":false}"#))]);
    assert_eq!(
        openai(&s, None).ask("q ?", "ctx").unwrap(),
        AskReply::NotFound
    );
}

// ---- réponses invalides : erreur d'appel, ni clé ni réseau ----

#[test]
fn req002_invalid_responses_are_message_errors() {
    let s = FakeServer::start(vec![
        (200, anthropic_reply("pas du json")),
        (
            200,
            anthropic_reply(r#"{"version":9,"declares":[],"references":[]}"#),
        ),
        (200, r#"{"content":[]}"#.into()),
        (200, "<html>".into()),
    ]);
    let llm = anthropic(&s);
    for _ in 0..4 {
        assert!(matches!(
            llm.enrich("A.java", 1, "x", &a_ids(), &a_ids()),
            Err(Error::Format(_))
        ));
    }
    let s = FakeServer::start(vec![(200, r#"{"choices":[]}"#.into())]);
    assert!(matches!(
        openai(&s, None).enrich("A.java", 1, "x", &a_ids(), &a_ids()),
        Err(Error::Format(_))
    ));
}

// ---- AC-002-8 : délai d'appel réglable, erreur distincte ----

#[test]
fn req002_ac8_slow_server_gives_timeout_error() {
    let s = FakeServer::start_with_delays(vec![(200, anthropic_reply(EXTRACTION), 2000)]);
    let llm = AnthropicLlm::new(
        Some(s.base_url.clone()),
        "m".into(),
        "k".into(),
        1,
        &Tls::default(),
        1,
        default_generation(),
    );
    let t = std::time::Instant::now();
    let r = llm.enrich("A.java", 1, "class A {}", &a_ids(), &a_ids());
    assert!(matches!(r, Err(Error::Timeout(_))), "{r:?}");
    assert!(t.elapsed() < std::time::Duration::from_millis(2000));
}

#[test]
fn req002_ac8_openai_slow_server_gives_timeout_error() {
    let s = FakeServer::start_with_delays(vec![(200, openai_reply(EXTRACTION), 2000)]);
    let llm = OpenAiLlm::new(
        Some(s.base_url.clone()),
        "m".into(),
        None,
        1,
        &Tls::default(),
        1,
        default_generation(),
    );
    let r = llm.enrich("A.java", 1, "x", &a_ids(), &a_ids());
    assert!(matches!(r, Err(Error::Timeout(_))), "{r:?}");
}

#[test]
fn req002_ac8_fast_server_succeeds_within_timeout() {
    let s = FakeServer::start_with_delays(vec![(200, anthropic_reply(EXTRACTION), 500)]);
    let llm = AnthropicLlm::new(
        Some(s.base_url.clone()),
        "m".into(),
        "k".into(),
        1,
        &Tls::default(),
        1,
        default_generation(),
    );
    assert_eq!(
        llm.enrich("A.java", 1, "class A {}", &a_ids(), &a_ids())
            .unwrap()
            .summaries
            .len(),
        1
    );
}

#[test]
fn req002_ac8_config_timeout_secs_reaches_http_agent() {
    let s = FakeServer::start_with_delays(vec![(200, openai_reply(EXTRACTION), 2000)]);
    let text = format!(
        "provider='openai'\nenrich_code = true\nmodel='m'\nbase_url='{}'\ntimeout_secs=1\n",
        s.base_url
    );
    let cfg =
        nephila::config::Config::parse(&text, std::path::Path::new("/x/config.toml")).unwrap();
    let llm = nephila::llm::build_llm(&cfg).unwrap();
    let t = std::time::Instant::now();
    let r = llm.enrich("A.java", 1, "x", &a_ids(), &a_ids());
    assert!(matches!(r, Err(Error::Timeout(_))), "{r:?}");
    assert!(t.elapsed() < std::time::Duration::from_secs(2));
}

// ---- AC-024-8 : requête d'enrichissement (schéma version 2) ----

const ENRICHMENT: &str =
    r#"{"version":2,"summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]}"#;

fn check_enrich_request(content: &str) {
    assert!(content.contains("class A {}"), "{content}");
    assert!(content.contains("type:A"), "{content}");
    assert!(content.contains("method:A.f"), "{content}");
}

#[test]
fn req024_ac8_anthropic_enrich_request_carries_content_nodes_and_schema() {
    let s = FakeServer::start(vec![(200, anthropic_reply(ENRICHMENT))]);
    let ids = vec!["type:A".to_string(), "method:A.f".to_string()];
    let e = anthropic(&s)
        .enrich("A.java", 1, "class A {}", &ids, &ids)
        .unwrap();
    assert_eq!(e.summaries, vec![("type:A".into(), "Le A.".into())]);
    let b: serde_json::Value = serde_json::from_str(&s.requests()[0].body).unwrap();
    check_enrich_request(b["messages"][0]["content"].as_str().unwrap());
    let sys = b["system"].as_str().unwrap();
    assert!(!sys.contains("version") && sys.contains("summaries") && sys.contains("concepts"));
    check_system_prompt(sys);
}

#[test]
fn req024_ac8_openai_enrich_request_carries_content_nodes_and_schema() {
    let s = FakeServer::start(vec![(200, openai_reply(ENRICHMENT))]);
    let ids = vec!["type:A".to_string(), "method:A.f".to_string()];
    openai(&s, None)
        .enrich("A.java", 1, "class A {}", &ids, &ids)
        .unwrap();
    let b: serde_json::Value = serde_json::from_str(&s.requests()[0].body).unwrap();
    check_enrich_request(b["messages"][1]["content"].as_str().unwrap());
    let sys = b["messages"][0]["content"].as_str().unwrap();
    assert!(!sys.contains("version") && sys.contains("summaries") && sys.contains("concepts"));
    check_system_prompt(sys);
}

#[test]
fn req024_ac5_enrich_version_1_is_non_conforming() {
    let s = FakeServer::start(vec![(200, anthropic_reply(VERSION_1))]);
    assert!(matches!(
        anthropic(&s).enrich("A.java", 1, "x", &[], &[]),
        Err(Error::Format(_))
    ));
}

// ---- AC-027-6 : appel de lot (schéma version 3) au niveau fournisseur ----

const LOT_REPLY: &str = r#"{"version":3,"files":[
  {"file":"A.java","summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]},
  {"file":"B.java","summaries":[{"node":"type:B","summary":"Le B."},{"node":"type:A","summary":"x"}],"concepts":[]}]}"#;

fn lot_items() -> Vec<LotItem> {
    vec![
        LotItem {
            file: "A.java".into(),
            text: "class A {}".into(),
            node_ids: vec!["type:A".into(), "method:A.f".into()],
        },
        LotItem {
            file: "B.java".into(),
            text: "class B {}".into(),
            node_ids: vec!["type:B".into()],
        },
    ]
}

/// REQ-171 : types de fichiers tirés de `FileType` et angle par genre dans le prompt système.
fn check_system_prompt(sys: &str) {
    assert!(
        sys.contains(&nephila::llm::enrich::file_types_phrase()),
        "{sys}"
    );
    assert!(sys.contains(nephila::llm::enrich::KIND_RULES), "{sys}");
    // REQ-172 : configuration sans la clé `summary_language`.
    let rules = nephila::llm::enrich::language_rules(nephila::config::DEFAULT_SUMMARY_LANGUAGE);
    assert!(sys.contains(&rules), "{sys}");
    // REQ-175 : règles de choix et de nommage des concepts.
    assert!(sys.contains(nephila::llm::enrich::CONCEPT_RULES), "{sys}");
}

fn check_lot_request(system: &str, user: &str) {
    for needle in [
        "A.java",
        "B.java",
        "class A {}",
        "class B {}",
        "type:A",
        "method:A.f",
        "type:B",
    ] {
        assert!(user.contains(needle), "{needle} : {user}");
    }
    assert!(
        !system.contains("version") && system.contains("files"),
        "{system}"
    );
    check_system_prompt(system);
}

fn check_lot_result(r: Vec<(String, nephila::error::Result<nephila::llm::Enrichment>)>) {
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].0, "A.java");
    let a = r[0].1.as_ref().unwrap();
    assert_eq!(a.summaries, vec![("type:A".into(), "Le A.".into())]);
    let b = r[1].1.as_ref().unwrap();
    assert_eq!(b.summaries, vec![("type:B".into(), "Le B.".into())]);
    assert_eq!(b.rejected.len(), 1);
}

#[test]
fn req027_ac6_anthropic_lot_single_request_applied_per_file() {
    let s = FakeServer::start(vec![(200, anthropic_reply(LOT_REPLY))]);
    let r = anthropic(&s).enrich_lot(&lot_items()).unwrap();
    assert_eq!(s.requests().len(), 1);
    let b: serde_json::Value = serde_json::from_str(&s.requests()[0].body).unwrap();
    check_lot_request(
        b["system"].as_str().unwrap(),
        b["messages"][0]["content"].as_str().unwrap(),
    );
    check_lot_result(r);
}

#[test]
fn req027_ac6_openai_lot_single_request_applied_per_file() {
    let s = FakeServer::start(vec![(200, openai_reply(LOT_REPLY))]);
    let r = openai(&s, None).enrich_lot(&lot_items()).unwrap();
    assert_eq!(s.requests().len(), 1);
    let b: serde_json::Value = serde_json::from_str(&s.requests()[0].body).unwrap();
    check_lot_request(
        b["messages"][0]["content"].as_str().unwrap(),
        b["messages"][1]["content"].as_str().unwrap(),
    );
    check_lot_result(r);
}

/// REQ-172 : `llm` construit par `build_llm` depuis la configuration personnelle
/// `summary_language = "anglais"`, complétée ou non d'un `.nephila.toml` (`project`).
fn llm_with_language(provider: &str, base_url: &str, project: Option<&str>) -> Box<dyn Llm> {
    std::env::set_var("NEPHILA_TEST_KEY_172", "sk-test");
    let text = format!(
        "provider='{provider}'\nenrich_code = true\nmodel='m'\nbase_url='{base_url}'\napi_key_env='NEPHILA_TEST_KEY_172'\nsummary_language = \"anglais\"\n"
    );
    let mut cfg =
        nephila::config::Config::parse(&text, std::path::Path::new("/x/config.toml")).unwrap();
    if let Some(p) = project {
        let table: toml::Table = p.parse().unwrap();
        let settings =
            nephila::config::ProjectSettings::from_table(&table, "./.nephila.toml").unwrap();
        cfg = cfg.with_project(&settings, None, None).0;
    }
    nephila::llm::build_llm(&cfg).unwrap()
}

#[test]
fn req172_ac5_enrich_and_lot_requests_carry_summary_language() {
    for (provider, project, language) in [
        ("anthropic", None, "anglais"),
        ("openai", None, "anglais"),
        (
            "anthropic",
            Some("summary_language = \"espagnol\"\n"),
            "espagnol",
        ),
        (
            "openai",
            Some("summary_language = \"espagnol\"\n"),
            "espagnol",
        ),
    ] {
        let anth = provider == "anthropic";
        let ids = vec!["type:A".to_string(), "method:A.f".to_string()];
        let reply = |t: &str| {
            if anth {
                anthropic_reply(t)
            } else {
                openai_reply(t)
            }
        };
        let s = FakeServer::start(vec![(200, reply(ENRICHMENT)), (200, reply(LOT_REPLY))]);
        let llm = llm_with_language(provider, &s.base_url, project);
        llm.enrich("A.java", 1, "class A {}", &ids, &ids).unwrap();
        llm.enrich_lot(&lot_items()).unwrap();
        let reqs = s.requests();
        assert_eq!(reqs.len(), 2, "{provider}");
        for r in &reqs {
            let b: serde_json::Value = serde_json::from_str(&r.body).unwrap();
            let sys = if anth {
                b["system"].as_str().unwrap()
            } else {
                b["messages"][0]["content"].as_str().unwrap()
            };
            let rules = nephila::llm::enrich::language_rules(language);
            assert!(sys.contains(&rules), "{provider} {language} : {sys}");
            assert!(
                !sys.contains("en français"),
                "{provider} {language} : {sys}"
            );
        }
    }
}

#[test]
fn req027_ac8_lot_version_2_is_non_conforming_and_errors_classified() {
    let s = FakeServer::start(vec![
        (200, openai_reply(EXTRACTION)),
        (401, "{}".into()),
        (500, "{}".into()),
        (500, "{}".into()),
        (500, "{}".into()),
        (500, "{}".into()),
        (500, "{}".into()),
    ]);
    let o = openai(&s, None);
    assert!(matches!(o.enrich_lot(&lot_items()), Err(Error::Format(_))));
    assert!(matches!(
        o.enrich_lot(&lot_items()),
        Err(Error::InvalidKey(_))
    ));
    assert!(matches!(o.enrich_lot(&lot_items()), Err(Error::Network(_))));
}

// ---- AC-024-10 : liste de nœuds d'un fichier de données, faux serveur openai ----

#[test]
fn req024_ac10_openai_node_list_of_json_document_has_no_key() {
    let reply = r#"{"version":2,"summaries":[{"node":"document:config/app.json","summary":"Configuration."}],"concepts":[]}"#;
    let s = FakeServer::start(vec![(200, openai_reply(reply))]);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("config")).unwrap();
    std::fs::write(
        repo.join("config/app.json"),
        r#"{"server": {"port": 8080, "handler": "com.acme.core.OrderService"}}"#,
    )
    .unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T061_KEY\"\nconcurrency = 1\n",
            s.base_url
        ),
    )
    .unwrap();
    let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
        .arg("-C")
        .arg(&repo)
        .arg("build")
        .env("NEPHILA_CONFIG", &config)
        .env("NEPHILA_T061_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let posts: Vec<_> = s
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    assert!(posts[0].body.contains("document:config/app.json"));
    assert!(!posts[0].body.contains("key:"), "{}", posts[0].body);
}

// ---- AC-024-11 : code TypeScript et package.json, faux serveur openai ----

/// Construit un dépôt à un fichier, lance `build` contre le faux serveur et rend la requête POST.
fn openai_build_one_file(rel: &str, text: &str, reply: &str) -> String {
    let s = FakeServer::start(vec![(200, openai_reply(reply))]);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let p = repo.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T068_KEY\"\nconcurrency = 1\n",
            s.base_url
        ),
    )
    .unwrap();
    let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
        .arg("-C")
        .arg(&repo)
        .arg("build")
        .env("NEPHILA_CONFIG", &config)
        .env("NEPHILA_T068_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let posts: Vec<_> = s
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    posts[0].body.clone()
}

#[test]
fn req024_ac11_openai_node_list_of_typescript_has_class_not_package() {
    let reply = r#"{"version":2,"summaries":[{"node":"type:web/src/api/client.ApiClient","summary":"Client."}],"concepts":[]}"#;
    let body = openai_build_one_file(
        "web/src/api/client.ts",
        "export class ApiClient { fetchOrder() {} }\n",
        reply,
    );
    assert!(body.contains("type:web/src/api/client.ApiClient"), "{body}");
    assert!(
        body.contains("method:web/src/api/client.ApiClient.fetchOrder"),
        "{body}"
    );
    assert!(!body.contains("package:"), "{body}");
    assert!(body.contains("TypeScript"), "{body}");
    assert!(body.contains("JavaScript"), "{body}");
}

#[test]
fn req024_ac11_openai_node_list_of_package_json_is_its_module() {
    let reply = r#"{"version":2,"summaries":[{"node":"module:@acme/web","summary":"Module web."}],"concepts":[]}"#;
    let body = openai_build_one_file(
        "web/package.json",
        r#"{"name": "@acme/web", "dependencies": {"react": "^18"}}"#,
        reply,
    );
    assert!(body.contains("module:@acme/web"), "{body}");
    assert!(!body.contains("dependency:"), "{body}");
    assert!(!body.contains("package:"), "{body}");
}

// ---- AC-176-4 et AC-176-5 : texte autour de l'objet JSON, sans renvoi ----

#[test]
fn req176_ac4_trailing_remark_is_accepted_by_both_providers() {
    let enrich_text = format!("{EXTRACTION}\nRemarque : {{x}}");
    let lot_text = format!("{LOT_REPLY}\nRemarque : {{x}}");
    let ask_text = "{\"found\":true,\"answer\":\"oui\",\"nodes\":[\"type:A\"]}\n{x}";
    for provider in ["anthropic", "openai"] {
        let reply = |t: &str| {
            if provider == "anthropic" {
                anthropic_reply(t)
            } else {
                openai_reply(t)
            }
        };
        let s = FakeServer::start(vec![(200, reply(&enrich_text))]);
        let llm: Box<dyn Llm> = if provider == "anthropic" {
            Box::new(anthropic(&s))
        } else {
            Box::new(openai(&s, None))
        };
        let e = llm.enrich("A.java", 1, "x", &a_ids(), &a_ids()).unwrap();
        assert_eq!(e.summaries.len(), 1, "{provider}");
        assert_eq!(s.requests().len(), 1, "{provider}");

        let s = FakeServer::start(vec![(200, reply(&lot_text))]);
        let llm: Box<dyn Llm> = if provider == "anthropic" {
            Box::new(anthropic(&s))
        } else {
            Box::new(openai(&s, None))
        };
        check_lot_result(llm.enrich_lot(&lot_items()).unwrap());
        assert_eq!(s.requests().len(), 1, "{provider}");

        let s = FakeServer::start(vec![(200, reply(ask_text))]);
        let llm: Box<dyn Llm> = if provider == "anthropic" {
            Box::new(anthropic(&s))
        } else {
            Box::new(openai(&s, None))
        };
        assert_eq!(
            llm.ask("q ?", "ctx").unwrap(),
            AskReply::Found {
                answer: "oui".into(),
                nodes: vec!["type:A".into()]
            },
            "{provider}"
        );
        assert_eq!(s.requests().len(), 1, "{provider}");
    }
}

#[test]
fn req176_ac5_build_accepts_trailing_remark_without_resend() {
    let reply = format!(
        "{}\nNote : {{x}}",
        r#"{"version":2,"summaries":[{"node":"type:web/src/api/client.ApiClient","summary":"Client."}],"concepts":[]}"#
    );
    // `openai_build_one_file` vérifie le code 0 et une seule requête POST.
    openai_build_one_file(
        "web/src/api/client.ts",
        "export class ApiClient {}\n",
        &reply,
    );
}

// ---- REQ-201 : réponse sans `version` acceptée ----

/// `build` par le binaire contre le faux serveur : `docs` documents, réponse sans `version`.
fn build_without_version(provider: &str, docs: usize) {
    let names = ["a.md", "b.md"];
    let summary = |i: usize| {
        format!(
            r#"{{"node":"document:{}","summary":"Zorglub {i}."}}"#,
            names[i]
        )
    };
    let reply = if docs == 1 {
        format!(r#"{{"summaries":[{}],"concepts":[]}}"#, summary(0))
    } else {
        let entry = |i: usize| {
            format!(
                r#"{{"file":"{}","summaries":[{}],"concepts":[]}}"#,
                names[i],
                summary(i)
            )
        };
        format!(r#"{{"files":[{},{}]}}"#, entry(0), entry(1))
    };
    let body = if provider == "openai" {
        openai_reply(&reply)
    } else {
        anthropic_reply(&reply)
    };
    let s = FakeServer::start(vec![(200, body)]);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for n in &names[..docs] {
        std::fs::write(repo.join(n), format!("# Titre\n\nTexte de {n}.\n")).unwrap();
    }
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "provider = \"{provider}\"\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T293_KEY\"\nconcurrency = 1\nbatch_files = {docs}\n",
            s.base_url
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("NEPHILA_CONFIG", &config)
            .env("NEPHILA_T293_KEY", "sk-test")
            .output()
            .unwrap()
    };
    let out = run(&["build"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{provider} {docs} : {err}");
    assert!(!err.contains("avertissement:"), "{provider} {docs} : {err}");
    let posts = s
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(posts, 1, "{provider} {docs}");
    let found = String::from_utf8_lossy(&run(&["query", "search", "Zorglub"]).stdout).to_string();
    for n in &names[..docs] {
        assert!(found.contains(n), "{provider} {docs} : {found}");
    }
}

#[test]
fn req201_ac4_openai_reply_without_version_single_file_and_lot() {
    build_without_version("openai", 1);
    build_without_version("openai", 2);
}

#[test]
fn req201_ac4_anthropic_reply_without_version_single_file_and_lot() {
    build_without_version("anthropic", 1);
    build_without_version("anthropic", 2);
}

// ---- REQ-278 : modèle servi enregistré avec l'enrichissement ----

/// Réponse portant `model` (si donné) et `text`, au format du fournisseur.
fn served_body(provider: &str, model: &str, text: &str) -> String {
    let mut v: serde_json::Value = if provider == "openai" {
        serde_json::from_str(&openai_reply(text)).unwrap()
    } else {
        serde_json::from_str(&anthropic_reply(text)).unwrap()
    };
    v["model"] = serde_json::json!(model);
    v.to_string()
}

struct ServedEnv {
    _tmp: tempfile::TempDir,
    repo: std::path::PathBuf,
    config: std::path::PathBuf,
}

impl ServedEnv {
    /// Dépôt de documents `files` ; configuration vers `url` avec `model = "claude-x"`.
    fn new(provider: &str, url: &str, extra: &str, files: &[(&str, String)]) -> ServedEnv {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for (n, text) in files {
            std::fs::write(repo.join(n), text).unwrap();
        }
        let config = tmp.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nmodel = \"claude-x\"\nbase_url = \"{url}\"\napi_key_env = \"NEPHILA_T378_KEY\"\nconcurrency = 1\n{extra}"
            ),
        )
        .unwrap();
        ServedEnv {
            _tmp: tmp,
            repo,
            config,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        std::process::Command::new(assert_cmd::cargo::cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T378_KEY", "sk-test")
            .output()
            .unwrap()
    }

    /// `(chemin, model, served_model)` de chaque entrée, par chemin.
    fn entries(&self) -> Vec<(String, String, Option<String>)> {
        let conn = rusqlite::Connection::open(self.repo.join(".nephila/graph.db")).unwrap();
        let mut q = conn
            .prepare("SELECT path, model, served_model FROM enrichments ORDER BY path")
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }
}

fn posts(s: &FakeServer) -> usize {
    s.requests().iter().filter(|r| r.method == "POST").count()
}

const NO_SUMMARY: &str = r#"{"version":2,"summaries":[],"concepts":[]}"#;

fn doc(n: &str) -> (&'static str, String) {
    let name: &'static str = match n {
        "a" => "a.md",
        "b" => "b.md",
        _ => "c.md",
    };
    (name, format!("# Document {n}\n\nTexte du document {n}.\n"))
}

fn lot_reply(names: &[&str]) -> String {
    let files: Vec<_> = names
        .iter()
        .map(|n| serde_json::json!({"file": format!("{n}.md"), "summaries": [], "concepts": []}))
        .collect();
    serde_json::json!({"version": 3, "files": files}).to_string()
}

#[test]
fn req278_ac2_ac5_entry_records_served_model_and_a_new_name_reenriches_nothing() {
    for provider in ["anthropic", "openai"] {
        let s = FakeServer::start(vec![
            (200, served_body(provider, "claude-x-20250929", NO_SUMMARY)),
            (200, served_body(provider, "claude-x-20260101", NO_SUMMARY)),
        ]);
        let env = ServedEnv::new(provider, &s.base_url, "", &[doc("a")]);
        let o = env.run(&["build"]);
        assert!(o.status.success(), "{provider}: {:?}", o);
        assert_eq!(
            env.entries(),
            vec![(
                "a.md".to_string(),
                "claude-x".to_string(),
                Some("claude-x-20250929".to_string())
            )],
            "{provider}"
        );
        // AC-278-5 : le nom servi change derrière l'alias, rien n'est renvoyé ni réécrit.
        let before = posts(&s);
        let o = env.run(&["update"]);
        assert!(o.status.success(), "{provider}");
        assert!(
            String::from_utf8_lossy(&o.stdout).contains("graphe à jour"),
            "{provider}: {}",
            String::from_utf8_lossy(&o.stdout)
        );
        assert_eq!(posts(&s), before, "{provider}");
        assert_eq!(
            env.entries()[0].2.as_deref(),
            Some("claude-x-20250929"),
            "{provider}"
        );
    }
}

#[test]
fn req278_ac3_lot_big_file_and_distinct_names_are_recorded() {
    for provider in ["anthropic", "openai"] {
        // Lot de 3 fichiers : le même nom pour chacun.
        let s = FakeServer::start(vec![(
            200,
            served_body(provider, "s1", &lot_reply(&["a", "b", "c"])),
        )]);
        let env = ServedEnv::new(
            provider,
            &s.base_url,
            "batch_files = 10\n",
            &[doc("a"), doc("b"), doc("c")],
        );
        assert!(env.run(&["build"]).status.success(), "{provider}");
        assert_eq!(posts(&s), 1, "{provider}");
        let got: Vec<_> = env.entries().into_iter().map(|e| e.2).collect();
        assert_eq!(got, vec![Some("s1".to_string()); 3], "{provider}");

        // Gros fichier de 2 appels : noms distincts dans l'ordre des appels, sans doublon.
        for (names, want) in [(["s1", "s2"], "s1, s2"), (["s1", "s1"], "s1")] {
            let s = FakeServer::start(
                names
                    .iter()
                    .map(|n| (200, served_body(provider, n, NO_SUMMARY)))
                    .collect(),
            );
            let big = format!(
                "# Section1\n\n{}\n\n# Section2\n\n{}\n\n",
                "mot ".repeat(40),
                "mot ".repeat(40)
            );
            let env = ServedEnv::new(
                provider,
                &s.base_url,
                "max_chunk_chars = 200\n",
                &[("a.md", big)],
            );
            assert!(env.run(&["build"]).status.success(), "{provider}");
            assert_eq!(posts(&s), 2, "{provider} {names:?}");
            assert_eq!(
                env.entries()[0].2.as_deref(),
                Some(want),
                "{provider} {names:?}"
            );
        }

        // Aucun nom dans la réponse : valeur vide.
        let s = FakeServer::start(vec![(
            200,
            if provider == "openai" {
                openai_reply(NO_SUMMARY)
            } else {
                anthropic_reply(NO_SUMMARY)
            },
        )]);
        let env = ServedEnv::new(provider, &s.base_url, "", &[doc("a")]);
        assert!(env.run(&["build"]).status.success(), "{provider}");
        assert_eq!(env.entries()[0].2, None, "{provider}");
    }
}
