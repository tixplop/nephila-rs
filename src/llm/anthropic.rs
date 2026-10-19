//! Adaptateur Anthropic Messages (REQ-002).

use super::ask::{ask_system_prompt, ask_user_prompt, AskReply};
use super::enrich::{
    enrichment_system_prompt, enrichment_user_prompt, lot_system_prompt, lot_user_prompt, parse_lot,
};
use super::{http, Llm, Tls};
use super::{Enrichment, LotItem};
use crate::error::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 8192;

pub struct AnthropicLlm {
    base_url: Option<String>,
    model: String,
    key: String,
    agent: ureq::Agent,
}

impl AnthropicLlm {
    pub fn new(
        base_url: Option<String>,
        model: String,
        key: String,
        timeout_secs: u64,
        tls: &Tls,
    ) -> AnthropicLlm {
        AnthropicLlm {
            base_url,
            model,
            key,
            agent: http::agent(timeout_secs, tls),
        }
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &http::url(&self.base_url, DEFAULT_BASE_URL, path))
            .set("x-api-key", &self.key)
            .set("anthropic-version", API_VERSION)
    }

    /// Appelle `/v1/messages` et retourne le texte de la réponse.
    fn complete(&self, system: String, user: String) -> Result<String> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        });
        let raw = http::send(
            "anthropic",
            self.request("POST", "/v1/messages"),
            Some(&body),
        )?;
        let v = http::parse_body("anthropic", &raw)?;
        v["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .map(String::from)
            .ok_or_else(|| Error::Format("réponse de anthropic sans bloc texte".into()))
    }
}

impl Llm for AnthropicLlm {
    fn verify_key(&self) -> Result<()> {
        http::send("anthropic", self.request("GET", "/v1/models"), None).map(|_| ())
    }

    fn enrich(
        &self,
        file: &str,
        chunk: usize,
        text: &str,
        node_ids: &[String],
    ) -> Result<Enrichment> {
        let out = self.complete(
            enrichment_system_prompt(),
            enrichment_user_prompt(file, chunk, text, node_ids),
        )?;
        Enrichment::parse(http::extract_json(&out)?, file, node_ids)
    }

    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
        let out = self.complete(lot_system_prompt(), lot_user_prompt(items))?;
        Ok(parse_lot(http::extract_json(&out)?, items)?.into_results())
    }

    fn ask(&self, question: &str, context: &str) -> Result<AskReply> {
        let out = self.complete(ask_system_prompt(), ask_user_prompt(question, context))?;
        AskReply::parse(http::extract_json(&out)?)
    }
}
