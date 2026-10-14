//! Outils HTTP communs aux adaptateurs (REQ-002) : erreurs, JSON de la réponse texte.

use std::time::Duration;

use crate::error::{Error, Result};

/// Agent HTTP ; `timeout_secs` borne la durée totale d'une requête.
pub fn agent(timeout_secs: u64) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout_secs))
        .build()
}

/// `base_url` (ou défaut) sans barre finale, suivi de `path`.
pub fn url(base: &Option<String>, default: &str, path: &str) -> String {
    let b = base.as_deref().unwrap_or(default).trim_end_matches('/');
    format!("{b}{path}")
}

/// Exécute la requête ; 401/403 -> `InvalidKey`, autres échecs -> `Network`.
/// Retourne le corps de la réponse.
pub fn send(
    provider: &str,
    req: ureq::Request,
    body: Option<&serde_json::Value>,
) -> Result<String> {
    let url = req.url().to_string();
    let result = match body {
        Some(b) => req
            .set("content-type", "application/json")
            .send_string(&b.to_string()),
        None => req.call(),
    };
    match result {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Error::Timeout(
                    format!("délai dépassé avec {provider} sur {url} ; augmentez timeout_secs"),
                ),
                _ => Error::Network(format!("{provider} : lecture de la réponse : {e}")),
            }),
        Err(ureq::Error::Status(code @ (401 | 403), _)) => Err(Error::InvalidKey(format!(
            "clé d'API refusée par {provider} (HTTP {code}) ; vérifiez la variable désignée par api_key_env"
        ))),
        Err(ureq::Error::Status(code, _)) => Err(Error::Network(format!(
            "erreur de {provider} (HTTP {code}) sur {url}"
        ))),
        Err(e) if is_timeout(&e) => Err(Error::Timeout(format!(
            "délai dépassé avec {provider} sur {url} ({e}) ; augmentez timeout_secs"
        ))),
        Err(e) => Err(Error::Network(format!(
            "erreur réseau avec {provider} sur {url} : {e}"
        ))),
    }
}

/// Vrai si une erreur de transport a pour cause un délai (E/S expirée).
fn is_timeout(e: &ureq::Error) -> bool {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cause {
        if let Some(io) = c.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            );
        }
        cause = c.source();
    }
    false
}

/// Isole l'objet JSON d'un texte de modèle (clôtures ``` ou texte autour tolérés).
pub fn extract_json(text: &str) -> Result<&str> {
    match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if a < b => Ok(&text[a..=b]),
        _ => Err(Error::Message(
            "réponse LLM non conforme : aucun objet JSON dans le texte".into(),
        )),
    }
}

/// Lit `body` en JSON, avec un message d'erreur d'appel (ni clé ni réseau).
pub fn parse_body(provider: &str, body: &str) -> Result<serde_json::Value> {
    serde_json::from_str(body)
        .map_err(|e| Error::Message(format!("réponse de {provider} illisible : {e}")))
}
