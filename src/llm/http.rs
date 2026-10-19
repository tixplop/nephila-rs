//! Outils HTTP communs aux adaptateurs (REQ-002) : erreurs, JSON de la réponse texte.

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::error::{Error, Result};

/// Réglages TLS d'un fournisseur (REQ-036).
#[derive(Debug, Clone, Default)]
pub struct Tls {
    /// Autorités ajoutées aux autorités publiques embarquées (DER).
    pub ca_certs: Vec<CertificateDer<'static>>,
    /// Désactive tout contrôle du certificat du serveur (chaîne, dates, nom).
    pub insecure: bool,
}

/// Vérificateur qui accepte tout certificat ; les signatures de la poignée de main
/// restent vérifiées par le fournisseur cryptographique (la connexion reste chiffrée).
#[derive(Debug)]
struct AcceptAnyCert(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// Configuration rustls : racines Mozilla embarquées + `ca_certs`, ou vérificateur permissif.
fn client_config(tls: &Tls) -> Arc<rustls::ClientConfig> {
    let provider: Arc<CryptoProvider> = Arc::new(ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("versions de protocole par défaut valides");
    let config = if tls.insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(
                provider.signature_verification_algorithms,
            )))
            .with_no_client_auth()
    } else {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for cert in &tls.ca_certs {
            // Invariant : la configuration valide chaque certificat au chargement.
            roots
                .add(cert.clone())
                .expect("autorité tls_ca_cert validée au chargement de la configuration");
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    Arc::new(config)
}

/// Agent HTTP ; `timeout_secs` borne la durée totale d'une requête.
pub fn agent(timeout_secs: u64, tls: &Tls) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .tls_config(client_config(tls))
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
        Err(e) => match certificate_error(&e) {
            Some(cause) => Err(Error::Network(format!(
                "certificat du serveur refusé par {provider} sur {url} : {cause} ; \
                 ajoutez l'autorité avec tls_ca_cert, ou désactivez le contrôle avec tls_insecure = true"
            ))),
            None => Err(Error::Network(format!(
                "erreur réseau avec {provider} sur {url} : {e}"
            ))),
        },
    }
}

/// Cause rustls si le transport a échoué sur le certificat du serveur.
fn certificate_error(e: &ureq::Error) -> Option<rustls::Error> {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cause {
        if let Some(r) = c.downcast_ref::<rustls::Error>() {
            if matches!(r, rustls::Error::InvalidCertificate(_)) {
                return Some(r.clone());
            }
        }
        if let Some(io) = c.downcast_ref::<std::io::Error>() {
            if let Some(r) = io.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
                if matches!(r, rustls::Error::InvalidCertificate(_)) {
                    return Some(r.clone());
                }
            }
        }
        cause = c.source();
    }
    None
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
        _ => Err(Error::Format(
            "réponse LLM non conforme : aucun objet JSON dans le texte".into(),
        )),
    }
}

/// Lit `body` en JSON, avec un message d'erreur d'appel (ni clé ni réseau).
pub fn parse_body(provider: &str, body: &str) -> Result<serde_json::Value> {
    serde_json::from_str(body)
        .map_err(|e| Error::Format(format!("réponse de {provider} illisible : {e}")))
}
