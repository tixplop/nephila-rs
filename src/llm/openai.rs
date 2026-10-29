//! Adaptateur compatible OpenAI Chat Completions (REQ-002) : OpenAI, Ollama, llama.cpp, vLLM.

use super::{http, Complete, Generation, Retry, Tls};
use crate::config::Effort;
use crate::error::{Error, Result, TRUNCATED};
use std::sync::atomic::AtomicBool;

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Corps de la requête `/v1/chat/completions` (REQ-058, REQ-157, DEC-126) : `reasoning_effort`
/// si l'effort n'est pas `"default"` et que la réflexion est demandée ou l'effort écrit.
pub fn request_body(model: &str, gen: &Generation, system: &str, user: &str) -> serde_json::Value {
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
    body
}

pub struct OpenAiLlm {
    base_url: Option<String>,
    model: String,
    key: Option<String>,
    agent: ureq::Agent,
    retry: http::Retry,
    gen: Generation,
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
        }
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
    fn complete(
        &self,
        label: Option<&str>,
        stop: Option<&AtomicBool>,
        system: String,
        user: String,
    ) -> Result<String> {
        let body = request_body(&self.model, &self.gen, &system, &user);
        let raw = http::send(
            "openai",
            self.request("POST", "/v1/chat/completions"),
            Some(&body),
            &self.retry,
            label,
            stop,
        )?;
        let v = http::parse_body("openai", &raw)?;
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
        let b = request_body("m", &gen(thinking, effort, explicit, 1234), "s", "u");
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
        let b = request_body("m", &gen(false, Effort::Low, false, 1234), "s", "u");
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
        let b = request_body("m", &gen(true, Effort::Medium, true, 16384), "s", "u");
        assert_eq!(b["reasoning_effort"], "medium");
        assert_eq!(b["max_completion_tokens"], 16384);
        assert!(b.get("max_tokens").is_none());
    }
}
