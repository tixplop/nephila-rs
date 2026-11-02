//! Adaptateur compatible OpenAI Chat Completions (REQ-002) : OpenAI, Ollama, llama.cpp, vLLM.

use super::schema::Reply;
use super::{http, openai_usage, CallStats, Complete, Generation, Retry, Structured, Tls};
use crate::config::Effort;
use crate::error::{Error, Result, TRUNCATED};
use std::sync::atomic::AtomicBool;

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Corps de la requête `/v1/chat/completions` (REQ-058, REQ-157, DEC-126) : `reasoning_effort`
/// si l'effort n'est pas `"default"` et que la réflexion est demandée ou l'effort écrit ;
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
    gen: Generation,
    structured: Structured,
    summary_language: String,
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
            gen,
            structured: Structured::Off,
            summary_language: crate::config::DEFAULT_SUMMARY_LANGUAGE.to_string(),
        }
    }

    /// Réglage effectif de `structured_output` (REQ-202).
    pub fn with_structured(mut self, structured: Structured) -> OpenAiLlm {
        self.structured = structured;
        self
    }

    /// Langue des résumés demandée dans les prompts système (REQ-172).
    pub fn with_summary_language(mut self, language: String) -> OpenAiLlm {
        self.summary_language = language;
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

impl Complete for OpenAiLlm {
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
    ) -> Result<String> {
        let schema = (self.structured != Structured::Off).then(|| reply.schema());
        let format = schema.as_ref().map(|s| (reply.name(), s));
        let body = request_body(&self.model, &self.gen, &system, &user, format);
        let raw = http::send(
            "openai",
            self.request("POST", "/v1/chat/completions"),
            Some(&body),
            &self.retry,
            label,
            stop,
            stats,
        )?;
        let v = http::parse_body("openai", &raw)?;
        stats.record_response(openai_usage(&v));
        if v["choices"][0]["finish_reason"] == "length" {
            return Err(Error::Truncated(TRUNCATED.into()));
        }
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::Format("réponse de openai sans message".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn gen(thinking: bool, effort: Effort, explicit: bool, max: u32) -> Generation {
        Generation {
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
                .complete(None, None, &stats, "s".into(), "u".into(), Reply::File)
                .unwrap_err();
            if truncated {
                assert!(matches!(&e, Error::Truncated(m) if m == TRUNCATED), "{e:?}");
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
}
