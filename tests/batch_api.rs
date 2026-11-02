//! Protocole du lot du fournisseur `anthropic` : création, état, résultats (REQ-212, REQ-214),
//! par l'API publique de la bibliothèque et contre le faux serveur à routes.

#[allow(dead_code)]
#[path = "support/batch_server.rs"]
mod batch_server;
#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;

use batch_server::BatchServer;
use fake_server::Reply;
use nephila::config::Effort;
use nephila::error::Error;
use nephila::llm::anthropic::AnthropicLlm;
use nephila::llm::batch::{Answer, BatchApi, Collector, CreateError, Pending};
use nephila::llm::{Generation, Llm, Retry, Tls};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

const CREATE: &str = "/v1/messages/batches";

fn llm(server: &BatchServer) -> AnthropicLlm {
    AnthropicLlm::new(
        Some(format!("{}/", server.base_url)),
        "m".into(),
        "sk-test".into(),
        10,
        &Tls::default(),
        1,
        Generation {
            effort: Effort::ModelDefault,
            effort_explicit: false,
            thinking: false,
            max_output_tokens: 100,
        },
    )
    .with_retry(Retry::from_millis(1))
    .with_collector(Arc::new(Collector::new()))
}

fn pending(n: usize) -> Pending {
    let body = json!({"model": "m", "n": n});
    Pending {
        id: nephila::llm::batch::body_id(&body),
        body,
    }
}

fn ok(body: Value) -> Reply {
    Reply::status(200, &body.to_string())
}

fn create_error(server: &BatchServer, script: Vec<Reply>) -> CreateError {
    server.route("POST", CREATE, script);
    llm(server)
        .create(&[pending(1)], &AtomicBool::new(false))
        .unwrap_err()
}

fn counts(processing: u64, succeeded: u64, errored: u64, canceled: u64, expired: u64) -> Value {
    json!({"processing": processing, "succeeded": succeeded, "errored": errored,
           "canceled": canceled, "expired": expired})
}

#[test]
fn req212_ac1_create_posts_requests_with_headers() {
    let server = BatchServer::start();
    server.route("POST", CREATE, vec![ok(json!({"id": "msgbatch_01abc"}))]);
    let reqs = [pending(1), pending(2)];
    let (id, bytes) = llm(&server).create(&reqs, &AtomicBool::new(false)).unwrap();
    assert_eq!(id, "msgbatch_01abc");
    let hits = server.hits("POST", CREATE);
    assert_eq!(hits.len(), 1);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(hits[0].headers["x-api-key"], "sk-test");
    assert!(hits[0].headers.contains_key("anthropic-version"));
    assert_eq!(bytes, hits[0].body.len());
    let body: Value = serde_json::from_str(&hits[0].body).unwrap();
    let sent = body["requests"].as_array().unwrap();
    assert_eq!(sent.len(), 2);
    for (s, r) in sent.iter().zip(&reqs) {
        assert_eq!(s["custom_id"], r.id.as_str());
        assert_eq!(s["params"], r.body);
    }
}

#[test]
fn req212_ac1_create_without_id_is_an_error() {
    let server = BatchServer::start();
    let e = create_error(&server, vec![ok(json!({"type": "message_batch"}))]);
    assert!(matches!(e, CreateError::Other(Error::Format(_))), "{e:?}");
    let e = create_error(&server, vec![ok(json!({"id": "../etc"}))]);
    assert!(matches!(e, CreateError::Other(Error::Format(_))), "{e:?}");
}

