//! Adaptateur Anthropic Messages (REQ-002).

use super::{http, Complete, Generation, Retry, Tls};
use crate::config::Effort;
use crate::error::{Error, Result, TRUNCATED};
use std::sync::atomic::AtomicBool;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Vrai si `model` est un ancien modèle Claude, qui refuse `output_config.effort` (REQ-157).
///
/// Normalisation : minuscules, `.`, `@`, `:` et `_` remplacés par `-`. On lit ce qui suit la
/// dernière occurrence de `claude-`, découpé sur `-`. Liste fermée : seuls les modèles sortis
/// avant Opus 4.5 sont anciens.
pub fn is_legacy_claude(model: &str) -> bool {
    let norm: String = model
        .to_lowercase()
        .chars()
        .map(|c| {
            if matches!(c, '.' | '@' | ':' | '_') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let Some(at) = norm.rfind("claude-") else {
        return false;
    };
    let segs: Vec<&str> = norm[at + "claude-".len()..].split('-').collect();
    let number = |s: &str| -> Option<u32> {
        if !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let s0 = segs[0];
    if let Some(n) = number(s0) {
        return n <= 3;
    }
    if s0 == "instant" {
        return true;
    }
    if !matches!(s0, "opus" | "sonnet" | "haiku") {
        return false;
    }
    let Some(major) = segs.get(1).and_then(|s| number(s)) else {
        return false;
    };
    let minor = segs
        .get(2)
        .filter(|s| s.len() <= 2)
        .and_then(|s| number(s))
        .unwrap_or(0);
    if s0 == "opus" && major == 4 && minor == 5 {
        return false;
    }
    major <= 3 || (major == 4 && minor <= 5)
}

/// Corps de la requête `/v1/messages` (REQ-058, REQ-157, DEC-126) : jamais de `thinking`
/// désactivé ; `thinking` adaptatif si demandé ; `output_config.effort` sauf `effort = "default"`
/// ou ancien modèle Claude sans `thinking`.
pub fn request_body(model: &str, gen: &Generation, system: &str, user: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": gen.max_output_tokens,
        "system": system,
        "messages": [{"role": "user", "content": user}],
    });
    if gen.thinking {
        body["thinking"] = serde_json::json!({"type": "adaptive"});
    }
    if gen.effort != Effort::ModelDefault && (gen.thinking || !is_legacy_claude(model)) {
        body["output_config"] = serde_json::json!({"effort": gen.effort.as_str()});
    }
    body
}

pub struct AnthropicLlm {
    base_url: Option<String>,
    model: String,
    key: String,
    agent: ureq::Agent,
    retry: http::Retry,
    gen: Generation,
}

impl AnthropicLlm {
    pub fn new(
        base_url: Option<String>,
        model: String,
        key: String,
        timeout_secs: u64,
        tls: &Tls,
        concurrency: usize,
        gen: Generation,
    ) -> AnthropicLlm {
        AnthropicLlm {
            base_url,
            model,
            key,
            agent: http::agent(timeout_secs, concurrency, tls),
            retry: http::Retry::default(),
            gen,
        }
    }

    /// Base d'attente des nouveaux essais HTTP (REQ-052).
    pub fn with_retry(mut self, retry: Retry) -> AnthropicLlm {
        self.retry = retry;
        self
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &http::url(&self.base_url, DEFAULT_BASE_URL, path))
            .set("x-api-key", &self.key)
            .set("anthropic-version", API_VERSION)
    }
}

