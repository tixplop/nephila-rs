//! Abstraction du fournisseur LLM (REQ-002, REQ-003, REQ-006).

pub mod anthropic;
pub mod ask;
pub mod enrich;
mod http;
pub use http::Tls;
pub mod mock;
pub mod openai;
pub mod retry;

use crate::config::{Config, Provider};
use crate::error::{Error, Result};
pub use ask::AskReply;
pub use enrich::{Enrichment, LotItem};
pub use retry::with_format_retries;

/// Fournisseur LLM. Les implémentations sont partagées entre threads.
pub trait Llm: Send + Sync {
    /// Enrichit un appel : résumés et concepts (schéma version 2). `requested` = nœuds écrits
    /// dans le prompt ; `known` = liste complète du fichier, qui filtre la réponse.
    fn enrich(
        &self,
        file: &str,
        call: usize,
        text: &str,
        requested: &[String],
        known: &[String],
    ) -> Result<Enrichment>;
    /// Enrichit un lot de 2 fichiers ou plus en un seul appel (schéma version 3, DEC-033).
    /// `Err` = échec de l'appel entier ; sinon un résultat par fichier du lot, dans l'ordre.
    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>>;
    /// Répond à une question à partir d'un contexte de graphe.
    fn ask(&self, question: &str, context: &str) -> Result<AskReply>;
}

/// Construit le fournisseur décrit par la configuration.
pub fn build_llm(config: &Config) -> Result<Box<dyn Llm>> {
    let tls = Tls {
        ca_certs: config.tls_ca_certs.clone(),
        insecure: config.tls_insecure,
    };
    if config.tls_insecure && config.provider != Provider::Mock {
        let default = match config.provider {
            Provider::Anthropic => "https://api.anthropic.com",
            _ => "https://api.openai.com",
        };
        let url = config.base_url.as_deref().unwrap_or(default);
        if url.starts_with("https://") {
            crate::error::warn(
                "tls_insecure = true : contrôle des certificats du serveur désactivé",
            );
        }
    }
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
                &tls,
            )))
        }
        Provider::OpenAi => Ok(Box::new(openai::OpenAiLlm::new(
            config.base_url.clone(),
            config.model.clone(),
            config.api_key()?,
            config.timeout_secs,
            &tls,
        ))),
    }
}
