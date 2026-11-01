//! Configuration du fournisseur LLM (REQ-002).

use std::path::{Path, PathBuf};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use serde::Deserialize;

use crate::error::{Error, Result};

pub const DEFAULT_MAX_CHUNK_CHARS: usize = 60_000;
pub const DEFAULT_CONCURRENCY: usize = 4;
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Langue par défaut des résumés (`summary_language`, REQ-172).
pub const DEFAULT_SUMMARY_LANGUAGE: &str = "français";
/// Longueur maximale, en caractères, de `summary_language` (REQ-172).
pub const SUMMARY_LANGUAGE_MAX_CHARS: usize = 40;
pub const DEFAULT_BATCH_FILES: usize = 10;
pub const DEFAULT_FORMAT_RETRIES: u32 = 2;

/// Réglages que le fichier de projet peut porter, dans l'ordre d'affichage (REQ-042). Seule
/// déclaration de la liste : l'aide, l'erreur de clé inconnue et la trace s'en servent.
pub const PROJECT_SETTINGS: [&str; 7] = [
    "model",
    "summary_language",
    "max_chunk_chars",
    "concurrency",
    "timeout_secs",
    "batch_files",
    "format_retries",
];

/// Règle de `summary_language`, source des deux messages d'erreur (V-006), sans article.
fn summary_language_expected() -> String {
    format!(
        "nom de langue de 1 à {SUMMARY_LANGUAGE_MAX_CHARS} lettres, espaces, traits d'union ou \
         apostrophes"
    )
}

/// Valide une langue des résumés : après `trim`, 1 à 40 caractères, chacun alphabétique, espace,
/// `-` ou `'` (REQ-172). Rend la valeur retenue.
pub fn summary_language_of(raw: &str) -> Option<String> {
    let v = raw.trim();
    let n = v.chars().count();
    let ok = (1..=SUMMARY_LANGUAGE_MAX_CHARS).contains(&n)
        && v.chars()
            .all(|c| c.is_alphabetic() || c == ' ' || c == '-' || c == '\'');
    ok.then(|| v.to_string())
}

/// Plafonds inclus des réglages entiers du fichier de projet seulement (DEC-055), dans l'ordre
/// de `PROJECT_SETTINGS`, `model` exclu. Seule source du message d'erreur, de l'exemple et du
/// README ; la configuration personnelle et les options n'en ont pas.
pub const PROJECT_CAPS: [(&str, u64); 5] = [
    ("max_chunk_chars", 1_000_000),
    ("concurrency", 64),
    ("timeout_secs", 3600),
    ("batch_files", 100),
    ("format_retries", 10),
];

/// Plafond de `key` dans le fichier de projet, s'il en a un.
pub fn project_cap(key: &str) -> Option<u64> {
    PROJECT_CAPS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, c)| *c)
}

/// Clés de la section `[ask]`, dans l'ordre de REQ-162. Seule déclaration de la liste : l'aide,
/// l'erreur de clé inconnue et les tests s'en servent.
pub const ASK_SETTINGS: [&str; 7] = [
    "model",
    "thinking",
    "effort",
    "max_output_tokens",
    "max_context_chars",
    "max_seeds",
    "max_nodes",
];

/// Plafonds inclus des entiers de `[ask]` dans le fichier de projet seulement (DEC-132). Seule
/// source du message d'erreur, de l'exemple et du README ; la configuration personnelle et
/// `--context-chars` n'en ont pas.
pub const ASK_CAPS: [(&str, u64); 4] = [
    ("max_output_tokens", 16384),
    ("max_context_chars", 100_000),
    ("max_seeds", 100),
    ("max_nodes", 500),
];

/// Plafond de `key` dans la section `[ask]` du fichier de projet, s'il en a un.
pub fn ask_cap(key: &str) -> Option<u64> {
    ASK_CAPS.iter().find(|(k, _)| *k == key).map(|(_, c)| *c)
}

/// Nombre maximal de nœuds trouvés par les mots de la question, par défaut.
pub const DEFAULT_ASK_MAX_SEEDS: usize = 20;
/// Nombre maximal de nœuds (trouvés + voisins) décrits dans le contexte, par défaut.
pub const DEFAULT_ASK_MAX_NODES: usize = 60;
/// Taille maximale du contexte, en caractères, par défaut.
pub const DEFAULT_ASK_MAX_CONTEXT_CHARS: usize = 12_000;
/// Taille minimale admise pour le contexte, en caractères.
pub const ASK_MIN_CONTEXT_CHARS: usize = 1000;

/// Bornes du contexte de `query ask` (REQ-163).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AskLimits {
    pub max_context_chars: usize,
    pub max_seeds: usize,
    pub max_nodes: usize,
}

impl Default for AskLimits {
    fn default() -> Self {
        AskLimits {
            max_context_chars: DEFAULT_ASK_MAX_CONTEXT_CHARS,
            max_seeds: DEFAULT_ASK_MAX_SEEDS,
            max_nodes: DEFAULT_ASK_MAX_NODES,
        }
    }
}

/// Valeurs de la section `[ask]` d'un fichier, telles qu'écrites (REQ-162, REQ-163).
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct AskOverrides {
    pub model: Option<String>,
    pub thinking: Option<bool>,
    pub effort: Option<Effort>,
    pub max_output_tokens: Option<u32>,
    pub max_context_chars: Option<usize>,
    pub max_seeds: Option<usize>,
    pub max_nodes: Option<usize>,
}

/// Clés de connexion : permises dans la configuration personnelle seulement (DEC-052).
pub const CONNECTION_KEYS: [&str; 5] = [
    "provider",
    "base_url",
    "api_key_env",
    "tls_ca_cert",
    "tls_insecure",
];

/// Réglages LLM lus dans le fichier de projet `.nephila.toml` (REQ-042).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    pub model: Option<String>,
    pub summary_language: Option<String>,
    pub max_chunk_chars: Option<usize>,
    pub concurrency: Option<usize>,
    pub timeout_secs: Option<u64>,
    pub batch_files: Option<usize>,
    pub format_retries: Option<u32>,
    /// Section `[ask]` (REQ-162).
    pub ask: AskOverrides,
}

impl ProjectSettings {
    /// Lit et valide les réglages présents dans `table` ; `shown` = chemin affiché du fichier.
    pub fn from_table(table: &toml::Table, shown: &str) -> Result<ProjectSettings> {
        let mut out = ProjectSettings::default();
        for key in PROJECT_SETTINGS {
            let Some(value) = table.get(key) else {
                continue;
            };
            let invalid = |expected: &str| {
                Error::Message(format!(
                    "{shown} : clé `{key}` : valeur invalide `{value}` ({expected})"
                ))
            };
            // Un seul `<attendu>` par clé, quelle que soit la faute (DEC-055).
            let bounded = |min: i64, cap: u64, expected: &str| -> Result<u64> {
                match value {
                    toml::Value::Integer(n) if *n >= min && (*n as u64) <= cap => Ok(*n as u64),
                    _ => Err(invalid(&format!("{expected}, au plus {cap}"))),
                }
            };
            let positive = || -> Result<u64> {
                let cap = project_cap(key)
                    .unwrap_or_else(|| unreachable!("réglage entier sans plafond : {key}"));
                bounded(1, cap, "entier strictement positif attendu")
            };
            match key {
                "model" => match value {
                    toml::Value::String(s) if !s.trim().is_empty() => out.model = Some(s.clone()),
                    _ => return Err(invalid("chaîne non vide attendue")),
                },
                "summary_language" => match value {
                    toml::Value::String(s) if summary_language_of(s).is_some() => {
                        out.summary_language = summary_language_of(s)
                    }
                    _ => {
                        return Err(Error::Message(format!(
                            "{shown} : clé `{key}` : valeur invalide `{}` ({} attendu)",
                            crate::escape::path(&value.to_string()),
                            summary_language_expected()
                        )))
                    }
                },
                "max_chunk_chars" => out.max_chunk_chars = Some(positive()? as usize),
                "concurrency" => out.concurrency = Some(positive()? as usize),
                "timeout_secs" => out.timeout_secs = Some(positive()?),
                "batch_files" => out.batch_files = Some(positive()? as usize),
                "format_retries" => {
                    let cap = project_cap(key)
                        .unwrap_or_else(|| unreachable!("réglage entier sans plafond : {key}"));
                    out.format_retries =
                        Some(bounded(0, cap, "entier positif ou nul attendu")? as u32)
                }
                _ => unreachable!("réglage hors de PROJECT_SETTINGS : {key}"),
            }
        }
        if let Some(value) = table.get("ask") {
            out.ask = ask_from_value(value, shown)?;
        }
        Ok(out)
    }
}

