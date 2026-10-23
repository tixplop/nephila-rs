//! Adaptateur compatible OpenAI Chat Completions (REQ-002) : OpenAI, Ollama, llama.cpp, vLLM.

use super::ask::{ask_system_prompt, ask_user_prompt, AskReply};
use super::enrich::{
    enrichment_system_prompt, enrichment_user_prompt, lot_system_prompt, lot_user_prompt, parse_lot,
};
use super::{http, Generation, Llm, Retry, Tls};
use super::{Enrichment, LotItem};
use crate::error::{Error, Result, TRUNCATED};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Corps de la requête `/v1/chat/completions` (REQ-058) : `reasoning_effort` seulement si la
/// réflexion est demandée.
pub fn request_body(model: &str, gen: &Generation, system: &str, user: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_completion_tokens": gen.max_output_tokens,
    });
    if gen.thinking {
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

    fn complete(&self, label: Option<&str>, system: String, user: String) -> Result<String> {
        let body = request_body(&self.model, &self.gen, &system, &user);
        let raw = http::send(
            "openai",
            self.request("POST", "/v1/chat/completions"),
            Some(&body),
            &self.retry,
            label,
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

impl Llm for OpenAiLlm {
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

impl OpenAiLlm {
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
    fn body_without_thinking_has_no_reasoning_field() {
        let b = request_body("m", &gen(false, Effort::High, 1234), "s", "u");
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
    fn body_with_thinking_adds_reasoning_effort() {
        let b = request_body("m", &gen(true, Effort::Medium, 16384), "s", "u");
        assert_eq!(b["reasoning_effort"], "medium");
        assert_eq!(b["max_completion_tokens"], 16384);
        assert!(b.get("max_tokens").is_none());
    }
}
