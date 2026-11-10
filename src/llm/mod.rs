//! Abstraction du fournisseur LLM (REQ-002, REQ-003, REQ-006).

pub mod anthropic;
pub mod ask;
pub mod batch;
pub mod compact;
pub use batch::BatchLink;
pub mod enrich;
pub(crate) mod http;
pub use http::{Retry, Tls};
pub mod mock;
pub mod openai;
pub mod redact;
pub mod retry;
mod schema;
mod sse;
pub mod subsystem;
mod tidy;
mod usage;
#[cfg(test)]
pub(crate) use usage::recording_server;
pub use usage::{
    anthropic_usage, calls_line, openai_usage, served_model, tokens_segment, CallStats, LlmTotals,
    Usage,
};

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::config::{Config, Effort, Provider, StructuredOutput};
use crate::error::{Error, Result};
pub use ask::AskReply;
use ask::{ask_system_prompt, ask_user_prompt};
use enrich::{
    cacheable_prefix_len, enrichment_system_prompt, enrichment_user_prompt, lot_system_prompt,
    lot_user_prompt, parse_lot,
};
pub use enrich::{Enrichment, Excerpt, LotItem};
pub use retry::{with_format_retries, with_note, FormatFault};
use schema::Reply;
pub use subsystem::SubsystemName;
use subsystem::{subsystem_system_prompt, subsystem_user_prompt};

/// Variable de test qui remplace le sondage d'un lot du fournisseur, une fois par minute
/// (millisecondes).
pub const BATCH_POLL_ENV: &str = "NEPHILA_BATCH_POLL_MS";

/// Délai entre deux sondages d'un lot du fournisseur (REQ-212) : 60 s, ou `NEPHILA_BATCH_POLL_MS`
/// (absente ou vide : 60 s ; mal formée : erreur).
pub fn poll_interval() -> Result<std::time::Duration> {
    poll_value(std::env::var(BATCH_POLL_ENV).ok().as_deref())
}

fn poll_value(value: Option<&str>) -> Result<std::time::Duration> {
    match value.filter(|v| !v.is_empty()) {
        None => Ok(std::time::Duration::from_secs(60)),
        Some(v) => v
            .trim()
            .parse()
            .map(std::time::Duration::from_millis)
            .map_err(|_| Error::bad_integer_env(BATCH_POLL_ENV, v)),
    }
}

/// Options de génération communes aux fournisseurs HTTP (REQ-058, DEC-072).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Generation {
    pub effort: Effort,
    /// La clé `effort` est écrite dans la configuration (REQ-157).
    pub effort_explicit: bool,
    pub thinking: bool,
    pub max_output_tokens: u32,
    /// Température envoyée sauf avec `thinking` (REQ-275).
    pub temperature: Option<f64>,
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
    /// Mode lot du fournisseur (DEC-168) : `None` quand il n'en a pas ou que le collecteur manque.
    fn batch(&self) -> Option<BatchLink<'_>> {
        None
    }
    /// Modèle effectif, qui entre dans l'empreinte d'enrichissement (REQ-049). Vide par défaut.
    fn model(&self) -> &str {
        ""
    }
    /// Langue des résumés demandée, enregistrée dans l'empreinte d'enrichissement (REQ-173).
    fn summary_language(&self) -> &str {
        crate::config::DEFAULT_SUMMARY_LANGUAGE
    }
    /// Minimum de jetons au-delà duquel le contenu d'un morceau relu porte `cache_control`
    /// (REQ-268) ; `None` : jamais de marqueur.
    fn cache_floor(&self) -> Option<usize> {
        None
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
        cached: bool,
    ) -> Result<Enrichment> {
        let _ = (label, excerpt, stop, stats, fault, cached);
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
    /// Nom et description d'une communauté d'après ses lignes `- <identifiant> : <résumé>`
    /// (REQ-261). `label` nomme l'appel pour les traces d'attente. Par défaut, une erreur.
    fn name_subsystem(
        &self,
        label: &str,
        lines: &[String],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<SubsystemName> {
        let _ = (label, lines, stop, stats, fault);
        Err(Error::Message(
            "ce fournisseur LLM ne nomme pas de sous-système".into(),
        ))
    }
}

/// Appel de complétion d'un adaptateur HTTP : le reste de `Llm` s'en déduit (REQ-127).
pub(crate) trait Complete: Send + Sync {
    /// Mode lot de l'adaptateur (voir `Llm::batch`).
    fn batch(&self) -> Option<BatchLink<'_>> {
        None
    }
    /// Langue des résumés demandée dans les prompts système (REQ-172).
    fn summary_language(&self) -> &str;
    /// Voir `Llm::cache_floor`.
    fn cache_floor(&self) -> Option<usize> {
        None
    }
    /// Envoie la consigne système et la requête, retourne le texte de la réponse.
    #[allow(clippy::too_many_arguments)]
    fn complete(
        &self,
        label: Option<&str>,
        stop: Option<&AtomicBool>,
        stats: &CallStats,
        system: String,
        user: String,
        reply: Reply,
        cache_upto: Option<usize>,
    ) -> Result<String>;
}