/// Lit la section `[ask]` du fichier de projet, clé par clé dans l'ordre d'`ASK_SETTINGS`.
fn ask_from_value(value: &toml::Value, shown: &str) -> Result<AskOverrides> {
    let toml::Value::Table(t) = value else {
        return Err(Error::Message(format!(
            "{shown} : clé `ask` : valeur invalide `{value}` (table [ask] attendue)"
        )));
    };
    // Toute clé hors de la section, clés de connexion comprises, donne la même erreur : la
    // valeur n'est jamais reprise.
    if let Some(unknown) = t.keys().find(|k| !ASK_SETTINGS.contains(&k.as_str())) {
        return Err(Error::Message(format!(
            "{shown} : clé inconnue `ask.{unknown}` (clés permises dans [ask] : {})",
            ASK_SETTINGS.join(", ")
        )));
    }
    let mut out = AskOverrides::default();
    for key in ASK_SETTINGS {
        let Some(v) = t.get(key) else {
            continue;
        };
        let invalid = |expected: &str| {
            Error::Message(format!(
                "{shown} : clé `ask.{key}` : valeur invalide `{v}` ({expected})"
            ))
        };
        let capped = |min: i64, expected: &str| -> Result<u64> {
            let cap = ask_cap(key).unwrap_or_else(|| unreachable!("entier sans plafond : {key}"));
            match v {
                toml::Value::Integer(n) if *n >= min && (*n as u64) <= cap => Ok(*n as u64),
                _ => Err(invalid(&format!("{expected}, au plus {cap}"))),
            }
        };
        match key {
            "model" => match v {
                toml::Value::String(s) if !s.trim().is_empty() => out.model = Some(s.clone()),
                _ => return Err(invalid("chaîne non vide attendue")),
            },
            "thinking" => match v {
                toml::Value::Boolean(b) => out.thinking = Some(*b),
                _ => return Err(invalid("booléen attendu, true ou false")),
            },
            "effort" => match v.as_str().and_then(Effort::parse) {
                Some(e) => out.effort = Some(e),
                None => {
                    return Err(invalid(
                        "\"low\", \"medium\", \"high\" ou \"default\" attendu",
                    ))
                }
            },
            "max_output_tokens" => {
                out.max_output_tokens =
                    Some(capped(1, "entier strictement positif attendu")? as u32)
            }
            "max_context_chars" => {
                out.max_context_chars = Some(capped(
                    ASK_MIN_CONTEXT_CHARS as i64,
                    &format!("entier d'au moins {ASK_MIN_CONTEXT_CHARS} attendu"),
                )? as usize)
            }
            "max_seeds" => {
                out.max_seeds = Some(capped(1, "entier strictement positif attendu")? as usize)
            }
            "max_nodes" => {
                out.max_nodes = Some(capped(1, "entier strictement positif attendu")? as usize)
            }
            _ => unreachable!("réglage hors de ASK_SETTINGS : {key}"),
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Mock,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::Mock => "mock",
        }
    }
}

/// Niveau de la réflexion du modèle (REQ-058).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    /// Aucun niveau envoyé : le modèle garde le sien (REQ-157).
    ModelDefault,
}

/// Politique de la configuration personnelle envers le modèle imposé par `.nephila.toml`
/// (REQ-195) : `allow` l'accepte, `warn` (défaut) aussi mais avertit (REQ-196), `deny` l'ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelOverride {
    Allow,
    Warn,
    Deny,
}

impl ModelOverride {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelOverride::Allow => "allow",
            ModelOverride::Warn => "warn",
            ModelOverride::Deny => "deny",
        }
    }
}

pub const DEFAULT_EFFORT: Effort = Effort::Low;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16384;

impl Effort {
    /// Niveau écrit dans un fichier : `low`, `medium`, `high` ou `default`.
    pub fn parse(s: &str) -> Option<Effort> {
        match s {
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "default" => Some(Effort::ModelDefault),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::ModelDefault => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub provider: Provider,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub max_chunk_chars: usize,
    pub concurrency: usize,
    /// Durée maximale d'un appel LLM HTTP, en secondes.
    pub timeout_secs: u64,
    /// Nombre maximal de fichiers par appel d'enrichissement (REQ-027).
    pub batch_files: usize,
    /// Nouveaux essais d'un appel dont la réponse est non conforme (REQ-038).
    pub format_retries: u32,
    /// Plafond d'appels d'enrichissement par commande, configuration personnelle seulement
    /// (REQ-186) ; `None` = aucun plafond.
    pub max_calls: Option<usize>,
    /// Enrichit aussi les fichiers de code, pas seulement les documents (REQ-050).
    pub enrich_code: bool,
    /// Demande la réflexion au modèle (REQ-058).
    pub thinking: bool,
    /// Niveau d'effort demandé au modèle, avec ou sans `thinking` (REQ-058, REQ-157).
    pub effort: Effort,
    /// Vrai si la clé `effort` est écrite, quelle que soit sa valeur (REQ-157).
    pub effort_explicit: bool,
    /// Limite de jetons de la réponse, réflexion comprise (REQ-058).
    pub max_output_tokens: u32,
    /// Chemin résolu du PEM d'autorités ajoutées aux autorités publiques (REQ-036).
    pub tls_ca_cert: Option<PathBuf>,
    /// Certificats d'autorité (DER) lus dans `tls_ca_cert`, déjà validés.
    pub tls_ca_certs: Vec<CertificateDer<'static>>,
    /// Désactive le contrôle du certificat du serveur (REQ-036).
    pub tls_insecure: bool,
    /// Langue retenue des résumés et des noms de concepts (REQ-172).
    pub summary_language: String,
    /// Politique envers le modèle imposé par `.nephila.toml` (REQ-195).
    pub model_override: ModelOverride,
    /// Section `[ask]` de la configuration personnelle (REQ-162).
    pub ask: AskOverrides,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    provider: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
    max_chunk_chars: Option<i64>,
    concurrency: Option<i64>,
    timeout_secs: Option<i64>,
    batch_files: Option<i64>,
    format_retries: Option<toml::Value>,
    max_calls: Option<toml::Value>,
    enrich_code: Option<toml::Value>,
    summary_language: Option<toml::Value>,
    thinking: Option<toml::Value>,
    effort: Option<toml::Value>,
    max_output_tokens: Option<toml::Value>,
    tls_ca_cert: Option<String>,
    tls_insecure: Option<bool>,
    model_override: Option<toml::Value>,
    ask: Option<toml::Value>,
}

/// Section `[ask]` de la configuration personnelle : une clé inconnue garde le message du
/// lecteur TOML (REQ-162) ; les valeurs sont validées à la main.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsk {
    model: Option<toml::Value>,
    thinking: Option<toml::Value>,
    effort: Option<toml::Value>,
    max_output_tokens: Option<toml::Value>,
    max_context_chars: Option<toml::Value>,
    max_seeds: Option<toml::Value>,
    max_nodes: Option<toml::Value>,
}

/// Chemin du fichier de configuration : `NEPHILA_CONFIG`, sinon
/// `$XDG_CONFIG_HOME/nephila/config.toml`, sinon `~/.config/nephila/config.toml`.
pub fn config_path_with(env: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf> {
    let non_empty = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(p) = non_empty("NEPHILA_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    if let Some(x) = non_empty("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(x).join("nephila").join("config.toml"));
    }
    if let Some(h) = non_empty("HOME") {
        return Ok(PathBuf::from(h)
            .join(".config")
            .join("nephila")
            .join("config.toml"));
    }
    Err(Error::Message(
        "impossible de déterminer le fichier de configuration : définissez NEPHILA_CONFIG, \
         XDG_CONFIG_HOME ou HOME"
            .into(),
    ))
}

pub fn config_path() -> Result<PathBuf> {
    config_path_with(&|k| std::env::var(k).ok())
}

impl Config {
    /// Applique les réglages de projet puis les options (REQ-042). Rend la configuration
    /// effective et les réglages du fichier de projet qu'aucune option n'a remplacés, dans
    /// l'ordre de `PROJECT_SETTINGS`. Les clés de connexion ne sont jamais touchées.
    pub fn with_project(
        mut self,
        p: &ProjectSettings,
        batch_files: Option<usize>,
        format_retries: Option<u32>,
    ) -> (Config, Vec<&'static str>) {
        let mut from_project = Vec::new();
        if let (Some(v), true) = (&p.model, self.model_override != ModelOverride::Deny) {
            self.model = v.clone();
            from_project.push("model");
        }
        if let Some(v) = &p.summary_language {
            self.summary_language = v.clone();
            from_project.push("summary_language");
        }
        if let Some(v) = p.max_chunk_chars {
            self.max_chunk_chars = v;
            from_project.push("max_chunk_chars");
        }
        if let Some(v) = p.concurrency {
            self.concurrency = v;
            from_project.push("concurrency");
        }
        if let Some(v) = p.timeout_secs {
            self.timeout_secs = v;
            from_project.push("timeout_secs");
        }
        if let Some(v) = p.batch_files {
            self.batch_files = v;
            from_project.push("batch_files");
        }
        if let Some(v) = p.format_retries {
            self.format_retries = v;
            from_project.push("format_retries");
        }
        if let Some(n) = batch_files {
            self.batch_files = n;
            from_project.retain(|k| *k != "batch_files");
        }
        if let Some(n) = format_retries {
            self.format_retries = n;
            from_project.retain(|k| *k != "format_retries");
        }
        (self, from_project)
    }

    /// Segments TLS de la ligne `config` (REQ-036), chacun précédé d'une espace.
    pub fn tls_trace(&self) -> String {
        let mut s = String::new();
        if let Some(p) = &self.tls_ca_cert {
            s.push_str(&format!(" tls_ca_cert={}", p.display()));
        }
        if self.tls_insecure {
            s.push_str(" tls_insecure");
        }
        s
    }