#[test]
fn req212_ac2_status_in_progress_then_ended_with_counters() {
    let server = BatchServer::start();
    let path = "/v1/messages/batches/msgbatch_1";
    server.route(
        "GET",
        path,
        vec![
            ok(json!({"processing_status": "in_progress", "request_counts": counts(5, 1, 1, 0, 0)})),
            ok(json!({"processing_status": "ended", "request_counts": counts(0, 3, 1, 1, 2)})),
        ],
    );
    let llm = llm(&server);
    let stop = AtomicBool::new(false);
    let s = llm.status("msgbatch_1", &stop).unwrap();
    assert!(!s.finished);
    assert_eq!((s.done, s.total), (2, 7));
    assert_eq!(s.status, "in_progress");
    let s = llm.status("msgbatch_1", &stop).unwrap();
    assert!(s.finished);
    assert_eq!((s.done, s.total), (7, 7));
    assert_eq!(s.id, "msgbatch_1");
    assert_eq!(s.failure, None);
    assert_eq!(
        s.default_answer,
        Answer::Failed {
            cause: "sans réponse".into(),
            refused: None
        }
    );
    let hits = server.hits("GET", path);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].headers["x-api-key"], "sk-test");
    assert!(hits[0].headers.contains_key("anthropic-version"));
}

#[test]
fn req212_ac2_status_without_counters_is_an_error() {
    let server = BatchServer::start();
    server.route(
        "GET",
        "/v1/messages/batches/b1",
        vec![ok(json!({"processing_status": "ended"}))],
    );
    let e = llm(&server)
        .status("b1", &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(e, Error::Format(_)), "{e:?}");
    // Identifiant qui sortirait du chemin : refusé sans requête.
    let e = llm(&server)
        .status("a/../b", &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(e, Error::Format(_)), "{e:?}");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn req212_ac2_results_url_of_another_host_is_never_contacted() {
    let server = BatchServer::start();
    let other = BatchServer::start();
    server.route(
        "GET",
        "/v1/messages/batches/b1",
        vec![ok(json!({
            "processing_status": "ended", "request_counts": counts(0, 1, 0, 0, 0),
            "results_url": format!("{}/v1/messages/batches/b1/results", other.base_url)
        }))],
    );
    let line = json!({"custom_id": "c1", "result": {"type": "succeeded",
        "message": {"content": [{"type": "text", "text": "ok"}]}}});
    server.route(
        "GET",
        "/v1/messages/batches/b1/results",
        vec![Reply::status(200, &format!("{line}\n"))],
    );
    let llm = llm(&server);
    let stop = AtomicBool::new(false);
    let status = llm.status("b1", &stop).unwrap();
    let answers = llm.results(&status, &stop).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(
        server.hits("GET", "/v1/messages/batches/b1/results").len(),
        1
    );
    assert!(other.requests().is_empty());
}

#[test]
fn req214_results_succeeded_keeps_the_message_body() {
    let server = BatchServer::start();
    let message = json!({"id": "msg_1", "content": [{"type": "text", "text": "bonjour"}],
        "stop_reason": "end_turn", "usage": {"input_tokens": 3, "output_tokens": 4}});
    let line = json!({"custom_id": "c1", "result": {"type": "succeeded", "message": message}});
    server.route(
        "GET",
        "/v1/messages/batches/b1/results",
        vec![Reply::status(200, &line.to_string())],
    );
    let llm = llm(&server);
    let status = status_of(&llm, &server, "b1");
    let answers = llm.results(&status, &AtomicBool::new(false)).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0, "c1");
    let Answer::Raw(raw) = &answers[0].1 else {
        panic!("{:?}", answers[0].1)
    };
    assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), message);
}

fn status_of(llm: &AnthropicLlm, server: &BatchServer, id: &str) -> nephila::llm::batch::LotStatus {
    server.route(
        "GET",
        &format!("/v1/messages/batches/{id}"),
        vec![ok(
            json!({"processing_status": "ended", "request_counts": counts(0, 1, 0, 0, 0)}),
        )],
    );
    llm.status(id, &AtomicBool::new(false)).unwrap()
}

fn failed(cause: &str, refused: Option<&str>) -> Answer {
    Answer::Failed {
        cause: cause.into(),
        refused: refused.map(String::from),
    }
}

