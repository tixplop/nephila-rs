//! Abstraction du fournisseur LLM (REQ-002, REQ-003, REQ-006).

pub mod anthropic;
pub mod extract;
mod http;
pub mod mock;
pub mod openai;

use crate::config::{Config, Provider};
use crate::error::{Error, Result};
pub use extract::{AskReply, Extraction};

/// Fournisseur LLM. Les implémentations sont partagées entre threads.
pub trait Llm: Send + Sync {
    /// Vérifie la clé par un appel sans coût de génération.
    fn verify_key(&self) -> Result<()>;
    /// Extrait nœuds et références d'un morceau (`chunk` numéroté à partir de 1).
    fn extract(&self, file: &str, chunk: usize, text: &str) -> Result<Extraction>;
    /// Répond à une question à partir d'un contexte de graphe.
    fn ask(&self, question: &str, context: &str) -> Result<AskReply>;
}

/// Construit le fournisseur décrit par la configuration.
pub fn build_llm(config: &Config) -> Result<Box<dyn Llm>> {
    match config.provider {
        Provider::Mock => Ok(Box::new(mock::MockLlm::from_env()?)),
        Provider::Anthropic => {
            let key = config.api_key()?.ok_or_else(|| {
                Error::Message("clé api_key_env manquante pour le fournisseur anthropic".into())
            })?;
            Ok(Box::new(anthropic::AnthropicLlm::new(
                config.base_url.clone(),
                config.model.clone(),
                key,
            )))
        }
        Provider::OpenAi => Ok(Box::new(openai::OpenAiLlm::new(
            config.base_url.clone(),
            config.model.clone(),
            config.api_key()?,
        ))),
    }
}