    /// Configuration, bornes du contexte et marque de source de `query ask` (REQ-162, DEC-132) :
    /// pour chaque clé de `[ask]`, la première valeur présente parmi l'option, `[ask]` du fichier
    /// de projet, `[ask]` personnel, la valeur effective de l'enrichissement, le défaut propre.
    /// Rend les clés du fichier de projet qui donnent une valeur de la ligne `config`, dans
    /// l'ordre de ses champs.
    pub fn for_ask(
        self,
        project: &ProjectSettings,
        format_retries: Option<u32>,
        context_chars: Option<usize>,
    ) -> (Config, AskLimits, Vec<&'static str>) {
        let personal = self.ask.clone();
        let (mut cfg, from_project) = self.with_project(project, None, format_retries);
        let pa = &project.ask;
        let deny = cfg.model_override == ModelOverride::Deny;
        let project_ask_model = if deny { None } else { pa.model.clone() };
        let mut marks: Vec<&'static str> = Vec::new();
        if project_ask_model.is_some() {
            marks.push("ask.model");
        } else if from_project.contains(&"model") && personal.model.is_none() {
            marks.push("model");
        }
        if let Some(v) = project_ask_model.or(personal.model.clone()) {
            cfg.model = v;
        }
        if let Some(v) = pa.thinking.or(personal.thinking) {
            cfg.thinking = v;
        }
        if pa.thinking.is_some() {
            marks.push("ask.thinking");
        }
        if let Some(v) = pa.effort.or(personal.effort) {
            cfg.effort = v;
            cfg.effort_explicit = true;
        }
        if pa.effort.is_some() {
            marks.push("ask.effort");
        }
        if let Some(v) = pa.max_output_tokens.or(personal.max_output_tokens) {
            cfg.max_output_tokens = v;
        }
        if pa.max_output_tokens.is_some() {
            marks.push("ask.max_output_tokens");
        }
        let limits = AskLimits {
            max_context_chars: context_chars
                .or(pa.max_context_chars)
                .or(personal.max_context_chars)
                .unwrap_or(DEFAULT_ASK_MAX_CONTEXT_CHARS),
            max_seeds: pa
                .max_seeds
                .or(personal.max_seeds)
                .unwrap_or(DEFAULT_ASK_MAX_SEEDS),
            max_nodes: pa
                .max_nodes
                .or(personal.max_nodes)
                .unwrap_or(DEFAULT_ASK_MAX_NODES),
        };
        if pa.max_context_chars.is_some() && context_chars.is_none() {
            marks.push("ask.max_context_chars");
        }
        if pa.max_seeds.is_some() {
            marks.push("ask.max_seeds");
        }
        if pa.max_nodes.is_some() {
            marks.push("ask.max_nodes");
        }
        if from_project.contains(&"timeout_secs") {
            marks.push("timeout_secs");
        }
        if from_project.contains(&"format_retries") {
            marks.push("format_retries");
        }
        (cfg, limits, marks)
    }

    /// Charge et valide la configuration depuis l'environnement du processus.
    pub fn load() -> Result<Config> {
        Config::load_with(&|k| std::env::var(k).ok())
    }

    /// Comme `load`, avec un environnement fourni.
    pub fn load_with(env: &dyn Fn(&str) -> Option<String>) -> Result<Config> {
        let path = config_path_with(env)?;
        let cfg = Config::load_from_with(&path, env)?;
        cfg.api_key_with(env)?;
        Ok(cfg)
    }

    /// Comme `load_with`, mais `None` quand aucun fichier de configuration n'existe : le LLM est
    /// alors inactif (REQ-002). Un fichier présent mais invalide reste une erreur.
    pub fn load_optional_with(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<Config>> {
        let cfg = Config::load_optional_without_key_with(env)?;
        if let Some(c) = &cfg {
            c.api_key_with(env)?;
        }
        Ok(cfg)
    }

    /// Comme `load_optional_with`, sans lire la clé d'API : même lecture et même validation
    /// (`api_key_env` exigé pour `anthropic`), la variable nommée n'est pas consultée (REQ-184).
    pub fn load_optional_without_key_with(
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<Config>> {
        let Ok(path) = config_path_with(env) else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(Config::load_from_with(&path, env)?))
    }

    /// `load_optional_without_key_with` sur l'environnement du processus.
    pub fn load_optional_without_key() -> Result<Option<Config>> {
        Config::load_optional_without_key_with(&|k| std::env::var(k).ok())
    }

    /// `load_optional_with` sur l'environnement du processus.
    pub fn load_optional() -> Result<Option<Config>> {
        Config::load_optional_with(&|k| std::env::var(k).ok())
    }

    /// Lit et valide un fichier de configuration (sans lire la clé d'API).
    pub fn load_from(path: &Path) -> Result<Config> {
        Config::load_from_with(path, &|k| std::env::var(k).ok())
    }

    /// Comme `load_from`, avec un environnement fourni (`HOME` pour `~/`).
    pub fn load_from_with(path: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Message(format!(
                    "configuration LLM introuvable : {} ; créez ce fichier TOML avec les clés \
                     provider (anthropic | openai | mock), model et api_key_env",
                    path.display()
                )))
            }
            Err(e) => {
                return Err(Error::Message(format!(
                    "lecture de la configuration {} impossible : {e}",
                    path.display()
                )))
            }
        };
        Config::parse_with(&text, path, env)
    }

    /// Analyse le contenu TOML ; `path` désigne le fichier de configuration (messages et
    /// base des chemins relatifs). `HOME` est celui du processus.
    pub fn parse(text: &str, path: &Path) -> Result<Config> {
        Config::parse_with(text, path, &|k| std::env::var(k).ok())
    }

    /// Comme `parse`, avec un environnement fourni.
    pub fn parse_with(
        text: &str,
        path: &Path,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Config> {
        let at = path.display();
        let raw: RawConfig = toml::from_str(text)
            .map_err(|e| Error::Message(format!("configuration invalide ({at}) : {e}")))?;
        let missing = |key: &str| {
            Error::Message(format!(
                "configuration invalide ({at}) : clé « {key} » manquante"
            ))
        };
        let nonblank = |key: &str, v: Option<String>| -> Result<Option<String>> {
            match v {
                Some(s) if s.trim().is_empty() => Err(Error::Message(format!(
                    "configuration invalide ({at}) : clé « {key} » vide"
                ))),
                other => Ok(other),
            }
        };
        let provider = match raw.provider.as_deref() {
            None => return Err(missing("provider")),
            Some("anthropic") => Provider::Anthropic,
            Some("openai") => Provider::OpenAi,
            Some("mock") => Provider::Mock,
            Some(other) => {
                return Err(Error::Message(format!(
                    "configuration invalide ({at}) : clé « provider » inconnue : « {other} » \
                     (attendu : anthropic, openai ou mock)"
                )))
            }
        };
        let model = nonblank("model", raw.model)?;
        let model = match (model, provider) {
            (Some(m), _) => m,
            (None, Provider::Mock) => "mock".to_string(),
            (None, _) => return Err(missing("model")),
        };
        let api_key_env = nonblank("api_key_env", raw.api_key_env)?;
        if provider == Provider::Anthropic && api_key_env.is_none() {
            return Err(missing("api_key_env"));
        }
        let base_url = nonblank("base_url", raw.base_url)?;
        let tls_ca_cert = nonblank("tls_ca_cert", raw.tls_ca_cert)?;
        let (tls_ca_cert, tls_ca_certs) = match tls_ca_cert {
            None => (None, Vec::new()),
            Some(raw_path) => {
                let resolved = resolve_ca_path(&raw_path, path, env, &at.to_string())?;
                let certs = read_ca_certs(&resolved)?;
                (Some(resolved), certs)
            }
        };
        let positive = |key: &str, v: Option<i64>, default: usize| -> Result<usize> {
            match v {
                None => Ok(default),
                Some(n) if n >= 1 => Ok(n as usize),
                Some(n) => Err(Error::Message(format!(
                    "configuration invalide ({at}) : clé « {key} » doit être un entier \
                     strictement positif (reçu : {n})"
                ))),
            }
        };
        let format_retries = match &raw.format_retries {
            None => DEFAULT_FORMAT_RETRIES,
            Some(toml::Value::Integer(n)) if (0..=i64::from(u32::MAX)).contains(n) => *n as u32,
            Some(v) => {
                return Err(Error::Message(format!(
                    "configuration invalide ({at}) : clé « format_retries » doit être un entier \
                     positif ou nul (reçu : {v})"
                )))
            }
        };
        let max_calls = match &raw.max_calls {
            None => None,
            Some(toml::Value::Integer(n)) if *n >= 1 => {
                Some(usize::try_from(*n).unwrap_or(usize::MAX))
            }
            Some(v) => {
                return Err(Error::Message(format!(
                    "configuration invalide ({at}) : clé « max_calls » doit être un entier \
                     strictement positif (reçu : {v})"
                )))
            }
        };
        let enrich_code = match &raw.enrich_code {
            None => false,
            Some(toml::Value::Boolean(b)) => *b,
            Some(v) => {
                return Err(Error::Message(format!(
                    "configuration invalide ({at}) : clé « enrich_code » doit être un booléen, \
                     true ou false (reçu : {v})"
                )))
            }
        };
        let summary_language = match &raw.summary_language {
            None => DEFAULT_SUMMARY_LANGUAGE.to_string(),
            Some(toml::Value::String(s)) if summary_language_of(s).is_some() => {
                summary_language_of(s).unwrap_or_default()
            }
            Some(v) => {
                return Err(Error::Message(format!(
                    "configuration invalide ({at}) : clé « summary_language » doit être un {} \
                     (reçu : {})",
                    summary_language_expected(),
                    crate::escape::path(&v.to_string())
                )))
            }
        };
        let invalid = |key: &str, expected: &str, v: &toml::Value| {
            Error::Message(format!(
                "configuration invalide ({at}) : clé « {key} » doit être {expected} (reçu : {v})"
            ))
        };
        let thinking = match &raw.thinking {
            None => false,
            Some(toml::Value::Boolean(b)) => *b,
            Some(v) => return Err(invalid("thinking", "un booléen, true ou false", v)),
        };
        let effort = match &raw.effort {
            None => DEFAULT_EFFORT,
            Some(toml::Value::String(s)) if s == "low" => Effort::Low,
            Some(toml::Value::String(s)) if s == "medium" => Effort::Medium,
            Some(toml::Value::String(s)) if s == "high" => Effort::High,
            Some(toml::Value::String(s)) if s == "default" => Effort::ModelDefault,
            Some(v) => {
                return Err(invalid(
                    "effort",
                    "\"low\", \"medium\", \"high\" ou \"default\"",
                    v,
                ));
            }
        };
        let model_override = match &raw.model_override {
            None => ModelOverride::Warn,
            Some(toml::Value::String(s)) if s == "allow" => ModelOverride::Allow,
            Some(toml::Value::String(s)) if s == "warn" => ModelOverride::Warn,
            Some(toml::Value::String(s)) if s == "deny" => ModelOverride::Deny,
            Some(v) => {
                return Err(invalid(
                    "model_override",
                    "\"allow\", \"warn\" ou \"deny\"",
                    v,
                ));
            }
        };
        let max_output_tokens = match &raw.max_output_tokens {
            None => DEFAULT_MAX_OUTPUT_TOKENS,
            Some(toml::Value::Integer(n)) if (1..=i64::from(u32::MAX)).contains(n) => *n as u32,
            Some(v) => {
                return Err(invalid(
                    "max_output_tokens",
                    "un entier strictement positif",
                    v,
                ))
            }
        };
        let ask = match &raw.ask {
            None => AskOverrides::default(),
            Some(toml::Value::Table(t)) => {
                let a = RawAsk::deserialize(toml::Value::Table(t.clone()))
                    .map_err(|e| Error::Message(format!("configuration invalide ({at}) : {e}")))?;
                let key = |k: &str| format!("ask.{k}");
                let int = |k: &str, v: &Option<toml::Value>, min: i64, what: &str| match v {
                    None => Ok(None),
                    Some(toml::Value::Integer(n)) if *n >= min => Ok(Some(*n)),
                    Some(v) => Err(invalid(&key(k), what, v)),
                };
                let positive = "un entier strictement positif";
                let min_chars = format!("un entier d'au moins {ASK_MIN_CONTEXT_CHARS}");
                AskOverrides {
                    model: match &a.model {
                        None => None,
                        Some(toml::Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
                        Some(v) => return Err(invalid("ask.model", "une chaîne non vide", v)),
                    },
                    thinking: match &a.thinking {
                        None => None,
                        Some(toml::Value::Boolean(b)) => Some(*b),
                        Some(v) => {
                            return Err(invalid("ask.thinking", "un booléen, true ou false", v))
                        }
                    },
                    effort: match &a.effort {
                        None => None,
                        Some(v) => match v.as_str().and_then(Effort::parse) {
                            Some(e) => Some(e),
                            None => {
                                return Err(invalid(
                                    "ask.effort",
                                    "\"low\", \"medium\", \"high\" ou \"default\"",
                                    v,
                                ))
                            }
                        },
                    },
                    max_output_tokens: match &a.max_output_tokens {
                        None => None,
                        Some(toml::Value::Integer(n)) if (1..=i64::from(u32::MAX)).contains(n) => {
                            Some(*n as u32)
                        }
                        Some(v) => return Err(invalid("ask.max_output_tokens", positive, v)),
                    },
                    max_context_chars: int(
                        "max_context_chars",
                        &a.max_context_chars,
                        ASK_MIN_CONTEXT_CHARS as i64,
                        &min_chars,
                    )?
                    .map(|n| n as usize),
                    max_seeds: int("max_seeds", &a.max_seeds, 1, positive)?.map(|n| n as usize),
                    max_nodes: int("max_nodes", &a.max_nodes, 1, positive)?.map(|n| n as usize),
                }
            }
            Some(v) => return Err(invalid("ask", "une table [ask]", v)),
        };
        Ok(Config {
            provider,
            model,
            base_url,
            api_key_env,
            max_chunk_chars: positive(
                "max_chunk_chars",
                raw.max_chunk_chars,
                DEFAULT_MAX_CHUNK_CHARS,
            )?,
            concurrency: positive("concurrency", raw.concurrency, DEFAULT_CONCURRENCY)?,
            timeout_secs: positive(
                "timeout_secs",
                raw.timeout_secs,
                DEFAULT_TIMEOUT_SECS as usize,
            )? as u64,
            batch_files: positive("batch_files", raw.batch_files, DEFAULT_BATCH_FILES)?,
            format_retries,
            max_calls,
            enrich_code,
            thinking,
            effort,
            effort_explicit: raw.effort.is_some(),
            max_output_tokens,
            tls_ca_cert,
            tls_ca_certs,
            tls_insecure: raw.tls_insecure.unwrap_or(false),
            summary_language,
            model_override,
            ask,
        })
    }

    /// Valeur de la clé d'API lue dans la variable désignée par `api_key_env`
    /// (`None` si aucune variable n'est configurée). Erreur si elle est absente ou vide.
    pub fn api_key_with(&self, env: &dyn Fn(&str) -> Option<String>) -> Result<Option<String>> {
        let Some(var) = &self.api_key_env else {
            return Ok(None);
        };
        match env(var) {
            Some(v) if !v.is_empty() => Ok(Some(v)),
            _ => Err(Error::Message(format!(
                "la variable d'environnement {var} (clé api_key_env) est absente ou vide ; \
                 définissez-la avec la clé d'API du fournisseur {}",
                self.provider.as_str()
            ))),
        }
    }

    pub fn api_key(&self) -> Result<Option<String>> {
        self.api_key_with(&|k| std::env::var(k).ok())
    }
}

/// Résout `tls_ca_cert` : absolu tel quel, `~/…` depuis `HOME`, sinon relatif au dossier du
/// fichier de configuration.
fn resolve_ca_path(
    raw: &str,
    config: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    at: &str,
) -> Result<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        return match env("HOME").filter(|h| !h.is_empty()) {
            Some(home) => Ok(PathBuf::from(home).join(rest)),
            None => Err(Error::Message(format!(
                "configuration invalide ({at}) : clé « tls_ca_cert » : {raw} commence par ~/ \
                 mais HOME n'est pas défini"
            ))),
        };
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    Ok(config.parent().unwrap_or(Path::new("")).join(p))
}

