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
pub const DEFAULT_BATCH_FILES: usize = 1;

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
