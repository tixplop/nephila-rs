//! Abstraction du fournisseur LLM (REQ-002, REQ-003, REQ-006).

pub mod anthropic;
pub mod ask;
pub mod enrich;
mod http;
pub use http::{Retry, Tls};
pub mod mock;
pub mod openai;
pub mod retry;
mod schema;
mod tidy;
mod usage;
#[cfg(test)]
pub(crate) use usage::recording_server;
pub use usage::{
    anthropic_usage, calls_line, openai_usage, tokens_segment, CallStats, LlmTotals, Usage,
};

use std::sync::atomic::AtomicBool;

use crate::config::{Config, Effort, Provider, StructuredOutput};
use crate::error::{Error, Result};
pub use ask::AskReply;
use ask::{ask_system_prompt, ask_user_prompt};
use enrich::{
    enrichment_system_prompt, enrichment_user_prompt, lot_system_prompt, lot_user_prompt, parse_lot,
};
pub use enrich::{Enrichment, Excerpt, LotItem};
pub use retry::{with_format_retries, with_note, FormatFault};
use schema::Reply;

/// Options de génération communes aux fournisseurs HTTP (REQ-058, DEC-072).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    pub effort: Effort,
    /// La clé `effort` est écrite dans la configuration (REQ-157).
    pub effort_explicit: bool,
    pub thinking: bool,
    pub max_output_tokens: u32,
}

/// Valeur effective de `structured_output` (REQ-202, DEC-163) : `On` et `Auto` envoient le
/// schéma, `Off` jamais ; `Auto` rend `Off` quand `base_url` est écrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Structured {
    Off,
    On,
    Auto,
}

pub(crate) fn structured(config: &Config) -> Structured {
    match (config.structured_output, config.base_url.is_some()) {
        (StructuredOutput::On, _) => Structured::On,
        (StructuredOutput::Off, _) | (StructuredOutput::Auto, true) => Structured::Off,
        (StructuredOutput::Auto, false) => Structured::Auto,
    }
}