/// Lit les blocs `CERTIFICATE` du PEM (les autres blocs sont ignorés) et vérifie que chacun
/// est exploitable comme autorité.
fn read_ca_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let fail = |why: String| {
        Error::Message(format!(
            "configuration invalide : clé « tls_ca_cert » ({}) : {why}",
            path.display()
        ))
    };
    let bytes = std::fs::read(path).map_err(|e| fail(format!("lecture impossible : {e}")))?;
    let mut certs = Vec::new();
    for item in CertificateDer::pem_slice_iter(&bytes) {
        certs.push(item.map_err(|e| fail(format!("PEM invalide : {e}")))?);
    }
    if certs.is_empty() {
        return Err(fail("aucun bloc CERTIFICATE dans le fichier".into()));
    }
    let mut roots = RootCertStore::empty();
    for cert in &certs {
        roots
            .add(cert.clone())
            .map_err(|e| fail(format!("certificat inexploitable : {e}")))?;
    }
    Ok(certs)
}

#[cfg(test)]
mod project_tests {
    use super::*;

    fn base() -> Config {
        Config {
            provider: Provider::OpenAi,
            model: "m-perso".into(),
            base_url: Some("http://x".into()),
            api_key_env: Some("K".into()),
            max_chunk_chars: 100,
            concurrency: 2,
            timeout_secs: 30,
            batch_files: 3,
            format_retries: 1,
            max_calls: None,
            enrich_code: false,
            thinking: false,
            effort: DEFAULT_EFFORT,
            effort_explicit: false,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            tls_ca_cert: None,
            tls_ca_certs: Vec::new(),
            tls_insecure: true,
            summary_language: DEFAULT_SUMMARY_LANGUAGE.to_string(),
            model_override: ModelOverride::Warn,
            ask: AskOverrides::default(),
        }
    }

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn req042_ac2_chaque_reglage_seul_remplace_sa_valeur() {
        let cases: Vec<(&str, ProjectSettings, Config)> = vec![
            (
                "model",
                ProjectSettings {
                    model: Some("m".into()),
                    ..Default::default()
                },
                Config {
                    model: "m".into(),
                    ..base()
                },
            ),
            (
                "summary_language",
                ProjectSettings {
                    summary_language: Some("anglais".into()),
                    ..Default::default()
                },
                Config {
                    summary_language: "anglais".into(),
                    ..base()
                },
            ),
            (
                "max_chunk_chars",
                ProjectSettings {
                    max_chunk_chars: Some(7),
                    ..Default::default()
                },
                Config {
                    max_chunk_chars: 7,
                    ..base()
                },
            ),
            (
                "concurrency",
                ProjectSettings {
                    concurrency: Some(9),
                    ..Default::default()
                },
                Config {
                    concurrency: 9,
                    ..base()
                },
            ),
            (
                "timeout_secs",
                ProjectSettings {
                    timeout_secs: Some(5),
                    ..Default::default()
                },
                Config {
                    timeout_secs: 5,
                    ..base()
                },
            ),
            (
                "batch_files",
                ProjectSettings {
                    batch_files: Some(8),
                    ..Default::default()
                },
                Config {
                    batch_files: 8,
                    ..base()
                },
            ),
            (
                "format_retries",
                ProjectSettings {
                    format_retries: Some(0),
                    ..Default::default()
                },
                Config {
                    format_retries: 0,
                    ..base()
                },
            ),
        ];
        assert_eq!(cases.len(), PROJECT_SETTINGS.len());
        for (name, p, expected) in cases {
            let (c, from) = base().with_project(&p, None, None);
            assert_eq!(c, expected, "{name}");
            assert_eq!(from, vec![name]);
        }
    }

