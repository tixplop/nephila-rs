//! Outils HTTP communs aux adaptateurs (REQ-002) : erreurs, JSON de la réponse texte.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Nombre maximal de nouveaux essais HTTP d'un appel (REQ-052).
const MAX_RETRIES: u32 = 4;
/// Attente maximale entre deux envois calculée par l'attente progressive.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Au-delà de cette valeur de `retry-after` (secondes), aucun nouvel essai.
const MAX_RETRY_AFTER_SECS: u64 = 60;
/// Variable de test qui remplace la base d'attente de 1 s (millisecondes).
pub const RETRY_BASE_ENV: &str = "NEPHILA_RETRY_BASE_MS";

/// Réglage des nouveaux essais HTTP d'un fournisseur (REQ-052, DEC-064).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    base: Duration,
}

impl Default for Retry {
    fn default() -> Retry {
        Retry {
            base: Duration::from_secs(1),
        }
    }
}

impl Retry {
    /// Base d'attente en millisecondes.
    pub fn from_millis(ms: u64) -> Retry {
        Retry {
            base: Duration::from_millis(ms),
        }
    }

    /// Base lue de `NEPHILA_RETRY_BASE_MS` (absente ou vide : 1 s) ; valeur mal formée : erreur.
    pub fn from_env() -> Result<Retry> {
        Retry::from_value(std::env::var(RETRY_BASE_ENV).ok().as_deref())
    }

    fn from_value(value: Option<&str>) -> Result<Retry> {
        match value.filter(|v| !v.is_empty()) {
            None => Ok(Retry::default()),
            Some(v) => v
                .trim()
                .parse()
                .map(Retry::from_millis)
                .map_err(|_| Error::bad_integer_env(RETRY_BASE_ENV, v)),
        }
    }
}

/// Attente avant le nouvel essai `k` (1 à 4), ou `None` si le serveur demande d'attendre plus de
/// 60 s. `retry_after` : valeur brute de l'en-tête ; seule la forme en secondes entières compte.
/// `alea` dans [0, 1] positionne l'attente dans [d/2, d], d = min(30 s, base × 2^(k−1)).
pub fn retry_delay(
    k: u32,
    base: Duration,
    retry_after: Option<&str>,
    alea: f64,
) -> Option<Duration> {
    if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
        return (secs <= MAX_RETRY_AFTER_SECS).then(|| Duration::from_secs(secs));
    }
    let factor = 1u32.checked_shl(k.saturating_sub(1)).unwrap_or(u32::MAX);
    let d = base.saturating_mul(factor).min(MAX_BACKOFF);
    let half = d / 2;
    Some(half + half.mul_f64(alea.clamp(0.0, 1.0)))
}

/// Gigue dans [0, 1[ : `RandomState` de la bibliothèque standard haché avec un compteur.
fn jitter() -> f64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// Statuts HTTP retentés (REQ-052).
fn is_retryable_status(code: u16) -> bool {
    matches!(code, 429 | 500 | 502 | 503 | 504 | 529)
}

/// Erreur de transport transitoire : ni statut HTTP, ni délai dépassé, ni refus de certificat,
/// ni adresse invalide, ni hôte introuvable.
fn is_transient(e: &ureq::Error) -> bool {
    use ureq::ErrorKind::{BadStatus, ConnectionFailed, Io};
    matches!(e, ureq::Error::Transport(_))
        && matches!(e.kind(), ConnectionFailed | Io | BadStatus)
        && !is_timeout(e)
        && certificate_error(e).is_none()
}

/// Résultat d'un envoi : définitif, ou à retenter avec l'erreur à rendre si les essais s'épuisent.
enum Attempt {
    Done(Result<String>),
    Retry {
        error: Error,
        retry_after: Option<String>,
    },
}

