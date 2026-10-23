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
pub const DEFAULT_BATCH_FILES: usize = 10;
pub const DEFAULT_FORMAT_RETRIES: u32 = 2;

/// Réglages que le fichier de projet peut porter, dans l'ordre d'affichage (REQ-042). Seule
/// déclaration de la liste : l'aide, l'erreur de clé inconnue et la trace s'en servent.
pub const PROJECT_SETTINGS: [&str; 6] = [
    "model",
    "max_chunk_chars",
    "concurrency",
    "timeout_secs",
    "batch_files",
    "format_retries",
];

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
    pub max_chunk_chars: Option<usize>,
    pub concurrency: Option<usize>,
    pub timeout_secs: Option<u64>,
    pub batch_files: Option<usize>,
    pub format_retries: Option<u32>,
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
        Ok(out)
    }
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
}

pub const DEFAULT_EFFORT: Effort = Effort::Low;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16384;

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
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
    /// Enrichit aussi les fichiers de code, pas seulement les documents (REQ-050).
    pub enrich_code: bool,
    /// Demande la réflexion au modèle (REQ-058).
    pub thinking: bool,
    /// Niveau de la réflexion, utilisé seulement avec `thinking` (REQ-058).
    pub effort: Effort,
    /// Limite de jetons de la réponse, réflexion comprise (REQ-058).
    pub max_output_tokens: u32,
    /// Chemin résolu du PEM d'autorités ajoutées aux autorités publiques (REQ-036).
    pub tls_ca_cert: Option<PathBuf>,
    /// Certificats d'autorité (DER) lus dans `tls_ca_cert`, déjà validés.
    pub tls_ca_certs: Vec<CertificateDer<'static>>,
    /// Désactive le contrôle du certificat du serveur (REQ-036).
    pub tls_insecure: bool,
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
    enrich_code: Option<toml::Value>,
    thinking: Option<toml::Value>,
    effort: Option<toml::Value>,
    max_output_tokens: Option<toml::Value>,
    tls_ca_cert: Option<String>,
    tls_insecure: Option<bool>,
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
        if let Some(v) = &p.model {
            self.model = v.clone();
            from_project.push("model");
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
        let Ok(path) = config_path_with(env) else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let cfg = Config::load_from_with(&path, env)?;
        cfg.api_key_with(env)?;
        Ok(Some(cfg))
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
            Some(v) => {
                return Err(invalid("effort", "\"low\", \"medium\" ou \"high\"", v));
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
            enrich_code,
            thinking,
            effort,
            max_output_tokens,
            tls_ca_cert,
            tls_ca_certs,
            tls_insecure: raw.tls_insecure.unwrap_or(false),
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
            enrich_code: false,
            thinking: false,
            effort: DEFAULT_EFFORT,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            tls_ca_cert: None,
            tls_ca_certs: Vec::new(),
            tls_insecure: true,
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
            .filter(|k| *k != "model")
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
                "\"low\", \"medium\" ou \"high\"",
                "\"max\"",
            ),
            (
                "effort = \"\"",
                "effort",
                "\"low\", \"medium\" ou \"high\"",
                "\"\"",
            ),
            (
                "effort = \"LOW\"",
                "effort",
                "\"low\", \"medium\" ou \"high\"",
                "\"LOW\"",
            ),
            (
                "effort = 1",
                "effort",
                "\"low\", \"medium\" ou \"high\"",
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