/// Clé de premier niveau d'une réponse d'enrichissement d'un fichier seul.
const KEY_SUMMARIES: &str = "summaries";
/// Clé de premier niveau d'une réponse d'enrichissement d'un lot.
const KEY_FILES: &str = "files";
/// Clé de premier niveau d'une réponse de `query ask`.
const KEY_FOUND: &str = "found";
/// Clé de premier niveau d'une réponse de la passe des sous-systèmes.
const KEY_NAME: &str = "name";

impl<T: Complete> Llm for T {
    fn batch(&self) -> Option<BatchLink<'_>> {
        Complete::batch(self)
    }

    fn summary_language(&self) -> &str {
        Complete::summary_language(self)
    }

    fn cache_floor(&self) -> Option<usize> {
        Complete::cache_floor(self)
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
            None,
        )?;
        http::parse_reply(&out, KEY_SUMMARIES, |j| {
            Enrichment::parse_numbered(j, file, requested, known)
        })
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
        cached: bool,
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
            cached.then(|| cacheable_prefix_len(file, text)),
        )?;
        http::parse_reply(&out, KEY_SUMMARIES, |j| {
            Enrichment::parse_numbered(j, file, requested, known)
        })
    }

    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
        let out = self.complete(
            None,
            None,
            &CallStats::default(),
            lot_system_prompt(self.summary_language()),
            lot_user_prompt(items),
            Reply::Lot,
            None,
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
            None,
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
            None,
        )?;
        http::parse_reply(&out, KEY_FOUND, AskReply::parse)
    }

    fn name_subsystem(
        &self,
        label: &str,
        lines: &[String],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<SubsystemName> {
        let out = self.complete(
            Some(label),
            Some(stop),
            stats,
            subsystem_system_prompt(self.summary_language()),
            with_note(subsystem_user_prompt(lines), fault),
            Reply::Subsystem,
            None,
        )?;
        http::parse_reply(&out, KEY_NAME, SubsystemName::parse).map_err(subsystem::fixed_label)
    }
}

/// Fournisseur qui porte le modèle effectif de la configuration (le simulé n'en a pas).
struct WithModel {
    inner: Box<dyn Llm>,
    model: String,
    summary_language: String,
}

impl Llm for WithModel {
    fn batch(&self) -> Option<BatchLink<'_>> {
        self.inner.batch()
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn summary_language(&self) -> &str {
        &self.summary_language
    }