    #[test]
    fn req042_ac2_vide_option_et_valeur_egale() {
        let (c, from) = base().with_project(&ProjectSettings::default(), None, None);
        assert_eq!(c, base());
        assert!(from.is_empty());

        let p = ProjectSettings {
            batch_files: Some(2),
            format_retries: Some(5),
            concurrency: Some(2),
            ..Default::default()
        };
        let (c, from) = base().with_project(&p, Some(1), Some(0));
        assert_eq!((c.batch_files, c.format_retries), (1, 0));
        assert_eq!(from, vec!["concurrency"]);
        // valeur égale à la valeur personnelle : nommée quand même
        assert_eq!(c.concurrency, 2);
    }

    #[test]
    fn req042_ac15_valeurs_invalides() {
        let cases = [
            ("model = \"\"", "model", "\"\"", "chaîne non vide attendue"),
            (
                "model = \"  \"",
                "model",
                "\"  \"",
                "chaîne non vide attendue",
            ),
            ("model = 3", "model", "3", "chaîne non vide attendue"),
            (
                "max_chunk_chars = 0",
                "max_chunk_chars",
                "0",
                "entier strictement positif attendu, au plus 1000000",
            ),
            (
                "concurrency = -1",
                "concurrency",
                "-1",
                "entier strictement positif attendu, au plus 64",
            ),
            (
                "timeout_secs = 1.5",
                "timeout_secs",
                "1.5",
                "entier strictement positif attendu, au plus 3600",
            ),
            (
                "batch_files = \"2\"",
                "batch_files",
                "\"2\"",
                "entier strictement positif attendu, au plus 100",
            ),
            (
                "batch_files = true",
                "batch_files",
                "true",
                "entier strictement positif attendu, au plus 100",
            ),
            (
                "format_retries = -1",
                "format_retries",
                "-1",
                "entier positif ou nul attendu, au plus 10",
            ),
            (
                "format_retries = 11",
                "format_retries",
                "11",
                "entier positif ou nul attendu, au plus 10",
            ),
        ];
        for (toml, key, shown, expected) in cases {
            let e = ProjectSettings::from_table(&table(toml), "./.nephila.toml").unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("./.nephila.toml : clé `{key}` : valeur invalide `{shown}` ({expected})")
            );
        }
        let ok = ProjectSettings::from_table(
            &table("format_retries = 10\nmodel = \"x\"\nbatch_files = 1"),
            "f",
        )
        .unwrap();
        assert_eq!(ok.format_retries, Some(10));
        assert_eq!(ok.model.as_deref(), Some("x"));
        assert_eq!(ok.batch_files, Some(1));
    }

    #[test]
    fn req042_ac17_plafonds_declares_une_fois() {
        assert_eq!(
            PROJECT_CAPS,
            [
                ("max_chunk_chars", 1_000_000),
                ("concurrency", 64),
                ("timeout_secs", 3600),
                ("batch_files", 100),
                ("format_retries", 10)
            ]
        );
        let keys: Vec<&str> = PROJECT_CAPS.iter().map(|(k, _)| *k).collect();
        let settings: Vec<&str> = PROJECT_SETTINGS
            .iter()
            .copied()
            .filter(|k| *k != "model" && *k != "summary_language")
            .collect();
        assert_eq!(keys, settings);
        assert_eq!(project_cap("concurrency"), Some(64));
        assert_eq!(project_cap("model"), None);
    }

    fn expected_for(key: &str) -> String {
        let cap = project_cap(key).unwrap();
        if key == "format_retries" {
            format!("entier positif ou nul attendu, au plus {cap}")
        } else {
            format!("entier strictement positif attendu, au plus {cap}")
        }
    }

    #[test]
    fn req042_ac17_valeur_au_plafond_acceptee_au_dessus_refusee() {
        for (key, cap) in PROJECT_CAPS {
            let ok = ProjectSettings::from_table(&table(&format!("{key} = {cap}")), "f").unwrap();
            let got = match key {
                "max_chunk_chars" => ok.max_chunk_chars.unwrap() as u64,
                "concurrency" => ok.concurrency.unwrap() as u64,
                "timeout_secs" => ok.timeout_secs.unwrap(),
                "batch_files" => ok.batch_files.unwrap() as u64,
                _ => u64::from(ok.format_retries.unwrap()),
            };
            assert_eq!(got, cap, "{key}");
            let e = ProjectSettings::from_table(&table(&format!("{key} = {}", cap + 1)), "f")
                .unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "f : clé `{key}` : valeur invalide `{}` ({})",
                    cap + 1,
                    expected_for(key)
                )
            );
        }
        for toml in [
            "format_retries = 4294967295",
            "concurrency = 9223372036854775807",
        ] {
            let key = toml.split(' ').next().unwrap();
            let value = toml.rsplit(' ').next().unwrap();
            let e = ProjectSettings::from_table(&table(toml), "f").unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "f : clé `{key}` : valeur invalide `{value}` ({})",
                    expected_for(key)
                )
            );
        }
        let all = ProjectSettings::from_table(
            &table(
                "model = \"m\"\nmax_chunk_chars = 1000000\nconcurrency = 64\ntimeout_secs = 3600\nbatch_files = 100\nformat_retries = 10",
            ),
            "f",
        )
        .unwrap();
        assert_eq!(all.max_chunk_chars, Some(1_000_000));
        assert_eq!(all.concurrency, Some(64));
        assert_eq!(all.timeout_secs, Some(3600));
        assert_eq!(all.batch_files, Some(100));
        assert_eq!(all.format_retries, Some(10));
    }

    #[test]
    fn req042_premier_reglage_invalide_dans_l_ordre_de_la_liste() {
        let e = ProjectSettings::from_table(&table("concurrency = 0\nmodel = 1"), "f").unwrap_err();
        assert!(e.to_string().contains("clé `model`"), "{e}");
    }
}

#[cfg(test)]
mod llm_options_tests {
    use super::*;

    fn parse(extra: &str) -> Result<Config> {
        Config::parse(
            &format!("provider = \"mock\"\n{extra}"),
            Path::new("/x/config.toml"),
        )
    }

    #[test]
    fn req058_ac1_defauts() {
        let c = parse("").unwrap();
        assert_eq!(c.effort, Effort::Low);
        assert!(!c.thinking);
        assert_eq!(c.max_output_tokens, 16384);
    }

    #[test]
    fn req157_ac7_effort_default_et_effort_ecrit() {
        let c = parse("effort = \"default\"").unwrap();
        assert_eq!(c.effort, Effort::ModelDefault);
        assert_eq!(c.effort.as_str(), "default");
        assert!(c.effort_explicit);
        for v in ["low", "medium", "high"] {
            assert!(parse(&format!("effort = \"{v}\"")).unwrap().effort_explicit);
        }
        let c = parse("").unwrap();
        assert_eq!(c.effort, Effort::Low);
        assert!(!c.effort_explicit);
    }

    #[test]
    fn req157_ac7_none_default_majuscules_et_auto_refuses() {
        for v in ["none", "DEFAULT", "auto"] {
            let msg = parse(&format!("effort = \"{v}\"")).unwrap_err().to_string();
            assert!(
                msg.contains("\"low\", \"medium\", \"high\" ou \"default\"")
                    && msg.contains(&format!("(reçu : \"{v}\")")),
                "{msg}"
            );
        }
    }

    #[test]
    fn req058_ac1_valeurs_acceptees() {
        assert_eq!(
            parse("effort = \"medium\"").unwrap().effort.as_str(),
            "medium"
        );
        assert_eq!(parse("effort = \"high\"").unwrap().effort, Effort::High);
        assert!(parse("thinking = true").unwrap().thinking);
        assert_eq!(parse("max_output_tokens = 1").unwrap().max_output_tokens, 1);
        assert_eq!(
            parse("max_output_tokens = 200000")
                .unwrap()
                .max_output_tokens,
            200000
        );
    }

    #[test]
    fn req058_ac1_cles_dans_l_erreur_de_cle_inconnue() {
        let msg = parse("zz_inconnue = 1").unwrap_err().to_string();
        for k in ["`effort`", "`thinking`", "`max_output_tokens`"] {
            assert!(msg.contains(k), "{msg}");
        }
    }

    #[test]
    fn req058_ac2_valeurs_invalides() {
        let cases = [
            (
                "effort = \"max\"",
                "effort",
                "\"low\", \"medium\", \"high\" ou \"default\"",
                "\"max\"",
            ),
            (
                "effort = \"\"",
                "effort",
                "\"low\", \"medium\", \"high\" ou \"default\"",
                "\"\"",
            ),
            (
                "effort = \"LOW\"",
                "effort",
                "\"low\", \"medium\", \"high\" ou \"default\"",
                "\"LOW\"",
            ),
            (
                "effort = 1",
                "effort",
                "\"low\", \"medium\", \"high\" ou \"default\"",
                "1",
            ),
            (
                "thinking = \"oui\"",
                "thinking",
                "un booléen, true ou false",
                "\"oui\"",
            ),
            ("thinking = 1", "thinking", "un booléen, true ou false", "1"),
            (
                "max_output_tokens = 0",
                "max_output_tokens",
                "un entier strictement positif",
                "0",
            ),
            (
                "max_output_tokens = -5",
                "max_output_tokens",
                "un entier strictement positif",
                "-5",
            ),
            (
                "max_output_tokens = 1.5",
                "max_output_tokens",
                "un entier strictement positif",
                "1.5",
            ),
            (
                "max_output_tokens = \"16384\"",
                "max_output_tokens",
                "un entier strictement positif",
                "\"16384\"",
            ),
            (
                "max_output_tokens = 4294967296",
                "max_output_tokens",
                "un entier strictement positif",
                "4294967296",
            ),
        ];
        for (line, key, expected, got) in cases {
            let msg = parse(line).unwrap_err().to_string();
            assert_eq!(
                msg,
                format!(
                    "configuration invalide (/x/config.toml) : clé « {key} » doit être {expected} (reçu : {got})"
                ),
                "{line}"
            );
        }
    }
}