impl Complete for AnthropicLlm {
    /// Appelle `/v1/messages` et retourne le texte de la réponse.
    fn complete(
        &self,
        label: Option<&str>,
        stop: Option<&AtomicBool>,
        system: String,
        user: String,
    ) -> Result<String> {
        let body = request_body(&self.model, &self.gen, &system, &user);
        let raw = http::send(
            "anthropic",
            self.request("POST", "/v1/messages"),
            Some(&body),
            &self.retry,
            label,
            stop,
        )?;
        let v = http::parse_body("anthropic", &raw)?;
        if v["stop_reason"] == "max_tokens" {
            return Err(Error::Truncated(TRUNCATED.into()));
        }
        v["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .map(String::from)
            .ok_or_else(|| Error::Format("réponse de anthropic sans bloc texte".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn gen(thinking: bool, effort: Effort, max: u32) -> Generation {
        Generation {
            effort,
            effort_explicit: false,
            thinking,
            max_output_tokens: max,
        }
    }

    fn base_fields() -> serde_json::Value {
        serde_json::json!({
            "model": "claude-opus-5-5", "max_tokens": 1234, "system": "s",
            "messages": [{"role": "user", "content": "u"}],
        })
    }

    #[test]
    fn req157_ac1_body_without_thinking_sends_effort_on_current_model() {
        let m = "claude-opus-5-5";
        let b = request_body(m, &gen(false, Effort::Low, 1234), "s", "u");
        let mut want = base_fields();
        want["output_config"] = serde_json::json!({"effort": "low"});
        assert_eq!(b, want);
        assert_eq!(b.as_object().unwrap().len(), 5);
        assert!(b.get("thinking").is_none());

        let b = request_body(m, &gen(false, Effort::High, 1234), "s", "u");
        assert_eq!(b["output_config"], serde_json::json!({"effort": "high"}));
        assert!(b.get("thinking").is_none());

        let b = request_body(m, &gen(false, Effort::ModelDefault, 1234), "s", "u");
        assert_eq!(b, base_fields());
    }

    #[test]
    fn req157_ac1_body_with_thinking_default_has_only_thinking() {
        let b = request_body(
            "claude-opus-5-5",
            &gen(true, Effort::ModelDefault, 1234),
            "s",
            "u",
        );
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert!(b.get("output_config").is_none());
        assert_eq!(b.as_object().unwrap().len(), 5);
    }

    #[test]
    fn req157_ac1_legacy_model_gets_no_effort_without_thinking() {
        let m = "claude-sonnet-4-5";
        let b = request_body(m, &gen(false, Effort::High, 1234), "s", "u");
        let mut want = base_fields();
        want["model"] = m.into();
        assert_eq!(b, want);
        let b = request_body(m, &gen(true, Effort::Medium, 1234), "s", "u");
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(b["output_config"], serde_json::json!({"effort": "medium"}));
        assert_eq!(b.as_object().unwrap().len(), 6);
    }

    #[test]
    fn body_with_thinking_adds_adaptive_and_effort() {
        let b = request_body("m", &gen(true, Effort::Medium, 16384), "s", "u");
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(b["output_config"], serde_json::json!({"effort": "medium"}));
        assert_eq!(b["max_tokens"], 16384);
        assert_eq!(b.as_object().unwrap().len(), 6);
    }

    #[test]
    fn req157_ac2_is_legacy_claude() {
        let legacy = [
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-1",
            "claude-opus-4-1-20250805",
            "claude-opus-4-0",
            "claude-opus-4-20250514",
            "claude-opus-4",
            "claude-sonnet-4-0",
            "claude-sonnet-4-20250514",
            "claude-sonnet-4",
            "claude-3-7-sonnet-latest",
            "claude-3-5-haiku-20241022",
            "claude-3-opus-20240229",
            "claude-3-haiku-20240307",
            "claude-2.1",
            "claude-instant-1.2",
            "CLAUDE-SONNET-4-5",
            "claude-sonnet-4.5",
            "anthropic/claude-sonnet-4.5",
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "claude-opus-4@20250514",
        ];
        for m in legacy {
            assert!(is_legacy_claude(m), "{m} devrait etre ancien");
        }
        let current = [
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-opus-5-5",
            "claude-sonnet-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "claude-mythos-preview",
            "claude-x",
            "m",
            "x",
            "mock",
            "gpt-4o-mini",
            "",
        ];
        for m in current {
            assert!(!is_legacy_claude(m), "{m:?} ne devrait pas etre ancien");
        }
    }
}
