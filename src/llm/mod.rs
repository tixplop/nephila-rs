//! Abstraction du fournisseur LLM (REQ-002, REQ-003, REQ-006).

pub mod anthropic;
pub mod ask;
pub mod enrich;
mod http;
pub mod mock;
pub mod openai;

use crate::config::{Config, Provider};
use crate::error::{Error, Result};
pub use ask::AskReply;
pub use enrich::{Enrichment, LotItem};

/// Fournisseur LLM. Les implémentations sont partagées entre threads.
pub trait Llm: Send + Sync {
    /// Vérifie la clé par un appel sans coût de génération.
    fn verify_key(&self) -> Result<()>;
    /// Enrichit un morceau : résumés des nœuds `node_ids` et concepts (schéma version 2).
    fn enrich(
        &self,
        file: &str,
        chunk: usize,
        text: &str,
        node_ids: &[String],
    ) -> Result<Enrichment>;
    /// Enrichit un lot de 2 fichiers ou plus en un seul appel (schéma version 3, DEC-033).
    /// `Err` = échec de l'appel entier ; sinon un résultat par fichier du lot, dans l'ordre.
    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>>;
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
                config.timeout_secs,
            )))
        }
        Provider::OpenAi => Ok(Box::new(openai::OpenAiLlm::new(
            config.base_url.clone(),
            config.model.clone(),
            config.api_key()?,
            config.timeout_secs,
        ))),
    }
}