#[cfg(test)]
mod model_override_tests {
    use super::*;

    fn parse(extra: &str) -> Result<Config> {
        Config::parse(
            &format!("provider = \"mock\"\n{extra}"),
            Path::new("/x/config.toml"),
        )
    }

    fn proj(s: &str) -> ProjectSettings {
        ProjectSettings::from_table(&s.parse::<toml::Table>().unwrap(), "./.nephila.toml").unwrap()
    }

    fn with_policy(p: ModelOverride, ask: AskOverrides) -> Config {
        let mut c = parse("model = \"m-perso\"").unwrap();
        c.model_override = p;
        c.ask = ask;
        c
    }

    #[test]
    fn req195_ac1_lecture_et_valeurs() {
        assert_eq!(parse("").unwrap().model_override, ModelOverride::Warn);
        for (v, want) in [
            ("allow", ModelOverride::Allow),
            ("warn", ModelOverride::Warn),
            ("deny", ModelOverride::Deny),
        ] {
            let c = parse(&format!("model_override = \"{v}\"")).unwrap();
            assert_eq!(c.model_override, want);
            assert_eq!(c.model_override.as_str(), v);
        }
        for (v, shown) in [
            ("\"Deny\"", "\"Deny\""),
            ("\"\"", "\"\""),
            ("\"oui\"", "\"oui\""),
            ("1", "1"),
            ("true", "true"),
        ] {
            let msg = parse(&format!("model_override = {v}"))
                .unwrap_err()
                .to_string();
            assert_eq!(
                msg,
                format!(
                    "configuration invalide (/x/config.toml) : clé « model_override » doit être \"allow\", \"warn\" ou \"deny\" (reçu : {shown})"
                )
            );
        }
    }

    #[test]
    fn req195_ac1_refusee_dans_le_fichier_de_projet() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".nephila.toml"),
            "model_override = \"deny\"\n",
        )
        .unwrap();
        let e = crate::filter::ProjectFile::load(dir.path())
            .map(|_| ())
            .expect_err("clé refusée")
            .to_string();
        assert!(e.contains("clé inconnue `model_override`"), "{e}");
        let permitted = e.split("clés permises").nth(1).unwrap_or("");
        assert!(!permitted.contains("model_override"), "{e}");
        assert!(!PROJECT_SETTINGS.contains(&"model_override"));
        assert_eq!(PROJECT_SETTINGS.len(), 7);
    }

    #[test]
    fn req195_ac2_fusion_selon_la_politique() {
        let p = proj("model = \"m-projet\"\nconcurrency = 2");
        for pol in [ModelOverride::Allow, ModelOverride::Warn] {
            let (c, from) = with_policy(pol, AskOverrides::default()).with_project(&p, None, None);
            assert_eq!(c.model, "m-projet");
            assert_eq!(c.concurrency, 2);
            assert_eq!(from, vec!["model", "concurrency"]);
        }
        let (c, from) =
            with_policy(ModelOverride::Deny, AskOverrides::default()).with_project(&p, None, None);
        assert_eq!(c.model, "m-perso");
        assert_eq!(c.concurrency, 2);
        assert_eq!(from, vec!["concurrency"]);
    }

    #[test]
    fn req195_ac3_for_ask_selon_la_politique() {
        let p = proj("model = \"m-projet\"\n[ask]\nmodel = \"m-ask-projet\"");
        let (c, _, m) =
            with_policy(ModelOverride::Deny, AskOverrides::default()).for_ask(&p, None, None);
        assert_eq!(c.model, "m-perso");
        assert!(m.is_empty(), "{m:?}");
        let perso = AskOverrides {
            model: Some("m-ask-perso".into()),
            ..Default::default()
        };
        let (c, _, m) = with_policy(ModelOverride::Deny, perso).for_ask(&p, None, None);
        assert_eq!(c.model, "m-ask-perso");
        assert!(m.is_empty(), "{m:?}");
        for pol in [ModelOverride::Allow, ModelOverride::Warn] {
            let (c, _, m) = with_policy(pol, AskOverrides::default()).for_ask(&p, None, None);
            assert_eq!(c.model, "m-ask-projet");
            assert_eq!(m, vec!["ask.model"]);
        }
        // Sous deny, les autres réglages de [ask] du dépôt s'appliquent toujours.
        let p2 = proj("[ask]\nmodel = \"m-ask-projet\"\nthinking = true");
        let (c, _, m) =
            with_policy(ModelOverride::Deny, AskOverrides::default()).for_ask(&p2, None, None);
        assert!(c.thinking);
        assert_eq!(m, vec!["ask.thinking"]);
    }
}

#[cfg(test)]
mod ask_tests {
    use super::*;

    const P: &str = "/x/config.toml";

    fn personal(extra: &str) -> Result<Config> {
        Config::parse(&format!("provider = \"mock\"\n{extra}"), Path::new(P))
    }

    fn project(toml: &str) -> Result<ProjectSettings> {
        ProjectSettings::from_table(&toml.parse().unwrap(), "./.nephila.toml")
    }