#[test]
fn req214_ac10_results_failures_have_the_causes_of_the_table() {
    let server = BatchServer::start();
    let long = format!("{}\n{}", "a".repeat(300), "b");
    let lines = [
        json!({"custom_id": "e1", "result": {"type": "errored", "error": {"type": "error",
            "error": {"type": "api_error", "message": "boom"}}}}),
        json!({"custom_id": "e2", "result": {"type": "errored", "error": {"type": "error",
            "error": {"type": "invalid_request_error", "message": "output_config.format bad"}}}}),
        json!({"custom_id": "e3", "result": {"type": "errored", "error": {"type": "error",
            "error": {"type": "overloaded_error"}}}}),
        json!({"custom_id": "e4", "result": {"type": "errored", "error": {"type": "error",
            "error": {"type": "api_error", "message": long}}}}),
        json!({"custom_id": "x1", "result": {"type": "expired"}}),
        json!({"custom_id": "x2", "result": {"type": "canceled"}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n\n")).collect();
    server.route(
        "GET",
        "/v1/messages/batches/b1/results",
        vec![Reply::status(200, &text)],
    );
    let llm = llm(&server);
    let status = status_of(&llm, &server, "b1");
    let answers = llm.results(&status, &AtomicBool::new(false)).unwrap();
    let get = |id: &str| answers.iter().find(|(i, _)| i == id).unwrap().1.clone();
    assert_eq!(answers.len(), 6);
    assert_eq!(get("e1"), failed("api_error : boom", None));
    assert_eq!(
        get("e2"),
        failed(
            "invalid_request_error : output_config.format bad",
            Some("output_config.format bad")
        )
    );
    assert_eq!(get("e3"), failed("overloaded_error", None));
    let Answer::Failed { cause, .. } = get("e4") else {
        panic!()
    };
    let message = cause.strip_prefix("api_error : ").unwrap();
    assert_eq!(message.chars().count(), 200);
    assert!(message.ends_with('…') && !message.contains('\n'));
    assert_eq!(get("x1"), failed("expirée", None));
    assert_eq!(get("x2"), failed("annulée", None));
}

#[test]
fn req214_results_invalid_line_is_an_error() {
    let server = BatchServer::start();
    server.route(
        "GET",
        "/v1/messages/batches/b1/results",
        vec![Reply::status(200, "pas du json\n")],
    );
    let llm = llm(&server);
    let status = status_of(&llm, &server, "b1");
    let e = llm.results(&status, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(e, Error::Format(_)), "{e:?}");
}

#[test]
fn req214_ac7_results_over_11_mb_are_read_entirely() {
    let server = BatchServer::start();
    let big = "x".repeat(11 * 1024 * 1024);
    let l1 = json!({"custom_id": "c1", "result": {"type": "succeeded",
        "message": {"content": [{"type": "text", "text": big}]}}});
    let l2 = json!({"custom_id": "c2", "result": {"type": "expired"}});
    let text = format!("{l1}\n{l2}\n");
    assert!(text.len() > 11_000_000);
    server.route(
        "GET",
        "/v1/messages/batches/b1/results",
        vec![Reply::status(200, &text)],
    );
    let llm = llm(&server);
    let status = status_of(&llm, &server, "b1");
    let answers = llm.results(&status, &AtomicBool::new(false)).unwrap();
    assert_eq!(answers.len(), 2);
    let Answer::Raw(raw) = &answers[0].1 else {
        panic!()
    };
    assert!(raw.len() > 11 * 1024 * 1024);
    assert_eq!(answers[1].1, failed("expirée", None));
}

#[test]
fn req214_create_without_batch_mode_for_404_405_501() {
    for code in [404u16, 405, 501] {
        let server = BatchServer::start();
        let e = create_error(&server, vec![Reply::status(code, "{}")]);
        assert_eq!(
            e,
            CreateError::NoBatchMode {
                code,
                url: format!("{}/{}", server.base_url, CREATE.trim_start_matches('/'))
            }
        );
        assert_eq!(server.requests().len(), 1, "{code}");
    }
}

#[test]
fn req214_create_401_is_invalid_key() {
    let server = BatchServer::start();
    let e = create_error(&server, vec![Reply::status(401, "{}")]);
    assert!(
        matches!(e, CreateError::Other(Error::InvalidKey(_))),
        "{e:?}"
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn req214_create_400_413_422_are_rejected_with_the_message() {
    for code in [400u16, 413, 422] {
        let server = BatchServer::start();
        let body = json!({"error": {"type": "invalid_request_error", "message": "trop gros"}});
        let e = create_error(&server, vec![ok_status(code, body)]);
        let CreateError::Other(Error::Rejected {
            code: c, detail, ..
        }) = e
        else {
            panic!("{code}: {e:?}")
        };
        assert_eq!(c, code);
        assert_eq!(detail.as_deref(), Some("trop gros"));
        assert_eq!(server.requests().len(), 1);
    }
}

fn ok_status(code: u16, body: Value) -> Reply {
    Reply::status(code, &body.to_string())
}

#[test]
fn req214_create_redirect_is_redirected() {
    let server = BatchServer::start();
    let e = create_error(
        &server,
        vec![Reply::with_header(
            302,
            "",
            "location",
            "http://ailleurs.example/x",
        )],
    );
    let CreateError::Other(e_inner @ Error::Redirected { .. }) = e else {
        panic!("{e:?}")
    };
    assert_eq!(
        e_inner.to_string(),
        "anthropic redirige (HTTP 302) vers http://ailleurs.example/x ; corrigez base_url"
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn req214_ac9_connection_cut_after_reading_is_one_request() {
    let server = BatchServer::start();
    let e = create_error(&server, vec![Reply::Close]);
    assert_eq!(
        e,
        CreateError::Failed {
            cause: "réseau".into()
        }
    );
    assert_eq!(server.hits("POST", CREATE).len(), 1);
}

#[test]
fn req214_ac9_body_cut_after_a_200_is_one_request() {
    let server = BatchServer::start();
    let e = create_error(
        &server,
        vec![Reply::Partial {
            status: 200,
            declared_len: 100,
            sent: 5,
        }],
    );
    assert_eq!(
        e,
        CreateError::Failed {
            cause: "réseau".into()
        }
    );
    assert_eq!(server.hits("POST", CREATE).len(), 1);
}

#[test]
fn req214_ac9_http_500_is_one_request() {
    let server = BatchServer::start();
    let e = create_error(&server, vec![Reply::status(500, "{}")]);
    assert_eq!(
        e,
        CreateError::Failed {
            cause: "HTTP 500".into()
        }
    );
    assert_eq!(server.hits("POST", CREATE).len(), 1);
}

#[test]
fn req214_ac9_429_then_success_is_two_requests() {
    let server = BatchServer::start();
    server.route(
        "POST",
        CREATE,
        vec![Reply::status(429, "{}"), ok(json!({"id": "msgbatch_2"}))],
    );
    let (id, _) = llm(&server)
        .create(&[pending(1)], &AtomicBool::new(false))
        .unwrap();
    assert_eq!(id, "msgbatch_2");
    assert_eq!(server.hits("POST", CREATE).len(), 2);
}

#[test]
fn req214_ac9_503_exhausted_is_failed_after_5_requests() {
    let server = BatchServer::start();
    let e = create_error(&server, vec![Reply::status(503, "{}")]);
    assert_eq!(
        e,
        CreateError::Failed {
            cause: "HTTP 503".into()
        }
    );
    assert_eq!(server.hits("POST", CREATE).len(), 5);
}

#[test]
fn req214_batch_link_needs_a_collector_and_address_is_effective() {
    let server = BatchServer::start();
    let with = llm(&server);
    let link = with.batch().unwrap();
    assert_eq!(link.api.provider(), "anthropic");
    assert_eq!(link.api.address(), server.base_url);
    let without = AnthropicLlm::new(
        None,
        "m".into(),
        "k".into(),
        10,
        &Tls::default(),
        1,
        Generation {
            effort: Effort::ModelDefault,
            effort_explicit: false,
            thinking: false,
            max_output_tokens: 100,
        },
    );
    assert!(without.batch().is_none());
}
