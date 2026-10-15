//! Réponse et prompts de `query ask` (REQ-003).

use serde::Deserialize;

use crate::error::{Error, Result};

fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::Message(format!("réponse LLM non conforme au schéma : {msg}"))
}

/// Réponse à `query ask`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskReply {
    Found { answer: String, nodes: Vec<String> },
    NotFound,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsk {
    found: bool,
    answer: Option<String>,
    nodes: Option<Vec<String>>,
}

impl AskReply {
    pub fn parse(json: &str) -> Result<AskReply> {
        let raw: RawAsk = serde_json::from_str(json).map_err(invalid)?;
        if !raw.found {
            if raw.answer.is_some() || raw.nodes.is_some() {
                return Err(invalid("« found » faux avec une réponse"));
            }
            return Ok(AskReply::NotFound);
        }
        let answer = raw
            .answer
            .filter(|a| !a.trim().is_empty())
            .ok_or_else(|| invalid("« answer » manquant"))?;
        Ok(AskReply::Found {
            answer,
            nodes: raw.nodes.unwrap_or_default(),
        })
    }
}

/// Prompt système de `query ask`.
pub fn ask_system_prompt() -> String {
    "Tu réponds à une question sur un dépôt à partir du seul contexte de graphe fourni. \
     Réponds par un objet JSON : {\"found\": true, \"answer\": \"...\", \"nodes\": [\"identifiants cités\"]} \
     ou {\"found\": false} si le contexte ne permet pas de répondre."
        .to_string()
}

pub fn ask_user_prompt(question: &str, context: &str) -> String {
    format!("Question : {question}\n\nContexte :\n{context}")
}
