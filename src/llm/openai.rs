//! Adaptateur compatible OpenAI Chat Completions (REQ-002) : OpenAI, Ollama, llama.cpp, vLLM.

use super::ask::{ask_system_prompt, ask_user_prompt, AskReply};
use super::enrich::{
    enrichment_system_prompt, enrichment_user_prompt, lot_system_prompt, lot_user_prompt, parse_lot,
};
use super::{http, Llm, Tls};
use super::{Enrichment, LotItem};
use crate::error::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

pub struct OpenAiLlm {
    base_url: Option<String>,
    model: String,
    key: Option<String>,
    agent: ureq::Agent,
}

impl OpenAiLlm {
    /// `key` est facultative (modèle local).
    pub fn new(
        base_url: Option<String>,
        model: String,
        key: Option<String>,
        timeout_secs: u64,
        tls: &Tls,
    ) -> OpenAiLlm {
        OpenAiLlm {
            base_url,
            model,
            key,
            agent: http::agent(timeout_secs, tls),
        }
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

    fn complete(&self, system: String, user: String) -> Result<String> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
        let raw = http::send(
            "openai",
            self.request("POST", "/v1/chat/completions"),
            Some(&body),
        )?;
        let v = http::parse_body("openai", &raw)?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::Message("réponse de openai sans message".into()))
    }
}

impl Llm for OpenAiLlm {
    fn verify_key(&self) -> Result<()> {
        http::send("openai", self.request("GET", "/v1/models"), None).map(|_| ())
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