/// Fournisseur LLM. Les implémentations sont partagées entre threads.
pub trait Llm: Send + Sync {
    /// Modèle effectif, qui entre dans l'empreinte d'enrichissement (REQ-049). Vide par défaut.
    fn model(&self) -> &str {
        ""
    }
    /// Langue des résumés demandée, enregistrée dans l'empreinte d'enrichissement (REQ-173).
    fn summary_language(&self) -> &str {
        crate::config::DEFAULT_SUMMARY_LANGUAGE
    }
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
    /// Comme `enrich`, en nommant l'appel pour les traces d'attente des nouveaux essais HTTP
    /// (`<chemin> [i/n]`) et en passant l'arrêt de l'envoi, qui interrompt leur attente (REQ-095).
    /// Par défaut, le libellé et l'arrêt sont ignorés.
    #[allow(clippy::too_many_arguments)]
    fn enrich_labeled(
        &self,
        label: &str,
        file: &str,
        call: usize,
        excerpt: Option<Excerpt>,
        text: &str,
        requested: &[String],
        known: &[String],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<Enrichment> {
        let _ = (label, excerpt, stop, stats, fault);
        self.enrich(file, call, text, requested, known)
    }
    /// Enrichit un lot de 2 fichiers ou plus en un seul appel (schéma version 3, DEC-033).
    /// `Err` = échec de l'appel entier ; sinon un résultat par fichier du lot, dans l'ordre.
    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>>;
    /// Comme `enrich_lot`, en nommant l'appel (`lot <chemin> +<n-1> [1/1]`).
    fn enrich_lot_labeled(
        &self,
        label: &str,
        items: &[LotItem],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<Vec<(String, Result<Enrichment>)>> {
        let _ = (label, stop, stats, fault);
        self.enrich_lot(items)
    }
    /// Répond à une question à partir d'un contexte de graphe.
    fn ask(&self, question: &str, context: &str) -> Result<AskReply>;
    /// Comme `ask`, en relevant l'appel dans `stats`. Par défaut, le relevé est ignoré.
    fn ask_with(
        &self,
        question: &str,
        context: &str,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<AskReply> {
        let _ = (stats, fault);
        self.ask(question, context)
    }
}

/// Appel de complétion d'un adaptateur HTTP : le reste de `Llm` s'en déduit (REQ-127).
pub(crate) trait Complete: Send + Sync {
    /// Langue des résumés demandée dans les prompts système (REQ-172).
    fn summary_language(&self) -> &str;
    /// Envoie la consigne système et la requête, retourne le texte de la réponse.
    fn complete(
        &self,
        label: Option<&str>,
        stop: Option<&AtomicBool>,
        stats: &CallStats,
        system: String,
        user: String,
        reply: Reply,
    ) -> Result<String>;
}

/// Clé de premier niveau d'une réponse d'enrichissement d'un fichier seul.
const KEY_SUMMARIES: &str = "summaries";
/// Clé de premier niveau d'une réponse d'enrichissement d'un lot.
const KEY_FILES: &str = "files";
/// Clé de premier niveau d'une réponse de `query ask`.
const KEY_FOUND: &str = "found";

impl<T: Complete> Llm for T {
    fn summary_language(&self) -> &str {
        Complete::summary_language(self)
    }

    fn enrich(
        &self,
        file: &str,
        _call: usize,
        text: &str,
        requested: &[String],
        known: &[String],
    ) -> Result<Enrichment> {
        let out = self.complete(
            None,
            None,
            &CallStats::default(),
            enrichment_system_prompt(self.summary_language()),
            enrichment_user_prompt(file, None, text, requested),
            Reply::File,
        )?;
        http::parse_reply(&out, KEY_SUMMARIES, |j| Enrichment::parse(j, file, known))
    }

    #[allow(clippy::too_many_arguments)]
    fn enrich_labeled(
        &self,
        label: &str,
        file: &str,
        _call: usize,
        excerpt: Option<Excerpt>,
        text: &str,
        requested: &[String],
        known: &[String],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<Enrichment> {
        let out = self.complete(
            Some(label),
            Some(stop),
            stats,
            enrichment_system_prompt(self.summary_language()),
            with_note(
                enrichment_user_prompt(file, excerpt, text, requested),
                fault,
            ),
            Reply::File,
        )?;
        http::parse_reply(&out, KEY_SUMMARIES, |j| Enrichment::parse(j, file, known))
    }

    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
        let out = self.complete(
            None,
            None,
            &CallStats::default(),
            lot_system_prompt(self.summary_language()),
            lot_user_prompt(items),
            Reply::Lot,
        )?;
        Ok(http::parse_reply(&out, KEY_FILES, |j| parse_lot(j, items))?.into_results())
    }

    fn enrich_lot_labeled(
        &self,
        label: &str,
        items: &[LotItem],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<Vec<(String, Result<Enrichment>)>> {
        let out = self.complete(
            Some(label),
            Some(stop),
            stats,
            lot_system_prompt(self.summary_language()),
            with_note(lot_user_prompt(items), fault),
            Reply::Lot,
        )?;
        Ok(http::parse_reply(&out, KEY_FILES, |j| parse_lot(j, items))?.into_results())
    }

    fn ask(&self, question: &str, context: &str) -> Result<AskReply> {
        self.ask_with(question, context, &CallStats::default(), None)
    }

    fn ask_with(
        &self,
        question: &str,
        context: &str,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<AskReply> {
        let out = self.complete(
            None,
            None,
            stats,
            ask_system_prompt(),
            with_note(ask_user_prompt(question, context), fault),
            Reply::Ask,
        )?;
        http::parse_reply(&out, KEY_FOUND, AskReply::parse)
    }
}

/// Fournisseur qui porte le modèle effectif de la configuration (le simulé n'en a pas).
struct WithModel {
    inner: Box<dyn Llm>,
    model: String,
    summary_language: String,
}

impl Llm for WithModel {
    fn model(&self) -> &str {
        &self.model
    }

    fn summary_language(&self) -> &str {
        &self.summary_language
    }

    fn enrich(
        &self,
        file: &str,
        call: usize,
        text: &str,
        requested: &[String],
        known: &[String],
    ) -> Result<Enrichment> {
        self.inner.enrich(file, call, text, requested, known)
    }

    fn enrich_labeled(
        &self,
        label: &str,
        file: &str,
        call: usize,
        excerpt: Option<Excerpt>,
        text: &str,
        requested: &[String],
        known: &[String],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<Enrichment> {
        self.inner.enrich_labeled(
            label, file, call, excerpt, text, requested, known, stop, stats, fault,
        )
    }

    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
        self.inner.enrich_lot(items)
    }

    fn enrich_lot_labeled(
        &self,
        label: &str,
        items: &[LotItem],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<Vec<(String, Result<Enrichment>)>> {
        self.inner
            .enrich_lot_labeled(label, items, stop, stats, fault)
    }

    fn ask(&self, question: &str, context: &str) -> Result<AskReply> {
        self.inner.ask(question, context)
    }

    fn ask_with(
        &self,
        question: &str,
        context: &str,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<AskReply> {
        self.inner.ask_with(question, context, stats, fault)
    }
}

/// Construit le fournisseur décrit par la configuration.
pub fn build_llm(config: &Config) -> Result<Box<dyn Llm>> {
    Ok(Box::new(WithModel {
        inner: build_provider(config)?,
        model: config.model.clone(),
        summary_language: config.summary_language.clone(),
    }))
}

fn build_provider(config: &Config) -> Result<Box<dyn Llm>> {
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
    let gen = Generation {
        effort: config.effort,
        effort_explicit: config.effort_explicit,
        thinking: config.thinking,
        max_output_tokens: config.max_output_tokens,
    };
    let structured = structured(config);
    match config.provider {
        Provider::Mock => Ok(Box::new(mock::MockLlm::from_env()?)),
        Provider::Anthropic => {
            let retry = Retry::from_env()?;
            let key = config.api_key()?.ok_or_else(|| {
                Error::Message("clé api_key_env manquante pour le fournisseur anthropic".into())
            })?;
            Ok(Box::new(
                anthropic::AnthropicLlm::new(
                    config.base_url.clone(),
                    config.model.clone(),
                    key,
                    config.timeout_secs,
                    &tls,
                    config.concurrency,
                    gen,
                )
                .with_structured(structured)
                .with_structured_key(config.structured_key)
                .with_retry(retry)
                .with_summary_language(config.summary_language.clone()),
            ))
        }
        Provider::OpenAi => {
            let retry = Retry::from_env()?;
            Ok(Box::new(
                openai::OpenAiLlm::new(
                    config.base_url.clone(),
                    config.model.clone(),
                    config.api_key()?,
                    config.timeout_secs,
                    &tls,
                    config.concurrency,
                    gen,
                )
                .with_structured(structured)
                .with_structured_key(config.structured_key)
                .with_retry(retry)
                .with_summary_language(config.summary_language.clone()),
            ))
        }
    }
}

#[cfg(test)]
mod structured_tests {
    use super::*;
    use std::path::Path;

    fn config(structured: &str, base_url: bool, provider: &str) -> Config {
        let url = if base_url {
            "base_url = \"http://x\"\n"
        } else {
            ""
        };
        Config::parse(
            &format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"K\"\n{url}\
                 structured_output = \"{structured}\"\n"
            ),
            Path::new("/x/config.toml"),
        )
        .unwrap()
    }

    #[test]
    fn req202_ac2_valeur_effective() {
        for provider in ["openai", "anthropic", "mock"] {
            let eff = |s: &str, url: bool| structured(&config(s, url, provider));
            assert_eq!(eff("auto", false), Structured::Auto, "{provider}");
            assert_eq!(eff("auto", true), Structured::Off, "{provider}");
            assert_eq!(eff("on", false), Structured::On, "{provider}");
            assert_eq!(eff("on", true), Structured::On, "{provider}");
            assert_eq!(eff("off", false), Structured::Off, "{provider}");
            assert_eq!(eff("off", true), Structured::Off, "{provider}");
        }
    }
}