    fn cache_floor(&self) -> Option<usize> {
        self.inner.cache_floor()
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
        cached: bool,
    ) -> Result<Enrichment> {
        self.inner.enrich_labeled(
            label, file, call, excerpt, text, requested, known, stop, stats, fault, cached,
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

    fn name_subsystem(
        &self,
        label: &str,
        lines: &[String],
        stop: &AtomicBool,
        stats: &CallStats,
        fault: Option<FormatFault>,
    ) -> Result<SubsystemName> {
        self.inner.name_subsystem(label, lines, stop, stats, fault)
    }
}

/// Construit le fournisseur décrit par la configuration.
pub fn build_llm(config: &Config) -> Result<Box<dyn Llm>> {
    build_llm_for(config, false)
}

/// Comme `build_llm`; avec `batch` (`--batch`, REQ-211), les adaptateurs HTTP reçoivent le
/// collecteur de requêtes du mode lot.
pub fn build_llm_for(config: &Config, batch: bool) -> Result<Box<dyn Llm>> {
    Ok(Box::new(WithModel {
        inner: build_provider(config, batch)?,
        model: config.model.clone(),
        summary_language: config.summary_language.clone(),
    }))
}

fn build_provider(config: &Config, batch: bool) -> Result<Box<dyn Llm>> {
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
        temperature: config.temperature,
    };
    let structured = structured(config);
    match config.provider {
        Provider::Mock => Ok(Box::new(mock::MockLlm::from_env()?)),
        Provider::Anthropic => {
            let retry = Retry::from_env()?;
            let key = config.api_key()?.ok_or_else(|| {
                Error::Message("clé api_key_env manquante pour le fournisseur anthropic".into())
            })?;
            let llm = anthropic::AnthropicLlm::new(
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
            .with_summary_language(config.summary_language.clone());
            let llm = if batch {
                llm.with_collector(Arc::new(batch::Collector::new()))
            } else {
                llm
            };
            Ok(Box::new(llm))
        }
        Provider::OpenAi => {
            let retry = Retry::from_env()?;
            let llm = openai::OpenAiLlm::new(
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
            .with_summary_language(config.summary_language.clone());
            let llm = if batch {
                llm.with_collector(Arc::new(batch::Collector::new()))
            } else {
                llm
            };
            Ok(Box::new(llm))
        }
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn req212_ac2_poll_interval_values() {
        assert_eq!(poll_value(None).unwrap(), Duration::from_secs(60));
        assert_eq!(poll_value(Some("")).unwrap(), Duration::from_secs(60));
        assert_eq!(poll_value(Some("25")).unwrap(), Duration::from_millis(25));
        assert_eq!(poll_value(Some(" 7 ")).unwrap(), Duration::from_millis(7));
        let e = poll_value(Some("vite")).unwrap_err();
        assert_eq!(
            e.to_string(),
            "NEPHILA_BATCH_POLL_MS : entier attendu, reçu « vite »"
        );
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

#[cfg(test)]
mod with_model_tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        seen: Arc<Mutex<Vec<bool>>>,
    }
    impl Llm for Fake {
        fn cache_floor(&self) -> Option<usize> {
            Some(42)
        }
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            Err(Error::Message("inutilisé".into()))
        }
        fn enrich_labeled(
            &self,
            _: &str,
            _: &str,
            _: usize,
            _: Option<Excerpt>,
            _: &str,
            _: &[String],
            _: &[String],
            _: &AtomicBool,
            _: &CallStats,
            _: Option<FormatFault>,
            cached: bool,
        ) -> Result<Enrichment> {
            self.seen.lock().unwrap().push(cached);
            Ok(Enrichment::default())
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req268_ac4_with_model_relays_cache_floor_and_cached() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let wrapped = WithModel {
            inner: Box::new(Fake { seen: seen.clone() }),
            model: "m".into(),
            summary_language: "français".into(),
        };
        assert_eq!(wrapped.cache_floor(), Some(42));
        for cached in [true, false] {
            wrapped
                .enrich_labeled(
                    "l",
                    "A.java",
                    1,
                    None,
                    "t",
                    &[],
                    &[],
                    &AtomicBool::new(false),
                    &CallStats::default(),
                    None,
                    cached,
                )
                .unwrap();
        }
        assert_eq!(*seen.lock().unwrap(), [true, false]);
    }
}
