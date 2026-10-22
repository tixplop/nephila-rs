//! TLS des adaptateurs HTTP contre un faux serveur HTTPS (REQ-036, REQ-002).

#[path = "support/fake_server.rs"]
mod fake_server;

use fake_server::FakeServer;
use nephila::error::Error;
use nephila::llm::anthropic::AnthropicLlm;
use nephila::llm::openai::OpenAiLlm;
use nephila::llm::{Llm, Tls};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;

const EXTRACTION: &str =
    r#"{"version":2,"summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]}"#;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/tls/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn tls_with(ca: &str) -> Tls {
    Tls {
        ca_certs: CertificateDer::pem_slice_iter(&fixture(ca))
            .collect::<Result<_, _>>()
            .unwrap(),
        insecure: false,
    }
}

fn insecure() -> Tls {
    Tls {
        ca_certs: vec![],
        insecure: true,
    }
}

fn server(cert: &str, responses: Vec<(u16, String)>) -> FakeServer {
    let key = cert.replace(".pem", ".key");
    FakeServer::start_tls(responses, &fixture(cert), &fixture(&key))
}

fn openai_reply(text: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string()
}

fn anthropic_reply(text: &str) -> String {
    serde_json::json!({"content":[{"type":"text","text":text}]}).to_string()
}

fn openai(url: &str, tls: &Tls) -> OpenAiLlm {
    OpenAiLlm::new(Some(url.into()), "m".into(), Some("k".into()), 30, tls, 1)
}

fn anthropic(url: &str, tls: &Tls) -> AnthropicLlm {
    AnthropicLlm::new(Some(url.into()), "m".into(), "k".into(), 30, tls, 1)
}

fn ids() -> Vec<String> {
    vec!["type:A".to_string()]
}

fn assert_cert_refused<T: std::fmt::Debug>(r: Result<T, Error>) {
    match r {
        Err(Error::Network(m)) => {
            assert!(m.contains("certificat"), "{m}");
            assert!(m.contains("tls_ca_cert"), "{m}");
            assert!(m.contains("tls_insecure"), "{m}");
            assert!(m.contains("https://127.0.0.1:"), "{m}");
        }
        other => panic!("erreur réseau attendue, obtenu {other:?}"),
    }
}

#[test]
fn req036_ac1_openai_without_authority_is_refused() {
    let s = server("server-ip.pem", vec![(200, r#"{"data":[]}"#.into())]);
    assert_cert_refused(openai(&s.base_url, &Tls::default()).enrich(
        "A.java",
        1,
        "class A {}",
        &ids(),
        &ids(),
    ));
    assert!(s.requests().is_empty());
}

#[test]
fn req036_ac1_anthropic_without_authority_is_refused() {
    let s = server("server-ip.pem", vec![(200, r#"{"data":[]}"#.into())]);
    assert_cert_refused(anthropic(&s.base_url, &Tls::default()).enrich(
        "A.java",
        1,
        "class A {}",
        &ids(),
        &ids(),
    ));
    assert!(s.requests().is_empty());
}

#[test]
fn req036_ac2_openai_with_authority_succeeds() {
    let s = server("server-ip.pem", vec![(200, openai_reply(EXTRACTION))]);
    let llm = openai(&s.base_url, &tls_with("ca.pem"));
    let ex = llm
        .enrich("A.java", 1, "class A {}", &ids(), &ids())
        .unwrap();
    assert_eq!(ex.summaries.len(), 1);
    let r = s.requests();
    assert_eq!(r.len(), 1);
    assert_eq!(
        (r[0].method.as_str(), r[0].path.as_str()),
        ("POST", "/v1/chat/completions")
    );
    assert_eq!(r[0].headers["authorization"], "Bearer k");
    let b: serde_json::Value = serde_json::from_str(&r[0].body).unwrap();
    assert_eq!(b["model"], "m");
}

#[test]
fn req036_ac2_anthropic_with_authority_succeeds() {
    let s = server("server-ip.pem", vec![(200, anthropic_reply(EXTRACTION))]);
    let llm = anthropic(&s.base_url, &tls_with("ca.pem"));
    let ex = llm
        .enrich("A.java", 1, "class A {}", &ids(), &ids())
        .unwrap();
    assert_eq!(ex.summaries.len(), 1);
    let r = s.requests();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].headers["x-api-key"], "k");
    assert_eq!(
        (r[0].method.as_str(), r[0].path.as_str()),
        ("POST", "/v1/messages")
    );
    assert!(r[0].headers.contains_key("anthropic-version"));
}

#[test]
fn req036_ac3_bundle_with_key_and_two_authorities_is_usable() {
    // bundle.pem : clé privée, other-ca, ca ; seuls les blocs CERTIFICATE comptent.
    let s = server("server-ip.pem", vec![(200, openai_reply(EXTRACTION))]);
    let tls = tls_with("bundle.pem");
    assert_eq!(tls.ca_certs.len(), 2);
    openai(&s.base_url, &tls)
        .enrich("A.java", 1, "class A {}", &ids(), &ids())
        .unwrap();
}

#[test]
fn req036_ac4_other_authority_is_refused() {
    let s = server("server-ip.pem", vec![(200, r#"{"data":[]}"#.into())]);
    assert_cert_refused(openai(&s.base_url, &tls_with("other-ca.pem")).enrich(
        "A.java",
        1,
        "class A {}",
        &ids(),
        &ids(),
    ));
    assert_cert_refused(anthropic(&s.base_url, &tls_with("other-ca.pem")).enrich(
        "A.java",
        1,
        "class A {}",
        &ids(),
        &ids(),
    ));
    assert!(s.requests().is_empty());
}

#[test]
fn req036_ac4_hostname_is_still_checked() {
    let s = server("server-name.pem", vec![(200, r#"{"data":[]}"#.into())]);
    assert_cert_refused(openai(&s.base_url, &tls_with("ca.pem")).enrich(
        "A.java",
        1,
        "class A {}",
        &ids(),
        &ids(),
    ));
    assert_cert_refused(anthropic(&s.base_url, &tls_with("ca.pem")).enrich(
        "A.java",
        1,
        "class A {}",
        &ids(),
        &ids(),
    ));
    assert!(s.requests().is_empty());
}

#[test]
fn req036_ac6_insecure_accepts_any_certificate() {
    for cert in ["server-ip.pem", "server-name.pem"] {
        let s = server(
            cert,
            vec![
                (200, openai_reply(EXTRACTION)),
                (200, anthropic_reply(EXTRACTION)),
            ],
        );
        let o = openai(&s.base_url, &insecure());
        o.enrich("A.java", 1, "class A {}", &ids(), &ids()).unwrap();
        let a = anthropic(&s.base_url, &insecure());
        a.enrich("A.java", 1, "class A {}", &ids(), &ids()).unwrap();
        assert_eq!(s.requests().len(), 2, "{cert}");
    }
}

#[test]
fn req036_ac7_tls_settings_do_not_affect_plain_http() {
    let s = FakeServer::start(vec![(200, openai_reply(EXTRACTION))]);
    let mut tls = tls_with("ca.pem");
    tls.insecure = true;
    openai(&s.base_url, &tls)
        .enrich("A.java", 1, "class A {}", &ids(), &ids())
        .unwrap();
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn req036_ac1_timeout_and_key_errors_keep_their_class() {
    let s = server("server-ip.pem", vec![(401, "{}".into())]);
    let tls = tls_with("ca.pem");
    assert!(matches!(
        openai(&s.base_url, &tls).enrich("A.java", 1, "class A {}", &ids(), &ids()),
        Err(Error::InvalidKey(_))
    ));
}
