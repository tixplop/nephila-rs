//! Adaptateur compatible OpenAI Chat Completions (REQ-002) : OpenAI, Ollama, llama.cpp, vLLM.

use super::batch::{Answer, BatchApi, BatchLink, Collector, CreateError, LotStatus, Pending};
use super::schema::Reply;
use super::{http, openai_usage, CallStats, Complete, Generation, Retry, Structured, Tls};
use crate::config::Effort;
use crate::error::{Error, Result, BLOCKED, TRUNCATED};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Corps de la requête `/v1/chat/completions` (REQ-058, REQ-157, DEC-126) : `reasoning_effort`
/// si l'effort n'est pas `"default"` et que la réflexion est demandée ou l'effort écrit ;
/// `temperature` part si elle est écrite et que `thinking` est faux (REQ-275) ;
/// `response_format` porte le schéma s'il est donné (REQ-202).
pub fn request_body(
    model: &str,
    gen: &Generation,
    system: &str,
    user: &str,
    format: Option<(&str, &serde_json::Value)>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_completion_tokens": gen.max_output_tokens,
    });
    if let (Some(t), false) = (gen.temperature, gen.thinking) {
        body["temperature"] = t.into();
    }
    if gen.effort != Effort::ModelDefault && (gen.thinking || gen.effort_explicit) {
        body["reasoning_effort"] = gen.effort.as_str().into();
    }
    if let Some((name, schema)) = format {
        body["response_format"] = serde_json::json!({
            "type": "json_schema",
            "json_schema": {"name": name, "strict": true, "schema": schema},
        });
    }
    body
}

pub struct OpenAiLlm {
    base_url: Option<String>,
    model: String,
    key: Option<String>,
    agent: ureq::Agent,
    retry: http::Retry,
    pause: http::Pause,
    gen: Generation,
    structured: Structured,
    structured_key: &'static str,
    fallback: AtomicBool,
    summary_language: String,
    collector: Option<Arc<Collector>>,
}

impl OpenAiLlm {
    /// `key` est facultative (modèle local).
    pub fn new(
        base_url: Option<String>,
        model: String,
        key: Option<String>,
        timeout_secs: u64,
        tls: &Tls,
        concurrency: usize,
        gen: Generation,
    ) -> OpenAiLlm {
        OpenAiLlm {
            base_url,
            model,
            key,
            agent: http::agent(timeout_secs, concurrency, tls),
            retry: http::Retry::default(),
            pause: http::Pause::new(concurrency),
            gen,
            structured: Structured::Off,
            structured_key: "structured_output",
            fallback: AtomicBool::new(false),
            summary_language: crate::config::DEFAULT_SUMMARY_LANGUAGE.to_string(),
            collector: None,
        }
    }

    /// Réglage effectif de `structured_output` (REQ-202).
    pub fn with_structured(mut self, structured: Structured) -> OpenAiLlm {
        self.structured = structured;
        self
    }

    /// Clé qui a donné le réglage, nommée par la trace de repli (REQ-203).
    pub fn with_structured_key(mut self, key: &'static str) -> OpenAiLlm {
        self.structured_key = key;
        self
    }

    /// Langue des résumés demandée dans les prompts système (REQ-172).
    pub fn with_summary_language(mut self, language: String) -> OpenAiLlm {
        self.summary_language = language;
        self
    }

    /// Collecteur du mode lot : les réponses viennent de lui au lieu du réseau (DEC-168).
    pub fn with_collector(mut self, collector: Arc<Collector>) -> OpenAiLlm {
        self.collector = Some(collector);
        self
    }

    /// Base d'attente des nouveaux essais HTTP (REQ-052).
    pub fn with_retry(mut self, retry: Retry) -> OpenAiLlm {
        self.retry = retry;
        self
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let req = self
            .agent
            .request(method, &http::url(&self.base_url, DEFAULT_BASE_URL, path));
        match &self.key {
            Some(k) => req.set("Authorization", &format!("Bearer {k}")),
            None => req,
        }
    }
}

/// Identifiant de fichier ou de lot sûr à placer dans un chemin d'URL.
fn check_id(id: &str) -> Result<&str> {
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        Ok(id)
    } else {
        Err(Error::Format("identifiant de openai illisible".into()))
    }
}

fn failed(cause: &str) -> Answer {
    Answer::Failed {
        cause: cause.into(),
        refused: None,
    }
}

