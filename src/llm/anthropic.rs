//! Adaptateur Anthropic Messages (REQ-002).

use super::extract::{
    ask_system_prompt, ask_user_prompt, extraction_system_prompt, extraction_user_prompt, AskReply,
    Extraction,
};
use super::{http, Llm};
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
    pub fn new(base_url: Option<String>, model: String, key: String) -> AnthropicLlm {
        AnthropicLlm {
            base_url,
            model,
            key,
            agent: http::agent(),
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
            .ok_or_else(|| Error::Message("réponse de anthropic sans bloc texte".into()))
    }
}

impl Llm for AnthropicLlm {
    fn verify_key(&self) -> Result<()> {
        http::send("anthropic", self.request("GET", "/v1/models"), None).map(|_| ())
    }

    fn extract(&self, file: &str, chunk: usize, text: &str) -> Result<Extraction> {
        let out = self.complete(
            extraction_system_prompt(),
            extraction_user_prompt(file, chunk, text),
        )?;
        Extraction::parse(http::extract_json(&out)?)
    }

    fn ask(&self, question: &str, context: &str) -> Result<AskReply> {
        let out = self.complete(ask_system_prompt(), ask_user_prompt(question, context))?;
        AskReply::parse(http::extract_json(&out)?)
    }
}
