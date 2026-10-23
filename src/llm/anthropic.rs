//! Adaptateur Anthropic Messages (REQ-002).

use super::ask::{ask_system_prompt, ask_user_prompt, AskReply};
use super::enrich::{
    enrichment_system_prompt, enrichment_user_prompt, lot_system_prompt, lot_user_prompt, parse_lot,
};
use super::{http, Generation, Llm, Retry, Tls};
use super::{Enrichment, LotItem};
use crate::error::{Error, Result, TRUNCATED};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Corps de la requête `/v1/messages` (REQ-058) : `thinking` et `output_config` seulement si la
/// réflexion est demandée.
pub fn request_body(model: &str, gen: &Generation, system: &str, user: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": gen.max_output_tokens,
        "system": system,
        "messages": [{"role": "user", "content": user}],
    });
    if gen.thinking {
        body["thinking"] = serde_json::json!({"type": "adaptive"});
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

    /// Appelle `/v1/messages` et retourne le texte de la réponse.
    fn complete(&self, label: Option<&str>, system: String, user: String) -> Result<String> {
        let body = request_body(&self.model, &self.gen, &system, &user);
        let raw = http::send(
            "anthropic",
            self.request("POST", "/v1/messages"),
            Some(&body),
            &self.retry,
            label,
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

impl Llm for AnthropicLlm {
    fn enrich(
        &self,
        file: &str,
        call: usize,
        text: &str,
        requested: &[String],
        known: &[String],
    ) -> Result<Enrichment> {
        self.enrich_with(None, file, call, text, requested, known)
    }

    fn enrich_labeled(
        &self,
        label: &str,
        file: &str,
        call: usize,
        text: &str,
        requested: &[String],
        known: &[String],
    ) -> Result<Enrichment> {
        self.enrich_with(Some(label), file, call, text, requested, known)
    }

    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
        self.enrich_lot_with(None, items)
    }

    fn enrich_lot_labeled(
        &self,
        label: &str,
        items: &[LotItem],
    ) -> Result<Vec<(String, Result<Enrichment>)>> {
        self.enrich_lot_with(Some(label), items)
    }

    fn ask(&self, question: &str, context: &str) -> Result<AskReply> {
        let out = self.complete(
            None,
            ask_system_prompt(),
            ask_user_prompt(question, context),
        )?;
        AskReply::parse(http::extract_json(&out)?)
    }
}

impl AnthropicLlm {
    fn enrich_with(
        &self,
        label: Option<&str>,
        file: &str,
        call: usize,
        text: &str,
        requested: &[String],
        known: &[String],
    ) -> Result<Enrichment> {
        let out = self.complete(
            label,
            enrichment_system_prompt(),
            enrichment_user_prompt(file, call, text, requested),
        )?;
        Enrichment::parse(http::extract_json(&out)?, file, known)
    }

    fn enrich_lot_with(
        &self,
        label: Option<&str>,
        items: &[LotItem],
    ) -> Result<Vec<(String, Result<Enrichment>)>> {
        let out = self.complete(label, lot_system_prompt(), lot_user_prompt(items))?;
        Ok(parse_lot(http::extract_json(&out)?, items)?.into_results())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn gen(thinking: bool, effort: Effort, max: u32) -> Generation {
        Generation {
            effort,
            thinking,
            max_output_tokens: max,
        }
    }

    #[test]
    fn body_without_thinking_has_only_the_base_fields() {
        let b = request_body("m", &gen(false, Effort::High, 1234), "s", "u");
        assert_eq!(
            b,
            serde_json::json!({
                "model": "m", "max_tokens": 1234, "system": "s",
                "messages": [{"role": "user", "content": "u"}],
            })
        );
    }

    #[test]
    fn body_with_thinking_adds_adaptive_and_effort() {
        let b = request_body("m", &gen(true, Effort::Medium, 16384), "s", "u");
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(b["output_config"], serde_json::json!({"effort": "medium"}));
        assert_eq!(b["max_tokens"], 16384);
        assert_eq!(b.as_object().unwrap().len(), 6);
    }
}