/// Compteur `name` de `request_counts`.
fn count(v: &serde_json::Value, name: &str) -> Result<usize> {
    v["request_counts"][name]
        .as_u64()
        .map(|n| n as usize)
        .ok_or_else(|| Error::Format(format!("état de lot de openai sans {name}")))
}

/// Message d'erreur d'un objet `{code, message}`, normalisé ; vide s'il manque.
fn error_message(e: &serde_json::Value) -> String {
    http::provider_text(e["message"].as_str().unwrap_or(""))
}

/// Fichier d'entrée du lot : JSONL, un `custom_id` par ligne.
fn jsonl(requests: &[Pending]) -> String {
    let mut out = String::new();
    for p in requests {
        let line = serde_json::json!({
            "custom_id": p.id, "method": "POST", "url": "/v1/chat/completions", "body": p.body,
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

/// Corps `multipart/form-data` du dépôt du fichier : `purpose` puis `file`. Le séparateur vient de
/// l'empreinte du contenu, donc ne s'y trouve pas.
fn multipart(jsonl: &str) -> (String, String) {
    let boundary = format!("nephila{}", blake3::hash(jsonl.as_bytes()).to_hex());
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nbatch\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"batch.jsonl\"\r\n\
         Content-Type: application/jsonl\r\n\r\n{jsonl}\r\n--{boundary}--\r\n"
    );
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Réponse d'une ligne de résultat (tableau d'A-17).
fn line_answer(v: &serde_json::Value) -> Answer {
    let response = &v["response"];
    let Some(code) = response["status_code"].as_u64() else {
        let message = error_message(&v["error"]);
        return failed(if message.is_empty() {
            "sans réponse"
        } else {
            &message
        });
    };
    if code == 200 {
        return Answer::Raw(response["body"].to_string());
    }
    let message = error_message(&response["body"]["error"]);
    if message.is_empty() {
        return failed(&format!("HTTP {code}"));
    }
    Answer::Failed {
        cause: format!("HTTP {code} : {message}"),
        refused: (code == 400).then(|| message.clone()),
    }
}

impl BatchApi for OpenAiLlm {
    fn provider(&self) -> &str {
        "openai"
    }

    fn address(&self) -> String {
        self.base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string()
    }

    fn create(
        &self,
        requests: &[Pending],
        stop: &AtomicBool,
    ) -> std::result::Result<(String, usize), CreateError> {
        let input = jsonl(requests);
        let (content_type, upload) = multipart(&input);
        let file = http::send_create(
            "openai",
            self.request("POST", "/v1/files"),
            &upload,
            &content_type,
            &self.retry,
            stop,
        )?;
        let file_id = http::parse_body("openai", &file)
            .and_then(|v| match v["id"].as_str() {
                Some(id) => check_id(id).map(String::from),
                None => Err(Error::Format(
                    "réponse de openai sans identifiant de fichier".into(),
                )),
            })
            .map_err(CreateError::Other)?;
        let body = serde_json::json!({
            "input_file_id": file_id,
            "endpoint": "/v1/chat/completions",
            "completion_window": "24h",
        });
        let text = http::send_create(
            "openai",
            self.request("POST", "/v1/batches"),
            &body.to_string(),
            "application/json",
            &self.retry,
            stop,
        )?;
        let id = http::parse_body("openai", &text)
            .and_then(|v| match v["id"].as_str() {
                Some(id) => check_id(id).map(String::from),
                None => Err(Error::Format(
                    "réponse de openai sans identifiant de lot".into(),
                )),
            })
            .map_err(CreateError::Other)?;
        Ok((id, input.len()))
    }

    fn status(&self, id: &str, stop: &AtomicBool) -> Result<LotStatus> {
        let id = check_id(id)?;
        let text = http::send_all(
            "openai",
            self.request("GET", &format!("/v1/batches/{id}")),
            &self.retry,
            stop,
        )?;
        let v = http::parse_body("openai", &text)?;
        let status = v["status"]
            .as_str()
            .ok_or_else(|| Error::Format("état de lot de openai sans status".into()))?;
        let finished = matches!(status, "completed" | "expired" | "cancelled" | "failed");
        let refused = {
            let message = error_message(&v["errors"]["data"][0]);
            failed(&if message.is_empty() {
                "lot refusé".to_string()
            } else {
                format!("lot refusé : {message}")
            })
        };
        let failure = (status == "failed").then(|| refused.clone());
        let mut files = Vec::new();
        for key in ["output_file_id", "error_file_id"] {
            if let Some(file) = v[key].as_str() {
                files.push(format!("/v1/files/{}/content", check_id(file)?));
            }
        }
        Ok(LotStatus {
            id: id.into(),
            status: status.into(),
            finished,
            done: count(&v, "completed")? + count(&v, "failed")?,
            total: count(&v, "total")?,
            failure,
            default_answer: match status {
                "failed" => refused,
                "expired" => failed("expirée"),
                "cancelled" => failed("annulée"),
                _ => failed("sans réponse"),
            },
            files,
        })
    }

    fn results(&self, status: &LotStatus, stop: &AtomicBool) -> Result<Vec<(String, Answer)>> {
        let mut out = Vec::new();
        for file in &status.files {
            let text = http::send_all("openai", self.request("GET", file), &self.retry, stop)?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let v = http::parse_body("openai", line)?;
                let custom_id = v["custom_id"].as_str().ok_or_else(|| {
                    Error::Format("ligne de résultat de openai sans custom_id".into())
                })?;
                out.push((custom_id.to_string(), line_answer(&v)));
            }
        }
        Ok(out)
    }
}

impl Complete for OpenAiLlm {
    fn batch(&self) -> Option<BatchLink<'_>> {
        self.collector.as_deref().map(|collector| BatchLink {
            api: self,
            collector,
        })
    }

    fn summary_language(&self) -> &str {
        &self.summary_language
    }

    fn complete(
        &self,
        label: Option<&str>,
        stop: Option<&AtomicBool>,
        stats: &CallStats,
        system: String,
        user: String,
        reply: Reply,
        _cache_upto: Option<usize>,
    ) -> Result<String> {
        let send = |with_schema: bool| {
            let schema = with_schema.then(|| reply.schema());
            let format = schema.as_ref().map(|s| (reply.name(), s));
            let mut body = request_body(&self.model, &self.gen, &system, &user, format);
            // La passe des sous-systèmes part toujours hors mode lot (REQ-261).
            if let (Some(collector), false) = (&self.collector, matches!(reply, Reply::Subsystem)) {
                return collector.exchange(&body, stats);
            }
            // Réponse reçue en flux (REQ-249) ; le mode lot ne passe pas ici.
            body["stream"] = serde_json::json!(true);
            body["stream_options"] = serde_json::json!({"include_usage": true});
            http::send(
                "openai",
                self.request("POST", "/v1/chat/completions"),
                Some(&body),
                &self.retry,
                label,
                stop,
                stats,
                &self.pause,
            )
        };
        let with_schema = match self.structured {
            Structured::Off => false,
            Structured::On => true,
            Structured::Auto => !self.fallback.load(Ordering::SeqCst),
        };
        let mut sent = send(with_schema);
        if let Err(e) = &sent {
            if with_schema
                && http::schema_refused(
                    "openai",
                    e,
                    self.structured,
                    &self.fallback,
                    self.structured_key,
                )
            {
                sent = send(false);
            }
        }
        let raw = sent?;
        let v = http::parse_body("openai", &raw)?;
        stats.record_response(openai_usage(&v));
        if v["choices"][0]["finish_reason"] == "content_filter"
            || v["choices"][0]["message"]["refusal"]
                .as_str()
                .is_some_and(|r| !r.is_empty())
        {
            return Err(Error::Refused(BLOCKED.into()));
        }
        let text = v["choices"][0]["message"]["content"]
            .as_str()
            .map(String::from);
        if v["choices"][0]["finish_reason"] == "length" {
            return Err(Error::Truncated {
                message: TRUNCATED.into(),
                partial: text.unwrap_or_default(),
            });
        }
        text.ok_or_else(|| Error::Format("réponse de openai sans message".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn gen(thinking: bool, effort: Effort, explicit: bool, max: u32) -> Generation {
        Generation {
            temperature: None,
            effort,
            effort_explicit: explicit,
            thinking,
            max_output_tokens: max,
        }
    }

    fn effort_of(thinking: bool, effort: Effort, explicit: bool) -> Option<String> {
        let b = request_body("m", &gen(thinking, effort, explicit, 1234), "s", "u", None);
        let n = b.as_object().unwrap().len();
        for k in ["thinking", "output_config", "max_tokens"] {
            assert!(b.get(k).is_none(), "{k}");
        }
        let r = b
            .get("reasoning_effort")
            .map(|v| v.as_str().unwrap().to_string());
        assert_eq!(n, if r.is_some() { 4 } else { 3 });
        r
    }

    #[test]
    fn body_without_thinking_has_no_reasoning_field() {
        let b = request_body("m", &gen(false, Effort::Low, false, 1234), "s", "u", None);
        assert_eq!(
            b,
            serde_json::json!({
                "model": "m",
                "messages": [
                    {"role": "system", "content": "s"},
                    {"role": "user", "content": "u"},
                ],
                "max_completion_tokens": 1234,
            })
        );
    }

    #[test]
    fn req275_temperature_envoyee_sauf_avec_thinking() {
        let none = request_body("m", &gen(false, Effort::Low, false, 1234), "s", "u", None);
        assert!(none.get("temperature").is_none());
        let mut g = gen(false, Effort::Low, false, 1234);
        g.temperature = Some(0.2);
        let mut want = none.clone();
        want["temperature"] = 0.2.into();
        assert_eq!(request_body("m", &g, "s", "u", None), want);
        g.temperature = Some(0.0);
        assert_eq!(request_body("m", &g, "s", "u", None)["temperature"], 0.0);
        let mut g = gen(true, Effort::Low, false, 1234);
        g.temperature = Some(0.2);
        assert!(request_body("m", &g, "s", "u", None)
            .get("temperature")
            .is_none());
    }

    #[test]
    fn req157_ac3_reasoning_effort_rules() {
        use Effort::*;
        assert_eq!(effort_of(false, Low, true).as_deref(), Some("low"));
        assert_eq!(effort_of(false, Medium, true).as_deref(), Some("medium"));
        assert_eq!(effort_of(false, ModelDefault, true), None);
        assert_eq!(effort_of(true, Low, false).as_deref(), Some("low"));
        assert_eq!(effort_of(true, High, true).as_deref(), Some("high"));
        assert_eq!(effort_of(true, ModelDefault, true), None);
    }

    #[test]
    fn body_with_thinking_adds_reasoning_effort() {
        let b = request_body("m", &gen(true, Effort::Medium, true, 16384), "s", "u", None);
        assert_eq!(b["reasoning_effort"], "medium");
        assert_eq!(b["max_completion_tokens"], 16384);
        assert!(b.get("max_tokens").is_none());
    }

    use crate::llm::usage::{fake_server, openai_usage, CallStats, Usage};
    use serde_json::json;

    fn ou(u: serde_json::Value) -> Option<Usage> {
        openai_usage(&json!({ "usage": u }))
    }

    #[test]
    fn req165_ac2_openai_usage_is_normalized() {
        let full = ou(json!({"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":64},
            "completion_tokens_details":{"reasoning_tokens":8}}));
        assert_eq!(
            full,
            Some(Usage {
                input: 100,
                cache_read: 64,
                cache_write: 0,
                output: 20,
                reasoning: Some(8)
            })
        );
        let plain = Some(Usage {
            input: 100,
            cache_read: 0,
            cache_write: 0,
            output: 20,
            reasoning: None,
        });
        assert_eq!(
            ou(json!({"prompt_tokens":100,"completion_tokens":20})),
            plain
        );
        assert_eq!(
            ou(json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":null})),
            plain
        );
        assert_eq!(
            ou(json!({"prompt_tokens":100,"completion_tokens":20,
                "completion_tokens_details":{"reasoning_tokens":null}})),
            plain
        );
    }

    #[test]
    fn req165_ac2_openai_usage_missing_or_invalid_gives_none() {
        assert_eq!(openai_usage(&json!({})), None);
        assert_eq!(openai_usage(&json!({"usage": null})), None);
        assert_eq!(ou(json!({"prompt_tokens":100})), None);
        assert_eq!(
            ou(json!({"prompt_tokens":"100","completion_tokens":20})),
            None
        );
        assert_eq!(ou(json!({"prompt_tokens":-1,"completion_tokens":20})), None);
    }

    #[test]
    fn req165_ac3_truncated_and_messageless_answers_are_still_measured() {
        let usage = r#""usage":{"prompt_tokens":10,"completion_tokens":3}"#;
        let want = Usage {
            input: 10,
            cache_read: 0,
            cache_write: 0,
            output: 3,
            reasoning: None,
        };
        let cases = [
            (
                format!(
                    r#"{{"choices":[{{"finish_reason":"length","message":{{"content":"x"}}}}],{usage}}}"#
                ),
                true,
            ),
            (format!(r#"{{"choices":[],{usage}}}"#), false),
        ];
        for (body, truncated) in cases {
            let l = OpenAiLlm::new(
                Some(fake_server(vec![(200, body)])),
                "m".into(),
                None,
                10,
                &Tls::default(),
                1,
                gen(false, Effort::Low, false, 100),
            );
            let stats = CallStats::default();
            let e = l
                .complete(
                    None,
                    None,
                    &stats,
                    "s".into(),
                    "u".into(),
                    Reply::File,
                    None,
                )
                .unwrap_err();
            if truncated {
                assert!(
                    matches!(&e, Error::Truncated { message: m, .. } if m == TRUNCATED),
                    "{e:?}"
                );
            } else {
                assert!(
                    matches!(&e, Error::Format(m) if m == "réponse de openai sans message"),
                    "{e:?}"
                );
            }
            assert!(stats.responded());
            assert_eq!(stats.usage(), Some(want));
            assert_eq!(stats.sends(), 1);
        }
    }

    #[test]
    fn req274_ac2_content_filter_and_refusal_are_blocked() {
        let usage = r#""usage":{"prompt_tokens":10,"completion_tokens":3}"#;
        let blocked = [
            format!(
                r#"{{"choices":[{{"finish_reason":"content_filter","message":{{"content":"x"}}}}],{usage}}}"#
            ),
            format!(
                r#"{{"choices":[{{"finish_reason":"content_filter","message":{{}}}}],{usage}}}"#
            ),
            format!(
                r#"{{"choices":[{{"finish_reason":"stop","message":{{"content":null,"refusal":"I can't"}}}}],{usage}}}"#
            ),
        ];
        let not_blocked = [
            (
                r#"{"choices":[{"finish_reason":"stop","message":{"content":"x","refusal":null}}]}"#,
                true,
            ),
            (
                r#"{"choices":[{"finish_reason":"stop","message":{"content":"x","refusal":""}}]}"#,
                true,
            ),
            (
                r#"{"choices":[{"finish_reason":"length","message":{"content":"x"}}]}"#,
                false,
            ),
        ];
        let run = |body: String| {
            let l = OpenAiLlm::new(
                Some(fake_server(vec![(200, body)])),
                "m".into(),
                None,
                10,
                &Tls::default(),
                1,
                gen(false, Effort::Low, false, 100),
            );
            let stats = CallStats::default();
            let r = l.complete(
                None,
                None,
                &stats,
                "s".into(),
                "u".into(),
                Reply::File,
                None,
            );
            (r, stats)
        };
        for body in blocked {
            let (r, stats) = run(body);
            assert!(
                matches!(&r, Err(Error::Refused(m)) if m == BLOCKED),
                "{r:?}"
            );
            assert_eq!(stats.usage().map(|u| (u.input, u.output)), Some((10, 3)));
            assert_eq!(stats.sends(), 1);
        }
        for (body, ok) in not_blocked {
            let (r, _) = run(body.to_string());
            if ok {
                assert_eq!(r.unwrap(), "x", "{body}");
            } else {
                assert!(matches!(&r, Err(Error::Truncated { .. })), "{body} {r:?}");
            }
        }
    }

    #[test]
    fn req274_ac3_stream_refusal_deltas_are_blocked() {
        let data = |v: serde_json::Value| format!("data: {v}\n\n");
        let mut s = data(serde_json::json!({"choices":[{"delta":{"refusal":"I can"}}]}));
        s += &data(serde_json::json!({"choices":[{"delta":{"refusal":"'t"}}]}));
        s += &data(serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}));
        s += "data: [DONE]\n\n";
        let (r, _) = streamed(s);
        assert!(
            matches!(&r, Err(Error::Refused(m)) if m == BLOCKED),
            "{r:?}"
        );
    }

    /// Flux OpenAI : texte en deux morceaux (s'il y en a un), `finish_reason`, `usage`, `[DONE]`.
    fn stream(text: Option<&str>, finish: &str) -> String {
        let data = |v: serde_json::Value| format!("data: {v}\n\n");
        let mut s = data(serde_json::json!({"choices":[{"delta":{"role":"assistant"}}]}));
        if let Some(t) = text {
            let (a, b) = t.split_at(1);
            for part in [a, b] {
                s += &data(serde_json::json!({"choices":[{"delta":{"content":part}}]}));
            }
        }
        s += &data(serde_json::json!({"choices":[{"delta":{},"finish_reason":finish}]}));
        s += &data(
            serde_json::json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3}}),
        );
        s + "data: [DONE]\n\n"
    }

    fn streamed(body: String) -> (Result<String>, CallStats) {
        let l = OpenAiLlm::new(
            Some(crate::llm::usage::sse_server(body)),
            "m".into(),
            None,
            10,
            &Tls::default(),
            1,
            gen(false, Effort::Low, false, 100),
        );
        let stats = CallStats::default();
        let r = l.complete(
            None,
            None,
            &stats,
            "s".into(),
            "u".into(),
            Reply::File,
            None,
        );
        (r, stats)
    }

    #[test]
    fn req249_ac4_stream_length_is_truncated_with_the_received_text() {
        let (r, stats) = streamed(stream(Some("abc"), "length"));
        assert!(
            matches!(&r, Err(Error::Truncated { message: m, partial }) if m == TRUNCATED && partial == "abc"),
            "{r:?}"
        );
        assert_eq!(stats.usage().map(|u| (u.input, u.output)), Some((10, 3)));
    }

    #[test]
    fn req249_ac4_stream_without_content_is_the_format_error() {
        let (r, _) = streamed(stream(None, "stop"));
        assert!(
            matches!(&r, Err(Error::Format(m)) if m == "réponse de openai sans message"),
            "{r:?}"
        );
    }

    #[test]
    fn req249_ac4_complete_stream_gives_the_text() {
        let (r, _) = streamed(stream(Some("abc"), "stop"));
        assert_eq!(r.unwrap(), "abc");
    }

    #[test]
    fn req202_ac4_corps_openai_avec_schema() {
        for (name, schema) in [
            ("nephila_fichier", crate::llm::schema::file()),
            ("nephila_lot", crate::llm::schema::lot()),
        ] {
            let f = Some((name, &schema));
            let rf = serde_json::json!({"type": "json_schema",
                "json_schema": {"name": name, "strict": true, "schema": schema}});
            let b = request_body("m", &gen(false, Effort::Low, false, 1234), "s", "u", f);
            let want = request_body("m", &gen(false, Effort::Low, false, 1234), "s", "u", None);
            assert_eq!(b["response_format"], rf);
            assert!(b.get("reasoning_effort").is_none());
            let mut b2 = b.clone();
            b2.as_object_mut().unwrap().remove("response_format");
            assert_eq!(b2, want);
            let b = request_body("m", &gen(true, Effort::Medium, true, 1234), "s", "u", f);
            assert_eq!(b["response_format"], rf);
            assert_eq!(b["reasoning_effort"], "medium");
        }
    }

    use crate::llm::usage::recording_server;

    const REFUSED_OPENAI: &str = r#"{"error":{"message":"Invalid parameter: \u0027response_format\u0027 is not supported"}}"#;
    const OK_OPENAI: &str = r#"{"choices":[{"message":{"content":"x"},"finish_reason":"stop"}]}"#;

    fn llm_structured(base: String, structured: Structured, key: &'static str) -> OpenAiLlm {
        OpenAiLlm::new(
            Some(base),
            "m".into(),
            Some("k".into()),
            10,
            &Tls::default(),
            2,
            gen(false, Effort::Low, false, 100),
        )
        .with_structured(structured)
        .with_structured_key(key)
    }

    fn expected_body(with_schema: bool) -> String {
        let schema = Reply::File.schema();
        let format = with_schema.then(|| (Reply::File.name(), &schema));
        request_body("m", &gen(false, Effort::Low, false, 100), "s", "u", format).to_string()
    }

    /// Corps attendu d'un appel direct : celui du lot, plus `stream` (REQ-249).
    fn live_body(with_schema: bool) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&expected_body(with_schema)).unwrap();
        v["stream"] = serde_json::json!(true);
        v["stream_options"] = serde_json::json!({"include_usage": true});
        v.to_string()
    }

    fn same_json(a: &str, b: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(a).unwrap()
            == serde_json::from_str::<serde_json::Value>(b).unwrap()
    }

    fn call(l: &OpenAiLlm, stats: &CallStats) -> Result<String> {
        l.complete(None, None, stats, "s".into(), "u".into(), Reply::File, None)
    }

    fn rejected_code(e: &Error) -> u16 {
        match e {
            Error::Rejected { code, .. } => *code,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn req203_ac1_refus_du_schema_puis_appels_sans_schema() {
        let (base, bodies) = recording_server(
            vec![(400, REFUSED_OPENAI.into()), (200, OK_OPENAI.into())],
            0,
        );
        let l = llm_structured(base, Structured::Auto, "structured_output");
        for _ in 0..3 {
            let stats = CallStats::default();
            assert_eq!(call(&l, &stats).unwrap(), "x");
        }
        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 4);
        assert!(same_json(&b[0], &live_body(true)));
        for later in &b[1..] {
            assert!(same_json(later, &live_body(false)), "{later}");
        }
        assert_ne!(expected_body(true), expected_body(false));
    }

    #[test]
    fn req203_ac2_refus_qui_ne_replie_pas() {
        let cases: [(u16, &str, Structured); 4] = [
            (400, r#"{"error":{}}"#, Structured::Auto),
            (
                400,
                r#"{"error":{"message":"model unknown"}}"#,
                Structured::Auto,
            ),
            (
                422,
                r#"{"error":{"message":"bad schema"}}"#,
                Structured::Auto,
            ),
            (400, r#"{"error":{"message":"bad schema"}}"#, Structured::On),
        ];
        for (code, body, structured) in cases {
            let (base, bodies) = recording_server(vec![(code, body.into())], 0);
            let l = llm_structured(base, structured, "structured_output");
            let stats = CallStats::default();
            let (r, events) = crate::trace::capture(|| call(&l, &stats));
            assert_eq!(rejected_code(&r.unwrap_err()), code, "{body}");
            assert_eq!(bodies.lock().unwrap().len(), 1, "{code} {body}");
            assert!(events.is_empty(), "{events:?}");
        }
    }

    #[test]
    fn req203_ac2_mots_cites_sans_casse() {
        for message in ["Bad FORMAT", "invalid Schema here"] {
            let body = format!(r#"{{"error":{{"message":"{message}"}}}}"#);
            let (base, bodies) = recording_server(vec![(400, body), (200, OK_OPENAI.into())], 0);
            let l = llm_structured(base, Structured::Auto, "structured_output");
            assert_eq!(call(&l, &CallStats::default()).unwrap(), "x", "{message}");
            assert_eq!(bodies.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn req203_ac3_renvoi_refuse_a_son_tour() {
        let (base, bodies) = recording_server(vec![(400, REFUSED_OPENAI.into())], 0);
        let l = llm_structured(base, Structured::Auto, "structured_output");
        let e = call(&l, &CallStats::default()).unwrap_err();
        assert_eq!(rejected_code(&e), 400);
        assert!(e
            .to_string()
            .starts_with("requête refusée par openai (HTTP 400) sur "));
        assert_eq!(bodies.lock().unwrap().len(), 2);
    }

    #[test]
    fn req203_ac4_deux_fils_refuses_une_seule_trace() {
        let (base, bodies) = recording_server(
            vec![
                (400, REFUSED_OPENAI.into()),
                (400, REFUSED_OPENAI.into()),
                (200, OK_OPENAI.into()),
            ],
            2,
        );
        let l = llm_structured(base, Structured::Auto, "structured_output");
        let traces: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        let (r, events) = crate::trace::capture(|| call(&l, &CallStats::default()));
                        assert_eq!(r.unwrap(), "x");
                        events.len()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(traces, 1);
        assert_eq!(call(&l, &CallStats::default()).unwrap(), "x");
        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 5);
        assert!(b[..2].iter().all(|x| same_json(x, &live_body(true))));
        assert!(b[2..].iter().all(|x| same_json(x, &live_body(false))));
    }

    #[test]
    fn req203_ac5_comptes_d_un_appel_replie() {
        let (base, _) = recording_server(
            vec![(400, REFUSED_OPENAI.into()), (200, OK_OPENAI.into())],
            0,
        );
        let l = llm_structured(base, Structured::Auto, "structured_output");
        let stats = CallStats::default();
        assert_eq!(call(&l, &stats).unwrap(), "x");
        assert_eq!(stats.sends(), 2);
        assert!(stats.responded());
    }

    #[test]
    fn req203_ac6_trace_de_repli() {
        for key in ["structured_output", "ask.structured_output"] {
            let (base, _) = recording_server(
                vec![(400, REFUSED_OPENAI.into()), (200, OK_OPENAI.into())],
                0,
            );
            let l = llm_structured(base, Structured::Auto, key);
            let (r, events) = crate::trace::capture(|| {
                call(&l, &CallStats::default()).unwrap();
                call(&l, &CallStats::default()).unwrap()
            });
            assert_eq!(r, "x");
            assert_eq!(
                events,
                [format!("sortie structurée: schéma JSON refusé par openai (HTTP 400), suite de la commande sans schéma ({key} = \"auto\")")]
            );
        }
    }

    fn batch_llm(c: &std::sync::Arc<crate::llm::batch::Collector>) -> OpenAiLlm {
        llm_structured(
            "http://127.0.0.1:1".into(),
            Structured::Auto,
            "structured_output",
        )
        .with_collector(c.clone())
    }

    #[test]
    fn req214_ac5_refus_du_schema_en_lot_replie_et_met_de_cote_sans_schema() {
        use crate::llm::batch::{body_id, Answer, Collector};
        let c = std::sync::Arc::new(Collector::new());
        let l = batch_llm(&c);
        let with: serde_json::Value = serde_json::from_str(&expected_body(true)).unwrap();
        let without: serde_json::Value = serde_json::from_str(&expected_body(false)).unwrap();
        c.add_answer(
            body_id(&with),
            Answer::Failed {
                cause: "invalid_request_error : response_format is not supported".into(),
                refused: Some("response_format is not supported".into()),
            },
        );
        // Un test muet les traces : celle du repli doit tout de même sortir.
        struct Unmute;
        impl Drop for Unmute {
            fn drop(&mut self) {
                crate::trace::mute(false);
            }
        }
        let _unmute = Unmute;
        crate::trace::mute(true);
        let (r, events) = crate::trace::capture(|| call(&l, &CallStats::default()));
        crate::trace::mute(false);
        assert_eq!(r, Err(Error::Deferred));
        let pend = c.take_pending();
        assert_eq!(pend.len(), 1);
        assert_eq!(pend[0].body, without);
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(events[0].starts_with("sortie structurée: schéma JSON refusé par openai"));
        // Sa réponse connue est rendue.
        c.add_answer(pend[0].id.clone(), Answer::Raw(OK_OPENAI.into()));
        let stats = CallStats::default();
        assert_eq!(call(&l, &stats).unwrap(), "x");
        assert!(stats.responded());
        // Le repli reste posé : les appels suivants partent sans schéma, sans nouvelle trace.
        let (r, events) = crate::trace::capture(|| call(&l, &CallStats::default()));
        assert_eq!(r.unwrap(), "x");
        assert!(events.is_empty());
    }

    #[test]
    fn req214_ac5_refus_sans_mention_de_schema_donne_batch_item() {
        use crate::llm::batch::{body_id, Answer, Collector};
        let c = std::sync::Arc::new(Collector::new());
        let l = batch_llm(&c);
        let with: serde_json::Value = serde_json::from_str(&expected_body(true)).unwrap();
        c.add_answer(
            body_id(&with),
            Answer::Failed {
                cause: "invalid_request_error : model unknown".into(),
                refused: Some("model unknown".into()),
            },
        );
        let e = call(&l, &CallStats::default()).unwrap_err();
        assert_eq!(
            e,
            Error::BatchItem {
                cause: "invalid_request_error : model unknown".into(),
                refused: Some("model unknown".into())
            }
        );
        assert!(c.take_pending().is_empty());
        // Sous `On`, même un refus qui cite le schéma ne replie pas.
        let c = std::sync::Arc::new(Collector::new());
        let l = llm_structured(
            "http://127.0.0.1:1".into(),
            Structured::On,
            "structured_output",
        )
        .with_collector(c.clone());
        c.add_answer(
            body_id(&with),
            Answer::Failed {
                cause: "x".into(),
                refused: Some("bad schema".into()),
            },
        );
        assert!(matches!(
            call(&l, &CallStats::default()),
            Err(Error::BatchItem { .. })
        ));
        assert!(c.take_pending().is_empty());
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::llm::usage::recording_server;
    use crate::llm::Llm;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn req268_ac4_openai_has_no_floor_and_keeps_a_string_content() {
        let text = r#"{\"summaries\":[],\"concepts\":[]}"#;
        let reply = format!(
            r#"{{"choices":[{{"finish_reason":"stop","message":{{"content":"{text}"}}}}]}}"#
        );
        let (base, seen) = recording_server(vec![(200, reply)], 0);
        let l = OpenAiLlm::new(
            Some(base),
            "m".into(),
            None,
            10,
            &Tls::default(),
            1,
            Generation {
                temperature: None,
                effort: Effort::Low,
                effort_explicit: false,
                thinking: false,
                max_output_tokens: 100,
            },
        );
        assert_eq!(Llm::cache_floor(&l), None);
        l.enrich_labeled(
            "l",
            "A.java",
            1,
            None,
            "t",
            &[],
            &[],
            &AtomicBool::new(false),
            &CallStats::default(),
            None,
            true,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
        assert!(body["messages"][1]["content"].is_string(), "{body}");
    }

    #[test]
    fn req268_ac4_mock_has_no_floor() {
        let m = crate::llm::mock::MockLlm::new(crate::llm::mock::MockOptions::default());
        assert_eq!(Llm::cache_floor(&m), None);
    }
}