    #[test]
    fn req186_ac1_max_calls_du_chargeur() {
        assert_eq!(personal("").unwrap().max_calls, None);
        assert_eq!(personal("max_calls = 500").unwrap().max_calls, Some(500));
        for shown in ["0", "-3", "\"5\"", "1.5", "true"] {
            let e = personal(&format!("max_calls = {shown}")).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "configuration invalide ({P}) : clé « max_calls » doit être un entier \
                     strictement positif (reçu : {shown})"
                )
            );
        }
        // jamais dans le fichier de projet
        assert!(!PROJECT_SETTINGS.contains(&"max_calls"));
        assert_eq!(project_cap("max_calls"), None);
    }

    fn forty() -> String {
        "é".repeat(SUMMARY_LANGUAGE_MAX_CHARS)
    }

    /// Valeurs TOML (texte source) refusées par les deux fichiers, avec la valeur réécrite.
    fn bad_languages() -> Vec<(String, String)> {
        let long = "a".repeat(SUMMARY_LANGUAGE_MAX_CHARS + 1);
        vec![
            ("\"\"".into(), "\"\"".into()),
            ("\"   \"".into(), "\"   \"".into()),
            (format!("\"{long}\""), format!("\"{long}\"")),
            ("\"en_US\"".into(), "\"en_US\"".into()),
            ("\"fr2\"".into(), "\"fr2\"".into()),
            ("\"français;\"".into(), "\"français;\"".into()),
            ("\"fr\\u0007\"".into(), "\"fr\\u0007\"".into()),
            ("3".into(), "3".into()),
            ("true".into(), "true".into()),
            ("[\"fr\"]".into(), "[\"fr\"]".into()),
        ]
    }

    #[test]
    fn req172_ac2_langue_de_la_configuration_personnelle() {
        assert_eq!(personal("").unwrap().summary_language, "français");
        let good = [
            ("\"anglais\"", "anglais"),
            ("\"  English  \"", "English"),
            ("\"portugais du Brésil\"", "portugais du Brésil"),
            ("\"bokmål\"", "bokmål"),
            ("\"zh-Hant\"", "zh-Hant"),
            ("\"langue d'oc\"", "langue d'oc"),
        ];
        for (raw, want) in good {
            let c = personal(&format!("summary_language = {raw}")).unwrap();
            assert_eq!(c.summary_language, want, "{raw}");
        }
        let c = personal(&format!("summary_language = \"{}\"", forty())).unwrap();
        assert_eq!(c.summary_language, forty());
        for (raw, shown) in bad_languages() {
            let e = personal(&format!("summary_language = {raw}")).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "configuration invalide ({P}) : clé « summary_language » doit être un nom de \
                     langue de 1 à 40 lettres, espaces, traits d'union ou apostrophes (reçu : {shown})"
                ),
                "{raw}"
            );
        }
        // Contrôle C1 et marque de direction : écrits sous forme échappée.
        let e = personal("summary_language = \"fr\u{202E}\u{85}\"").unwrap_err();
        assert!(
            e.to_string().ends_with("(reçu : \"fr\\u{202E}\\u{0085}\")"),
            "{e}"
        );
    }

    #[test]
    fn req172_ac3_langue_du_fichier_de_projet() {
        assert_eq!(
            PROJECT_SETTINGS,
            [
                "model",
                "summary_language",
                "max_chunk_chars",
                "concurrency",
                "timeout_secs",
                "batch_files",
                "format_retries"
            ]
        );
        assert_eq!(PROJECT_CAPS.len(), 5);
        assert_eq!(project_cap("summary_language"), None);
        let p = project("summary_language = \"  anglais \"").unwrap();
        assert_eq!(p.summary_language.as_deref(), Some("anglais"));
        let perso = personal("summary_language = \"espagnol\"").unwrap();
        let (c, from) = perso.clone().with_project(&p, None, None);
        assert_eq!(c.summary_language, "anglais");
        assert_eq!(from, vec!["summary_language"]);
        let both = project("model = \"m\"\nsummary_language = \"anglais\"").unwrap();
        let (_, from) = perso.with_project(&both, None, None);
        assert_eq!(from, vec!["model", "summary_language"]);
        assert_eq!(
            project(&format!("summary_language = \"{}\"", forty()))
                .unwrap()
                .summary_language,
            Some(forty())
        );
        for (raw, shown) in bad_languages() {
            let e = project(&format!("summary_language = {raw}")).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "./.nephila.toml : clé `summary_language` : valeur invalide `{shown}` (nom de \
                     langue de 1 à 40 lettres, espaces, traits d'union ou apostrophes attendu)"
                ),
                "{raw}"
            );
        }
        let e = project("summary_language = \"fr\u{202E}\u{85}\"").unwrap_err();
        assert!(e.to_string().contains("`\"fr\\u{202E}\\u{0085}\"`"), "{e}");
        // `[ask]` : la clé reste inconnue de la section.
        let e = project("[ask]\nsummary_language = \"anglais\"").unwrap_err();
        assert!(
            e.to_string()
                .contains("clé inconnue `ask.summary_language`"),
            "{e}"
        );
        let e = personal("[ask]\nsummary_language = \"anglais\"").unwrap_err();
        assert!(e.to_string().contains("summary_language"), "{e}");
        // `for_ask` ignore la langue : aucune marque.
        let (_, _, marks) = personal("").unwrap().for_ask(
            &project("summary_language = \"anglais\"").unwrap(),
            None,
            None,
        );
        assert!(marks.is_empty(), "{marks:?}");
    }

    #[test]
    fn req162_ac1_chargeur_personnel() {
        assert_eq!(personal("").unwrap().ask, AskOverrides::default());
        assert_eq!(
            personal("[ask]\n").unwrap().ask,
            AskOverrides::default(),
            "[ask] vide"
        );
        let c = personal(
            "model = \"m-glob\"\nmax_output_tokens = 99\n[ask]\nmodel = \"m-ask\"\nthinking = true\n\
             effort = \"default\"\nmax_output_tokens = 2048\nmax_context_chars = 30000\n\
             max_seeds = 30\nmax_nodes = 120\n",
        )
        .unwrap();
        assert_eq!(
            c.ask,
            AskOverrides {
                model: Some("m-ask".into()),
                thinking: Some(true),
                effort: Some(Effort::ModelDefault),
                max_output_tokens: Some(2048),
                max_context_chars: Some(30000),
                max_seeds: Some(30),
                max_nodes: Some(120),
            }
        );
        // Les clés globales ne bougent pas.
        assert_eq!((c.model.as_str(), c.max_output_tokens), ("m-glob", 99));
        assert!(!c.thinking && !c.effort_explicit);
        // Table en ligne : une table.
        let inline = personal("ask = { model = \"m\" }").unwrap();
        assert_eq!(inline.ask.model.as_deref(), Some("m"));
        // Les 7 clés acceptées dans la section.
        let e = personal("[ask]\nzz = 1").unwrap_err().to_string();
        assert!(
            e.starts_with("configuration invalide (/x/config.toml) : "),
            "{e}"
        );
        assert!(e.contains("zz"), "{e}");
        for key in ASK_SETTINGS {
            assert!(e.contains(&format!("`{key}`")), "{key} : {e}");
        }
        let e = personal("[ask]\nbase_url = \"http://127.0.0.1:9\"").unwrap_err();
        assert!(e.to_string().contains("base_url"), "{e}");
    }

    #[test]
    fn req162_ac7_fautes_de_la_configuration_personnelle() {
        let cases = [
            ("model = \"\"", "ask.model", "une chaîne non vide", "\"\""),
            (
                "thinking = \"oui\"",
                "ask.thinking",
                "un booléen, true ou false",
                "\"oui\"",
            ),
            (
                "effort = \"max\"",
                "ask.effort",
                "\"low\", \"medium\", \"high\" ou \"default\"",
                "\"max\"",
            ),
            (
                "max_output_tokens = 0",
                "ask.max_output_tokens",
                "un entier strictement positif",
                "0",
            ),
            (
                "max_context_chars = 999",
                "ask.max_context_chars",
                "un entier d'au moins 1000",
                "999",
            ),
            (
                "max_seeds = 0",
                "ask.max_seeds",
                "un entier strictement positif",
                "0",
            ),
            (
                "max_nodes = -1",
                "ask.max_nodes",
                "un entier strictement positif",
                "-1",
            ),
        ];
        for (line, key, expected, shown) in cases {
            let e = personal(&format!("[ask]\n{line}")).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "configuration invalide ({P}) : clé « {key} » doit être {expected} (reçu : {shown})"
                )
            );
        }
        for v in ["3", "\"x\""] {
            let e = personal(&format!("ask = {v}")).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("configuration invalide ({P}) : clé « ask » doit être une table [ask] (reçu : {v})")
            );
        }
        // Sans plafond : acceptés (AC-163-5).
        let c = personal("[ask]\nmax_context_chars = 1000000\nmax_seeds = 1000\nmax_nodes = 10000\nmax_output_tokens = 99999")
            .unwrap();
        assert_eq!(c.ask.max_context_chars, Some(1_000_000));
        assert_eq!(c.ask.max_seeds, Some(1000));
        assert_eq!(c.ask.max_nodes, Some(10_000));
        assert_eq!(c.ask.max_output_tokens, Some(99_999));
        let c = personal("[ask]\nmax_context_chars = 1000").unwrap();
        assert_eq!(c.ask.max_context_chars, Some(ASK_MIN_CONTEXT_CHARS));
    }

    #[test]
    fn req162_ac2_from_table() {
        assert_eq!(
            ASK_CAPS,
            [
                ("max_output_tokens", 16384),
                ("max_context_chars", 100_000),
                ("max_seeds", 100),
                ("max_nodes", 500)
            ]
        );
        assert_eq!(ask_cap("max_nodes"), Some(500));
        assert_eq!(ask_cap("model"), None);
        assert_eq!(
            ASK_SETTINGS,
            [
                "model",
                "thinking",
                "effort",
                "max_output_tokens",
                "max_context_chars",
                "max_seeds",
                "max_nodes"
            ]
        );
        let s = project(
            "[ask]\nmodel = \"m\"\nthinking = false\neffort = \"high\"\nmax_output_tokens = 16384\n\
             max_context_chars = 100000\nmax_seeds = 100\nmax_nodes = 500\n",
        )
        .unwrap();
        assert_eq!(
            s.ask,
            AskOverrides {
                model: Some("m".into()),
                thinking: Some(false),
                effort: Some(Effort::High),
                max_output_tokens: Some(16384),
                max_context_chars: Some(100_000),
                max_seeds: Some(100),
                max_nodes: Some(500),
            }
        );
        assert_eq!(project("").unwrap().ask, AskOverrides::default());
        // Tous les réglages du premier niveau et les 7 clés au plafond, ensemble.
        let all = project(
            "model = \"m\"\nmax_chunk_chars = 1000000\nconcurrency = 64\ntimeout_secs = 3600\n\
             batch_files = 100\nformat_retries = 10\n[ask]\nmax_output_tokens = 16384\n\
             max_context_chars = 100000\nmax_seeds = 100\nmax_nodes = 500\n",
        )
        .unwrap();
        assert_eq!(all.ask.max_nodes, Some(500));
        assert_eq!(all.format_retries, Some(10));
        // Plafond et texte d'E-10.
        for (key, cap) in ASK_CAPS {
            let e = project(&format!("[ask]\n{key} = {}", cap + 1)).unwrap_err();
            let expected = if key == "max_context_chars" {
                "entier d'au moins 1000 attendu".to_string()
            } else {
                "entier strictement positif attendu".to_string()
            };
            assert_eq!(
                e.to_string(),
                format!(
                    "./.nephila.toml : clé `ask.{key}` : valeur invalide `{}` ({expected}, au plus {cap})",
                    cap + 1
                )
            );
        }
        let e = project("[ask]\nmax_output_tokens = 16385").unwrap_err();
        assert_eq!(
            e.to_string(),
            "./.nephila.toml : clé `ask.max_output_tokens` : valeur invalide `16385` (entier strictement positif attendu, au plus 16384)"
        );
        // Valeurs de AC-163-5 en projet.
        for (key, v) in [
            ("max_context_chars", "100001"),
            ("max_context_chars", "999"),
            ("max_seeds", "101"),
            ("max_seeds", "0"),
            ("max_nodes", "501"),
            ("max_nodes", "-1"),
        ] {
            let e = project(&format!("[ask]\n{key} = {v}")).unwrap_err();
            assert!(
                e.to_string().starts_with(&format!(
                    "./.nephila.toml : clé `ask.{key}` : valeur invalide `{v}` ("
                )),
                "{e}"
            );
        }
        let e = project("[ask]\nmax_context_chars = 1000").unwrap();
        assert_eq!(e.ask.max_context_chars, Some(1000));
        // Valeurs sans plafond : réglages invalides de types.
        for (line, key, shown, expected) in [
            ("model = \"\"", "model", "\"\"", "chaîne non vide attendue"),
            (
                "thinking = \"oui\"",
                "thinking",
                "\"oui\"",
                "booléen attendu, true ou false",
            ),
            (
                "effort = \"max\"",
                "effort",
                "\"max\"",
                "\"low\", \"medium\", \"high\" ou \"default\" attendu",
            ),
        ] {
            let e = project(&format!("[ask]\n{line}")).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "./.nephila.toml : clé `ask.{key}` : valeur invalide `{shown}` ({expected})"
                )
            );
        }
        // Clés inconnues, clés de connexion comprises ; `ask` qui n'est pas une table.
        let list = ASK_SETTINGS.join(", ");
        for key in [
            "zz",
            "base_url",
            "provider",
            "tls_insecure",
            "format_retries",
        ] {
            let e = project(&format!("[ask]\n{key} = \"http://127.0.0.1:9\"")).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("./.nephila.toml : clé inconnue `ask.{key}` (clés permises dans [ask] : {list})")
            );
        }
        let e = project("ask = 3").unwrap_err();
        assert_eq!(
            e.to_string(),
            "./.nephila.toml : clé `ask` : valeur invalide `3` (table [ask] attendue)"
        );
        // Premier niveau d'abord, puis [ask].
        let e = project("[ask]\nmax_nodes = 501\n").unwrap_err();
        assert!(e.to_string().contains("ask.max_nodes"));
        let e = project("concurrency = 0\n[ask]\nmodel = \"\"\n").unwrap_err();
        assert!(e.to_string().contains("clé `concurrency`"), "{e}");
    }

    fn base() -> Config {
        personal("model = \"m-perso\"").unwrap()
    }

    fn with_ask(ask: AskOverrides) -> Config {
        Config { ask, ..base() }
    }

    #[test]
    fn req162_ac3_fusion_for_ask() {
        // Sans [ask] : valeurs de l'enrichissement, défauts propres.
        let (c, l, m) = base().for_ask(&ProjectSettings::default(), None, None);
        assert_eq!(
            (
                c.model.as_str(),
                c.thinking,
                c.effort,
                c.effort_explicit,
                c.max_output_tokens
            ),
            ("m-perso", false, Effort::Low, false, 16384)
        );
        assert_eq!(l, AskLimits::default());
        assert!(m.is_empty());

        // model : global projet, [ask] personnel, [ask] projet.
        let proj_model = ProjectSettings {
            model: Some("m-projet".into()),
            ..Default::default()
        };
        let (c, _, m) = base().for_ask(&proj_model, None, None);
        assert_eq!(c.model, "m-projet");
        assert_eq!(m, vec!["model"]);
        let perso_ask = AskOverrides {
            model: Some("m-ask".into()),
            ..Default::default()
        };
        let (c, _, m) = with_ask(perso_ask.clone()).for_ask(&proj_model, None, None);
        assert_eq!(c.model, "m-ask");
        assert!(m.is_empty(), "{m:?}");
        let proj_ask = ProjectSettings {
            ask: AskOverrides {
                model: Some("m-ask-projet".into()),
                ..Default::default()
            },
            ..proj_model.clone()
        };
        let (c, _, m) = with_ask(perso_ask).for_ask(&proj_ask, None, None);
        assert_eq!(c.model, "m-ask-projet");
        assert_eq!(m, vec!["ask.model"]);

        // thinking, effort, max_output_tokens : même gradation, chacun seul.
        let glob = Config {
            thinking: true,
            effort: Effort::Medium,
            effort_explicit: true,
            max_output_tokens: 777,
            ..base()
        };
        let (c, _, _) = glob
            .clone()
            .for_ask(&ProjectSettings::default(), None, None);
        assert_eq!(
            (c.thinking, c.effort, c.effort_explicit, c.max_output_tokens),
            (true, Effort::Medium, true, 777)
        );
        let perso = Config {
            ask: AskOverrides {
                thinking: Some(false),
                effort: Some(Effort::High),
                max_output_tokens: Some(2048),
                ..Default::default()
            },
            ..glob
        };
        let (c, _, m) = perso
            .clone()
            .for_ask(&ProjectSettings::default(), None, None);
        assert_eq!(
            (c.thinking, c.effort, c.max_output_tokens),
            (false, Effort::High, 2048)
        );
        assert!(m.is_empty());
        let proj = ProjectSettings {
            ask: AskOverrides {
                thinking: Some(true),
                effort: Some(Effort::Low),
                max_output_tokens: Some(1024),
                ..Default::default()
            },
            ..Default::default()
        };
        let (c, _, m) = perso.for_ask(&proj, None, None);
        assert_eq!(
            (c.thinking, c.effort, c.max_output_tokens),
            (true, Effort::Low, 1024)
        );
        assert_eq!(
            m,
            vec!["ask.thinking", "ask.effort", "ask.max_output_tokens"]
        );

        // effort_explicit : [ask].effort d'un fichier ou effort global écrit, rien d'autre.
        let (c, _, _) = base().for_ask(
            &ProjectSettings {
                ask: AskOverrides {
                    effort: Some(Effort::Low),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
            None,
        );
        assert!(c.effort_explicit);
        let (c, _, _) = with_ask(AskOverrides {
            effort: Some(Effort::Low),
            ..Default::default()
        })
        .for_ask(&ProjectSettings::default(), None, None);
        assert!(c.effort_explicit);
        let (c, _, _) = Config {
            effort_explicit: true,
            ..base()
        }
        .for_ask(&ProjectSettings::default(), None, None);
        assert!(c.effort_explicit);
        let (c, _, _) = with_ask(AskOverrides {
            thinking: Some(true),
            ..Default::default()
        })
        .for_ask(&ProjectSettings::default(), None, None);
        assert!(!c.effort_explicit);
    }

    #[test]
    fn req162_ac3_bornes_option_projet_personnel_defaut() {
        let perso = with_ask(AskOverrides {
            max_context_chars: Some(5000),
            max_seeds: Some(7),
            max_nodes: Some(70),
            ..Default::default()
        });
        let (_, l, m) = perso
            .clone()
            .for_ask(&ProjectSettings::default(), None, None);
        assert_eq!(
            l,
            AskLimits {
                max_context_chars: 5000,
                max_seeds: 7,
                max_nodes: 70
            }
        );
        assert!(m.is_empty());
        let proj = ProjectSettings {
            timeout_secs: Some(600),
            format_retries: Some(3),
            ask: AskOverrides {
                max_context_chars: Some(2000),
                max_seeds: Some(8),
                max_nodes: Some(30),
                ..Default::default()
            },
            ..Default::default()
        };
        let (c, l, m) = perso.clone().for_ask(&proj, None, None);
        assert_eq!(
            l,
            AskLimits {
                max_context_chars: 2000,
                max_seeds: 8,
                max_nodes: 30
            }
        );
        assert_eq!((c.timeout_secs, c.format_retries), (600, 3));
        assert_eq!(
            m,
            vec![
                "ask.max_context_chars",
                "ask.max_seeds",
                "ask.max_nodes",
                "timeout_secs",
                "format_retries"
            ]
        );
        // Options : --context-chars l'emporte et sort de la marque, --format-retries aussi.
        let (c, l, m) = perso.for_ask(&proj, Some(1), Some(20_000));
        assert_eq!(l.max_context_chars, 20_000);
        assert_eq!(c.format_retries, 1);
        assert_eq!(m, vec!["ask.max_seeds", "ask.max_nodes", "timeout_secs"]);
    }

    #[test]
    fn req162_ac3_enrichissement_egal_a_with_project() {
        let p = ProjectSettings {
            model: Some("m-projet".into()),
            concurrency: Some(3),
            ask: AskOverrides {
                model: Some("m-ask".into()),
                max_nodes: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let without_ask = ProjectSettings {
            ask: AskOverrides::default(),
            ..p.clone()
        };
        let b = with_ask(AskOverrides {
            thinking: Some(true),
            ..Default::default()
        });
        let (enrich, from) = b.clone().with_project(&p, None, Some(0));
        let (reference, from_ref) = Config {
            ask: AskOverrides::default(),
            ..b.clone()
        }
        .with_project(&without_ask, None, Some(0));
        assert_eq!(enrich.model, reference.model);
        assert_eq!(enrich.thinking, reference.thinking);
        assert_eq!(from, from_ref);
        assert_eq!(enrich.model, "m-projet");
        // `for_ask` partage le reste avec `with_project`.
        let (ask, _, _) = b.for_ask(&p, None, None);
        assert_eq!(ask.concurrency, 3);
        assert_eq!(ask.model, "m-ask");
    }
}

#[cfg(test)]
mod without_key_tests {
    use super::*;

    fn with_config(text: &str, check: impl FnOnce(&dyn Fn(&str) -> Option<String>)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        let p = path.to_string_lossy().to_string();
        let env = move |k: &str| (k == "NEPHILA_CONFIG").then(|| p.clone());
        check(&env);
    }

    /// REQ-184 : la variable de `api_key_env` absente ne fait pas échouer la variante sans clé,
    /// alors que `load_optional_with` échoue.
    #[test]
    fn req184_load_optional_without_key_variable_absente_succes() {
        with_config(
            "provider = \"openai\"\nmodel = \"m\"\napi_key_env = \"NEPHILA_ABSENTE_X\"\n",
            |env| {
                let c = Config::load_optional_without_key_with(env)
                    .unwrap()
                    .unwrap();
                assert_eq!(c.model, "m");
                assert!(Config::load_optional_with(env).is_err());
            },
        );
    }

    /// REQ-184 : `api_key_env` manquant pour `anthropic` donne la même erreur qu'avant.
    #[test]
    fn req184_load_optional_without_key_anthropic_sans_api_key_env() {
        with_config("provider = \"anthropic\"\nmodel = \"m\"\n", |env| {
            let a = Config::load_optional_without_key_with(env)
                .unwrap_err()
                .to_string();
            let b = Config::load_optional_with(env).unwrap_err().to_string();
            assert_eq!(a, b);
            assert!(a.contains("api_key_env"), "{a}");
        });
    }

    #[test]
    fn req184_load_optional_without_key_sans_fichier() {
        let env = |k: &str| (k == "NEPHILA_CONFIG").then(|| "/nulle/part/c.toml".to_string());
        assert!(Config::load_optional_without_key_with(&env)
            .unwrap()
            .is_none());
    }
}
