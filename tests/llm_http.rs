//! Adaptateurs HTTP Anthropic et compatible OpenAI contre un faux serveur (REQ-002).

#[path = "support/fake_server.rs"]
mod fake_server;

use fake_server::FakeServer;
use nephila::error::Error;
use nephila::llm::anthropic::AnthropicLlm;
use nephila::llm::openai::OpenAiLlm;
use nephila::llm::AskReply;
use nephila::llm::Llm;
use nephila::llm::LotItem;
use nephila::llm::Tls;

const EXTRACTION: &str =
    r#"{"version":2,"summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]}"#;
const VERSION_1: &str =
    r#"{"version":1,"declares":[{"kind":"type","name":"A","subkind":"class"}],"references":[]}"#;

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
    )
}

fn openai(s: &FakeServer, key: Option<&str>) -> OpenAiLlm {
    OpenAiLlm::new(
        Some(s.base_url.clone()),
        "gpt-x".into(),
        key.map(String::from),
        30,
        &Tls::default(),
    )
}

// ---- AC-002-3 : 401/403 -> erreur de clé, distincte d'une erreur réseau ----

#[test]
fn req002_ac3_anthropic_401_is_invalid_key() {
    let s = FakeServer::start(vec![(401, "{}".into())]);
    assert!(matches!(
        anthropic(&s).verify_key(),
        Err(Error::InvalidKey(_))
    ));
}

#[test]
fn req002_ac3_openai_403_is_invalid_key() {
    let s = FakeServer::start(vec![(403, "{}".into())]);
    assert!(matches!(
        openai(&s, Some("k")).verify_key(),
        Err(Error::InvalidKey(_))
    ));
}

#[test]
fn req002_ac3_server_error_is_network_not_key() {
    let s = FakeServer::start(vec![(500, "{}".into()), (429, "{}".into())]);
    assert!(matches!(anthropic(&s).verify_key(), Err(Error::Network(_))));
    assert!(matches!(
        openai(&s, None).verify_key(),
        Err(Error::Network(_))
    ));
}

#[test]
fn req002_ac3_connection_refused_is_network() {
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let llm = AnthropicLlm::new(Some(url), "m".into(), "k".into(), 30, &Tls::default());
    assert!(matches!(llm.verify_key(), Err(Error::Network(_))));
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
fn req002_ac5_anthropic_verify_key_request() {
    let s = FakeServer::start(vec![(200, r#"{"data":[]}"#.into())]);
    anthropic(&s).verify_key().unwrap();
    let r = &s.requests()[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/v1/models"));
    assert_eq!(r.headers["x-api-key"], "sk-test");
    assert!(r.headers.contains_key("anthropic-version"));
}

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
fn req002_ac5_openai_verify_key_request() {
    let s = FakeServer::start(vec![(200, r#"{"data":[]}"#.into())]);
    openai(&s, Some("sk-o")).verify_key().unwrap();
    let r = &s.requests()[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/v1/models"));
    assert_eq!(r.headers["authorization"], "Bearer sk-o");
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
    let s = FakeServer::start(vec![(200, r#"{"data":[]}"#.into())]);
    openai(&s, None).verify_key().unwrap();
    assert!(!s.requests()[0].headers.contains_key("authorization"));
}

#[test]
fn req002_ac5_base_url_trailing_slash_is_tolerated() {
    let s = FakeServer::start(vec![(200, "{}".into())]);
    let llm = AnthropicLlm::new(
        Some(format!("{}/", s.base_url)),
        "m".into(),
        "k".into(),
        30,
        &Tls::default(),
    );
    llm.verify_key().unwrap();
    assert_eq!(s.requests()[0].path, "/v1/models");
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
    );
    let r = llm.verify_key();
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
        "provider='openai'\nmodel='m'\nbase_url='{}'\ntimeout_secs=1\n",
        s.base_url
    );
    let cfg =
        nephila::config::Config::parse(&text, std::path::Path::new("/x/config.toml")).unwrap();
    let llm = nephila::llm::build_llm(&cfg).unwrap();
    let t = std::time::Instant::now();
    let r = llm.verify_key();
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
    assert!(sys.contains("version\": 2") && sys.contains("summaries") && sys.contains("concepts"));
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
    assert!(sys.contains("version\": 2") && sys.contains("summaries") && sys.contains("concepts"));
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
        system.contains("version\": 3") && system.contains("files"),
        "{system}"
    );
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

#[test]
fn req027_ac8_lot_version_2_is_non_conforming_and_errors_classified() {
    let s = FakeServer::start(vec![
        (200, openai_reply(EXTRACTION)),
        (401, "{}".into()),
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
    let s = FakeServer::start(vec![(200, "{}".into()), (200, openai_reply(reply))]);
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
            "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T061_KEY\"\nconcurrency = 1\n",
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
    let s = FakeServer::start(vec![(200, "{}".into()), (200, openai_reply(reply))]);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let p = repo.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T068_KEY\"\nconcurrency = 1\n",
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
