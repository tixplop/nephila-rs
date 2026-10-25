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

/// Agent HTTP ; `timeout_secs` borne la durée totale d'une requête. Il garde au moins
/// `concurrency` connexions inactives par hôte (et au moins 100 au total), pour que chaque fil
/// d'une commande réutilise une connexion établie (REQ-053).
pub fn agent(timeout_secs: u64, concurrency: usize, tls: &Tls) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .max_idle_connections_per_host(concurrency.max(1))
        .max_idle_connections(concurrency.max(100))
        .tls_config(client_config(tls))
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout_secs))
        // Aucune redirection suivie (REQ-092, DEC-100) : ni requête ni clé vers une autre adresse.
        .redirects(0)
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

/// Cause d'un nouvel essai après une erreur de transport, dans la trace d'attente.
const NETWORK_CAUSE: &str = "erreur réseau";

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
        /// Cause dite dans la trace d'attente : `HTTP <code>` ou `erreur réseau`.
        cause: String,
    },
}

/// Exécute la requête, avec jusqu'à 4 nouveaux essais à l'identique (statuts 429, 500, 502, 503,
/// 504, 529 et erreurs réseau transitoires, attente progressive) ; 401/403 -> `InvalidKey`,
/// 400/404/413/422 -> `Rejected` (jamais retentée), autres échecs -> `Network`. Retourne le corps de la réponse. `label` nomme l'appel dans la
/// trace d'attente (`<chemin> [i/n]`) ; `None` (`query ask`) : aucune ligne d'attente.
pub fn send(
    provider: &str,
    req: ureq::Request,
    body: Option<&serde_json::Value>,
    retry: &Retry,
    label: Option<&str>,
) -> Result<String> {
    let mut k = 0;
    loop {
        match attempt(provider, req.clone(), body) {
            Attempt::Done(r) => return r,
            Attempt::Retry {
                error,
                retry_after,
                cause,
            } => {
                k += 1;
                if k > MAX_RETRIES {
                    return Err(error);
                }
                match retry_delay(k, retry.base, retry_after.as_deref(), jitter()) {
                    Some(d) => {
                        if let Some(label) = label {
                            crate::trace!(
                                "llm enrich attente {label} {} ms après {cause} (nouvel essai {k}/{MAX_RETRIES})",
                                d.as_millis()
                            );
                        }
                        std::thread::sleep(d)
                    }
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
        Ok(resp) if (300..400).contains(&resp.status()) => {
            // Corps non lu : la connexion n'est pas réutilisée, rien n'est suivi (E-20).
            let location = resp.header("location").map(provider_text);
            Attempt::Done(Err(redirected(
                provider,
                resp.status(),
                location.as_deref(),
            )))
        }
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
                            cause: NETWORK_CAUSE.into(),
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
        Err(ureq::Error::Status(code @ (400 | 404 | 413 | 422), resp)) => {
            // Corps lu une fois ; une lecture qui échoue donne un message sans partie `error.message`.
            let detail = resp
                .into_string()
                .ok()
                .and_then(|body| provider_error_message(&body));
            Attempt::Done(Err(rejected(provider, code, &url, detail.as_deref())))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let error = Error::Network(format!("erreur de {provider} (HTTP {code}) sur {url}"));
            if is_retryable_status(code) {
                Attempt::Retry {
                    error,
                    retry_after: resp.header("retry-after").map(String::from),
                    cause: format!("HTTP {code}"),
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
                        cause: NETWORK_CAUSE.into(),
                    }
                } else {
                    Attempt::Done(Err(error))
                }
            }
        },
    }
}

/// Indice de fin du texte d'une requête refusée (E-19) : seule source.
const REJECTED_HINT: &str = "vérifiez model, thinking, effort et max_output_tokens";

/// Longueur maximale d'un texte recopié du fournisseur (`truncate.provider`).
const PROVIDER_TEXT_CHARS: usize = 200;

/// Texte recopié du fournisseur (`error.message`, `Location`) : une ligne (caractères de contrôle
/// remplacés par une espace, espaces répétées réduites, bords rognés), puis 200 caractères
/// Unicode au plus, `…` final compris. Seule fonction de `truncate.provider`.
pub fn provider_text(raw: &str) -> String {
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= PROVIDER_TEXT_CHARS {
        return line;
    }
    let cut: String = line.chars().take(PROVIDER_TEXT_CHARS - 1).collect();
    let mut out = cut.trim_end().to_string();
    out.push('…');
    out
}

/// `error.message` du corps JSON d'une réponse d'erreur, normalisé ; `None` si le corps n'est pas
/// du JSON, n'a pas de chaîne `error.message`, ou si elle est vide après normalisation.
fn provider_error_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = provider_text(value.get("error")?.get("message")?.as_str()?);
    (!text.is_empty()).then_some(text)
}

/// Requête refusée (E-19) : texte complet et détail fixe de la trace.
fn rejected(provider: &str, code: u16, url: &str, detail: Option<&str>) -> Error {
    let head = format!("requête refusée par {provider} (HTTP {code})");
    let message = match detail {
        Some(d) => format!("{head} sur {url} : {d} ; {REJECTED_HINT}"),
        None => format!("{head} sur {url} ; {REJECTED_HINT}"),
    };
    Error::Rejected {
        message,
        trace: head,
    }
}

/// Redirection (E-20) : texte complet et détail fixe de la trace (sans `Location`).
fn redirected(provider: &str, code: u16, location: Option<&str>) -> Error {
    let head = format!("{provider} redirige (HTTP {code})");
    let dest = match location {
        Some(l) if !l.is_empty() => format!("vers {l}"),
        _ => "sans destination".to_string(),
    };
    Error::Redirected {
        message: format!("{head} {dest} ; corrigez base_url"),
        trace: head,
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

    #[test]
    fn req091_ac4_long_message_is_one_line_of_200_chars_ending_with_ellipsis() {
        let raw = format!("{}\n\t{}", "a".repeat(150), "b".repeat(150));
        let t = provider_text(&raw);
        assert_eq!(t.chars().count(), 200);
        assert!(t.ends_with('…'));
        assert!(!t.chars().any(char::is_control));
        let body = serde_json::json!({"error": {"message": raw}}).to_string();
        let m = provider_error_message(&body).unwrap();
        assert_eq!(m, t);
    }

    #[test]
    fn req091_ac4_message_of_exactly_200_chars_is_kept() {
        let raw = "é".repeat(200);
        assert_eq!(provider_text(&raw), raw);
        let raw = format!("  {}  ", "é".repeat(200));
        assert_eq!(provider_text(&raw), "é".repeat(200));
        assert_eq!(provider_text(&"é".repeat(201)).chars().count(), 200);
        assert_eq!(provider_text("a \u{1} \r\n  b"), "a b");
        // Coupure juste après une espace : pas d'espace avant `…`.
        let raw = format!("{} {}", "a".repeat(198), "b".repeat(10));
        assert_eq!(provider_text(&raw), format!("{}…", "a".repeat(198)));
    }

    #[test]
    fn req091_ac4_unusable_bodies_give_no_message_part() {
        for body in [
            "",
            "pas du json",
            r#"{"error":"x"}"#,
            r#"{"error":{"message":42}}"#,
            r#"{"error":{"message":" \n\t "}}"#,
            r#"{"error":{}}"#,
        ] {
            assert_eq!(provider_error_message(body), None, "{body}");
            let e = rejected("openai", 400, "http://h/p", None);
            let text = e.to_string();
            assert_eq!(
                text,
                "requête refusée par openai (HTTP 400) sur http://h/p ; vérifiez model, thinking, effort et max_output_tokens"
            );
            assert!(!text.contains('\n'));
        }
        let e = rejected("anthropic", 422, "http://h/p", Some("model: x"));
        assert_eq!(
            e.to_string(),
            "requête refusée par anthropic (HTTP 422) sur http://h/p : model: x ; vérifiez model, thinking, effort et max_output_tokens"
        );
        match e {
            Error::Rejected { trace, .. } => {
                assert_eq!(trace, "requête refusée par anthropic (HTTP 422)")
            }
            other => panic!("{other:?}"),
        }
    }

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

    /// Serveur keep-alive local : compte les connexions acceptées ; les 4 premières requêtes
    /// ne reçoivent leur réponse que toutes ensemble (4 connexions simultanées nécessaires).
    mod pool {
        use super::*;
        use rustls::pki_types::pem::PemObject;
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Barrier, Condvar, Mutex};

        const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tls/");
        const THREADS: usize = 4;

        fn serve<S: Read + Write>(mut stream: S, arrived: &(Mutex<usize>, Condvar)) {
            loop {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                let head = String::from_utf8_lossy(&head).to_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                if stream.read_exact(&mut body).is_err() {
                    return;
                }
                {
                    let (count, cv) = arrived;
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    cv.notify_all();
                    while *n < THREADS {
                        n = cv.wait(n).unwrap();
                    }
                }
                let reply = "{}";
                let out = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                    reply.len()
                );
                if stream.write_all(out.as_bytes()).is_err() || stream.flush().is_err() {
                    return;
                }
            }
        }

        fn server(tls: bool) -> (String, Arc<AtomicUsize>) {
            use rustls::pki_types::PrivateKeyDer;
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let conns = Arc::new(AtomicUsize::new(0));
            let cfg = tls.then(|| {
                let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(
                    &std::fs::read(format!("{FIXTURES}server-ip.pem")).unwrap(),
                )
                .collect::<std::result::Result<_, _>>()
                .unwrap();
                let key = PrivateKeyDer::from_pem_slice(
                    &std::fs::read(format!("{FIXTURES}server-ip.key")).unwrap(),
                )
                .unwrap();
                Arc::new(
                    rustls::ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
                        .with_safe_default_protocol_versions()
                        .unwrap()
                        .with_no_client_auth()
                        .with_single_cert(certs, key)
                        .unwrap(),
                )
            });
            let counted = conns.clone();
            let arrived = Arc::new((Mutex::new(0usize), Condvar::new()));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let (cfg, counted, arrived) = (cfg.clone(), counted.clone(), arrived.clone());
                    std::thread::spawn(move || match cfg {
                        None => {
                            counted.fetch_add(1, Ordering::SeqCst);
                            serve(stream, &arrived)
                        }
                        Some(cfg) => {
                            let Ok(conn) = rustls::ServerConnection::new(cfg) else {
                                return;
                            };
                            let mut s: rustls::StreamOwned<_, TcpStream> =
                                rustls::StreamOwned::new(conn, stream);
                            while s.conn.is_handshaking() {
                                if s.conn.complete_io(&mut s.sock).is_err() {
                                    return;
                                }
                            }
                            counted.fetch_add(1, Ordering::SeqCst);
                            serve(s, &arrived)
                        }
                    });
                }
            });
            let scheme = if tls { "https" } else { "http" };
            (format!("{scheme}://{addr}/x"), conns)
        }

        /// `THREADS` fils : une requête, puis (une fois les réponses toutes lues) une seconde.
        fn connections_for(tls: bool) -> usize {
            let (url, conns) = server(tls);
            let settings = Tls {
                ca_certs: if tls {
                    CertificateDer::pem_slice_iter(
                        &std::fs::read(format!("{FIXTURES}ca.pem")).unwrap(),
                    )
                    .collect::<std::result::Result<_, _>>()
                    .unwrap()
                } else {
                    Vec::new()
                },
                insecure: false,
            };
            let agent = agent(10, THREADS, &settings);
            let barrier = Arc::new(Barrier::new(THREADS));
            let retry = Retry::from_millis(0);
            std::thread::scope(|scope| {
                for _ in 0..THREADS {
                    let (agent, url, barrier, retry) = (&agent, &url, &barrier, &retry);
                    scope.spawn(move || {
                        let body = serde_json::json!({});
                        for round in 0..2 {
                            let req = agent.post(url);
                            send("test", req, Some(&body), retry, None).unwrap();
                            if round == 0 {
                                barrier.wait();
                            }
                        }
                    });
                }
            });
            conns.load(Ordering::SeqCst)
        }

        #[test]
        fn req053_ac2_idle_pool_keeps_one_connection_per_thread() {
            assert_eq!(connections_for(false), THREADS);
        }

        #[test]
        fn req053_ac3_idle_pool_keeps_one_tls_connection_per_thread() {
            assert_eq!(connections_for(true), THREADS);
        }
    }
}