/// Exécute la requête, avec jusqu'à 4 nouveaux essais à l'identique (statuts 429, 500, 502, 503,
/// 504, 529 et erreurs réseau transitoires, attente progressive) ; 401/403 -> `InvalidKey`,
/// autres échecs -> `Network`. Retourne le corps de la réponse.
pub fn send(
    provider: &str,
    req: ureq::Request,
    body: Option<&serde_json::Value>,
    retry: &Retry,
) -> Result<String> {
    let mut k = 0;
    loop {
        match attempt(provider, req.clone(), body) {
            Attempt::Done(r) => return r,
            Attempt::Retry { error, retry_after } => {
                k += 1;
                if k > MAX_RETRIES {
                    return Err(error);
                }
                match retry_delay(k, retry.base, retry_after.as_deref(), jitter()) {
                    Some(d) => std::thread::sleep(d),
                    None => return Err(error),
                }
            }
        }
    }
}

/// Un envoi de la requête.
fn attempt(provider: &str, req: ureq::Request, body: Option<&serde_json::Value>) -> Attempt {
    let url = req.url().to_string();
    let result = match body {
        Some(b) => req
            .set("content-type", "application/json")
            .send_string(&b.to_string()),
        None => req.call(),
    };
    match result {
        Ok(resp) => match resp.into_string() {
            Ok(text) => Attempt::Done(Ok(text)),
            Err(e) => match e.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                    Attempt::Done(Err(Error::Timeout(format!(
                        "délai dépassé avec {provider} sur {url} ; augmentez timeout_secs"
                    ))))
                }
                kind => {
                    let error = Error::Network(format!("{provider} : lecture de la réponse : {e}"));
                    if matches!(
                        kind,
                        std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                    ) {
                        Attempt::Retry {
                            error,
                            retry_after: None,
                        }
                    } else {
                        Attempt::Done(Err(error))
                    }
                }
            },
        },
        Err(ureq::Error::Status(code @ (401 | 403), _)) => Attempt::Done(Err(Error::InvalidKey(format!(
            "clé d'API refusée par {provider} (HTTP {code}) ; vérifiez la variable désignée par api_key_env"
        )))),
        Err(ureq::Error::Status(code, resp)) => {
            let error = Error::Network(format!("erreur de {provider} (HTTP {code}) sur {url}"));
            if is_retryable_status(code) {
                Attempt::Retry {
                    error,
                    retry_after: resp.header("retry-after").map(String::from),
                }
            } else {
                Attempt::Done(Err(error))
            }
        }
        Err(e) if is_timeout(&e) => Attempt::Done(Err(Error::Timeout(format!(
            "délai dépassé avec {provider} sur {url} ({e}) ; augmentez timeout_secs"
        )))),
        Err(e) => match certificate_error(&e) {
            Some(cause) => Attempt::Done(Err(Error::Network(format!(
                "certificat du serveur refusé par {provider} sur {url} : {cause} ; \
                 ajoutez l'autorité avec tls_ca_cert, ou désactivez le contrôle avec tls_insecure = true"
            )))),
            None => {
                let error = Error::Network(format!("erreur réseau avec {provider} sur {url} : {e}"));
                if is_transient(&e) {
                    Attempt::Retry {
                        error,
                        retry_after: None,
                    }
                } else {
                    Attempt::Done(Err(error))
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn req052_ac5_delay_is_within_half_and_full_of_exponential_base() {
        for (k, d) in [(1, 1), (2, 2), (3, 4), (4, 8)] {
            let d = Duration::from_secs(d);
            for alea in [0.0, 0.3, 0.999, 1.0] {
                let w = retry_delay(k, S, None, alea).unwrap();
                assert!(w >= d / 2 && w <= d, "k={k} alea={alea} w={w:?}");
            }
        }
    }

    #[test]
    fn req052_ac5_delay_is_capped_at_30_seconds() {
        let base = Duration::from_secs(20);
        let w = retry_delay(4, base, None, 1.0).unwrap();
        assert_eq!(w, Duration::from_secs(30));
        let w = retry_delay(4, base, None, 0.0).unwrap();
        assert_eq!(w, Duration::from_secs(15));
    }

    #[test]
    fn req052_ac5_different_sources_give_different_delays() {
        let differs = (1..=4).any(|k| retry_delay(k, S, None, 0.1) != retry_delay(k, S, None, 0.9));
        assert!(differs);
        // La vraie source varie d'un appel à l'autre.
        let draws: Vec<f64> = (0..8).map(|_| jitter()).collect();
        assert!(draws.iter().all(|a| (0.0..1.0).contains(a)));
        assert!(draws.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn req052_ac5_retry_after_seconds_is_respected() {
        assert_eq!(
            retry_delay(1, S, Some("7"), 0.5),
            Some(Duration::from_secs(7))
        );
        assert_eq!(retry_delay(3, S, Some("0"), 0.5), Some(Duration::ZERO));
        assert_eq!(
            retry_delay(1, S, Some("60"), 0.5),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn req052_ac5_retry_after_above_60_gives_no_retry() {
        assert_eq!(retry_delay(1, S, Some("61"), 0.5), None);
        assert_eq!(retry_delay(1, S, Some("120"), 0.5), None);
    }

    #[test]
    fn req052_ac5_http_date_or_garbage_falls_back_to_computed_delay() {
        for v in ["Wed, 21 Oct 2026 07:28:00 GMT", "bientôt", "-3", "1.5", ""] {
            let w = retry_delay(2, S, Some(v), 1.0).unwrap();
            assert_eq!(w, Duration::from_secs(2), "{v}");
        }
    }

    #[test]
    fn req052_ac5_zero_base_means_no_wait() {
        assert_eq!(
            retry_delay(4, Duration::ZERO, None, 0.7),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn req052_ac5_base_variable_must_be_an_integer() {
        let e = Retry::from_value(Some("abc")).unwrap_err();
        assert_eq!(
            e.to_string(),
            "NEPHILA_RETRY_BASE_MS : entier attendu, reçu « abc »"
        );
        for bad in ["-1", "1.5"] {
            assert!(Retry::from_value(Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(
            Retry::from_value(Some("10")).unwrap(),
            Retry::from_millis(10)
        );
        assert_eq!(Retry::from_value(Some("0")).unwrap(), Retry::from_millis(0));
        assert_eq!(Retry::from_value(None).unwrap(), Retry::default());
    }

    fn io(kind: std::io::ErrorKind) -> ureq::Error {
        ureq::Error::from(std::io::Error::from(kind))
    }

    fn real(url: &str) -> ureq::Error {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .resolver(|addr: &str| -> std::io::Result<Vec<std::net::SocketAddr>> {
                use std::net::ToSocketAddrs;
                if addr.starts_with("127.0.0.1:") {
                    addr.to_socket_addrs().map(|i| i.collect())
                } else {
                    Err(std::io::Error::other("résolution simulée en échec"))
                }
            })
            .build()
            .get(url)
            .call()
            .unwrap_err()
    }

    #[test]
    fn req052_ac6_reset_closed_eof_and_broken_pipe_are_transient() {
        use std::io::ErrorKind::*;
        for k in [
            ConnectionReset,
            ConnectionAborted,
            UnexpectedEof,
            BrokenPipe,
        ] {
            assert!(is_transient(&io(k)), "{k:?}");
        }
    }

    #[test]
    fn req052_ac6_refused_connection_is_transient() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = real(&format!("http://127.0.0.1:{port}/"));
        assert!(is_transient(&e), "{e:?}");
    }

    #[test]
    fn req052_ac6_timeout_is_not_transient() {
        assert!(!is_transient(&io(std::io::ErrorKind::TimedOut)));
        assert!(!is_transient(&io(std::io::ErrorKind::WouldBlock)));
    }

    #[test]
    fn req052_ac6_certificate_refusal_is_not_transient() {
        let cert = rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        let e = ureq::Error::from(std::io::Error::new(std::io::ErrorKind::InvalidData, cert));
        assert!(!is_transient(&e));
    }

    #[test]
    fn req052_ac6_invalid_address_and_unknown_host_are_not_transient() {
        let e = real("http://exemple invalide/");
        assert!(!is_transient(&e), "{e:?}");
        let e = real("http://hote-introuvable.invalid/");
        assert!(!is_transient(&e), "{e:?}");
    }

    #[test]
    fn req052_ac6_statuses_are_never_transport_errors() {
        for c in [429, 500, 502, 503, 504, 529] {
            assert!(is_retryable_status(c));
        }
        for c in [200, 400, 401, 403, 404, 413, 501] {
            assert!(!is_retryable_status(c));
        }
    }
}
