//! Outils HTTP communs aux adaptateurs (REQ-002) : erreurs, JSON de la réponse texte.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};

use super::batch::CreateError;
use super::sse;
use super::usage::CallStats;
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

/// Agent HTTP ; `timeout_secs` est le délai maximal sans octet reçu (`timeout_read`) ni envoyé
/// (`timeout_write`) : une réponse qui continue d'arriver n'est pas coupée (REQ-250). Pas de
/// `.timeout(d)` : en ureq 2 il a priorité sur les délais de lecture et d'écriture et bornerait la
/// durée totale (DEC-185) ; le plafond d'un flux est vérifié par `sse::read_body`. Il garde au moins
/// `concurrency` connexions inactives par hôte (et au moins 100 au total), pour que chaque fil
/// d'une commande réutilise une connexion établie (REQ-053).
pub fn agent(timeout_secs: u64, concurrency: usize, tls: &Tls) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .max_idle_connections_per_host(concurrency.max(1))
        .max_idle_connections(concurrency.max(100))
        .tls_config(client_config(tls))
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(timeout_secs))
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
/// 60 s (REQ-253). Ordre de lecture : `retry_after_ms` (millisecondes, partie décimale ignorée),
/// `retry_after` en secondes entières, `retry_after` en IMF-fixdate (date − `now`, 0 si passée) ;
/// un en-tête illisible est ignoré, aucune forme lisible → attente calculée.
/// `alea` dans [0, 1] positionne l'attente calculée dans [d/2, d], d = min(30 s, base × 2^(k−1)).
pub fn retry_delay(
    k: u32,
    base: Duration,
    retry_after_ms: Option<&str>,
    retry_after: Option<&str>,
    now: SystemTime,
    alea: f64,
) -> Option<Duration> {
    if let Some(ms) = retry_after_ms.and_then(parse_millis) {
        return (ms <= MAX_RETRY_AFTER_SECS * 1000).then(|| Duration::from_millis(ms));
    }
    if let Some(v) = retry_after {
        let requested = match v.trim().parse::<u64>() {
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(_) => parse_http_date(v.trim()).map(|t| t.duration_since(now).unwrap_or_default()),
        };
        if let Some(d) = requested {
            return (d <= Duration::from_secs(MAX_RETRY_AFTER_SECS)).then_some(d);
        }
    }
    let factor = 1u32.checked_shl(k.saturating_sub(1)).unwrap_or(u32::MAX);
    let d = base.saturating_mul(factor).min(MAX_BACKOFF);
    let half = d / 2;
    Some(half + half.mul_f64(alea.clamp(0.0, 1.0)))
}

/// Nombre de millisecondes entier ou décimal ≥ 0 ; la partie décimale est ignorée.
fn parse_millis(v: &str) -> Option<u64> {
    let v = v.trim();
    let (int, frac) = v.split_once('.').unwrap_or((v, "0"));
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    (digits(int) && digits(frac)).then(|| int.parse().unwrap_or(u64::MAX))
}

/// Date HTTP au format IMF-fixdate (`Wed, 21 Oct 2015 07:28:00 GMT`) ; toute autre forme → `None`.
fn parse_http_date(v: &str) -> Option<SystemTime> {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = v.split(' ').collect();
    let [day_name, day, month, year, time, "GMT"] = parts[..] else {
        return None;
    };
    let day_name = day_name.strip_suffix(',')?;
    let weekday = DAYS.iter().position(|d| *d == day_name)? as i64;
    let month = MONTHS.iter().position(|m| *m == month)? as i64 + 1;
    let num = |t: &str, len: usize| -> Option<i64> {
        (t.len() == len && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.parse().ok())?
    };
    let (day, year) = (num(day, 2)?, num(year, 4)?);
    let t: Vec<&str> = time.split(':').collect();
    let [h, mi, se] = t[..] else {
        return None;
    };
    let (h, mi, se) = (num(h, 2)?, num(mi, 2)?, num(se, 2)?);
    if !(1..=31).contains(&day) || h > 23 || mi > 59 || se > 60 || year < 1970 {
        return None;
    }
    // Jours depuis 1970-01-01 (calendrier grégorien proleptique).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if (days + 3).rem_euclid(7) != weekday {
        return None;
    }
    let secs = u64::try_from(days * 86_400 + h * 3600 + mi * 60 + se).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
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
const INTERRUPTED_CAUSE: &str = "lecture interrompue";

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
        retry_after_ms: Option<String>,
        retry_after: Option<String>,
        /// Cause dite dans la trace d'attente : `HTTP <code>`, `erreur réseau` ou `lecture interrompue`.
        cause: String,
        /// Lecture interrompue (REQ-097) : un seul nouvel envoi, dont le résultat est définitif.
        last: bool,
    },
}

/// Exécute la requête, avec jusqu'à 4 nouveaux essais à l'identique (statuts 429, 500, 502, 503,
/// 504, 529 et erreurs réseau transitoires, attente progressive) ; 401/403 -> `InvalidKey`,
/// 400/404/413/422 -> `Rejected` (jamais retentée), autres échecs -> `Network`. Retourne le corps de la réponse. `label` nomme l'appel dans la
/// trace d'attente (`<chemin> [i/n]`) ; `None` (`query ask`) : aucune ligne d'attente. `stop` :
/// arrêt de l'envoi (REQ-095) ; posé, l'attente s'interrompt, aucun nouvel envoi n'a lieu et
/// l'erreur du dernier envoi est rendue. `stats` compte chaque envoi réel (REQ-165).
#[allow(clippy::too_many_arguments)]
pub fn send(
    provider: &str,
    req: ureq::Request,
    body: Option<&serde_json::Value>,
    retry: &Retry,
    label: Option<&str>,
    stop: Option<&AtomicBool>,
    stats: &CallStats,
    pause: &Pause,
) -> Result<String> {
    send_with(
        provider,
        req,
        body,
        retry,
        label,
        stop,
        stats,
        Some(pause),
        false,
    )
}

/// Comme `send` pour un `GET`, mais le corps est lu en entier : les résultats d'un lot du
/// fournisseur dépassent la limite de 10 Mo d'`into_string` (REQ-214).
pub fn send_all(
    provider: &str,
    req: ureq::Request,
    retry: &Retry,
    stop: &AtomicBool,
) -> Result<String> {
    send_with(
        provider,
        req,
        None,
        retry,
        None,
        Some(stop),
        &CallStats::default(),
        None,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn send_with(
    provider: &str,
    req: ureq::Request,
    body: Option<&serde_json::Value>,
    retry: &Retry,
    label: Option<&str>,
    stop: Option<&AtomicBool>,
    stats: &CallStats,
    pause: Option<&Pause>,
    unlimited: bool,
) -> Result<String> {
    let mut k = 0;
    let mut definitive = false;
    let mut last_error = None;
    loop {
        if pause.is_some_and(|p| p.attendre(stop)) {
            // REQ-254 : arrêt posé pendant la pause. Rien envoyé : erreur interne ; sinon l'erreur
            // du dernier envoi (REQ-095).
            return Err(last_error.unwrap_or(Error::Unsent));
        }
        // REQ-255 : une place d'envoi, rendue dès la réponse reçue ou l'échec.
        let place = match pause {
            Some(p) => match p.prendre(stop) {
                Some(place) => Some(place),
                None => return Err(last_error.unwrap_or(Error::Unsent)),
            },
            None => None,
        };
        stats.add_send();
        let result = attempt(provider, req.clone(), body, unlimited);
        match result {
            Attempt::Done(r) => {
                drop(place);
                if let (Some(p), Ok(_)) = (pause, &r) {
                    if let Some((new, old)) = p.reussir() {
                        if label.is_some() {
                            crate::trace!(
                                "limite d'envois: {new}/{}, remontée après {old} envois réussis",
                                p.concurrency()
                            );
                        }
                    }
                }
                return r;
            }
            Attempt::Retry { error, .. } if definitive => return Err(error),
            Attempt::Retry {
                error,
                retry_after_ms,
                retry_after,
                cause,
                last,
            } => {
                definitive = last;
                k += 1;
                if k > MAX_RETRIES {
                    return Err(error);
                }
                match retry_delay(
                    k,
                    retry.base,
                    retry_after_ms.as_deref(),
                    retry_after.as_deref(),
                    SystemTime::now(),
                    jitter(),
                ) {
                    Some(d) => {
                        if is_stopped(stop) {
                            return Err(error);
                        }
                        if let Some(label) = label {
                            crate::trace!(
                                "{} attente {label} {} ms après {cause} (nouvel essai {k}/{MAX_RETRIES}){}",
                                stats.word(),
                                d.as_millis(),
                                stats.mark()
                            );
                        }
                        if let (Some(pause), "HTTP 429" | "HTTP 529") = (pause, cause.as_str()) {
                            let now = Instant::now();
                            let (moved, lowered) = pause.poser_limite(now, now + d);
                            if let (true, Some(label)) = (moved, label) {
                                crate::trace!(
                                    "{} pause {label} : aucun envoi pendant {} ms après {cause}{}",
                                    stats.word(),
                                    d.as_millis(),
                                    stats.mark()
                                );
                                if let Some(l) = lowered {
                                    crate::trace!(
                                        "limite d'envois: {l}/{}, baissée par la pause",
                                        pause.concurrency()
                                    );
                                }
                            }
                        }
                        // La place est rendue après la pause posée : aucun fil en attente d'une
                        // place ne part pendant la pause.
                        drop(place);
                        if wait(d, stop) {
                            return Err(error);
                        }
                        last_error = Some(error);
                    }
                    None => return Err(error),
                }
            }
        }
    }
}

/// Création d'un lot du fournisseur : envoi unique, renvoyé seulement sur HTTP 429, 503 ou 529
/// (REQ-214) ; une réponse perdue ne se rejoue pas, le lot pourrait être payé deux fois.
pub fn send_create(
    provider: &str,
    req: ureq::Request,
    body: &str,
    content_type: &str,
    retry: &Retry,
    stop: &AtomicBool,
) -> std::result::Result<String, CreateError> {
    let url = req.url().to_string();
    let mut k = 0;
    loop {
        let result = req
            .clone()
            .set("content-type", content_type)
            .send_string(body);
        let failed = |cause: &str| CreateError::Failed {
            cause: cause.into(),
        };
        let (code, retry_after_ms, retry_after) = match result {
            Ok(resp) if (300..400).contains(&resp.status()) => {
                let location = resp.header("location").map(provider_text);
                return Err(CreateError::Other(redirected(
                    provider,
                    resp.status(),
                    location.as_deref(),
                )));
            }
            Ok(resp) => {
                return match read_text(resp, true) {
                    Ok(text) => Ok(text),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) =>
                    {
                        Err(failed("délai dépassé"))
                    }
                    Err(_) => Err(failed("réseau")),
                }
            }
            Err(ureq::Error::Status(code @ (401 | 403), _)) => {
                return Err(CreateError::Other(invalid_key(provider, code)))
            }
            Err(ureq::Error::Status(code @ (404 | 405 | 501), _)) => {
                return Err(CreateError::NoBatchMode { code, url })
            }
            Err(ureq::Error::Status(code @ (400 | 413 | 422), resp)) => {
                let detail = resp
                    .into_string()
                    .ok()
                    .and_then(|body| provider_error_message(&body));
                return Err(CreateError::Other(rejected(
                    provider,
                    code,
                    &url,
                    detail.as_deref(),
                )));
            }
            Err(ureq::Error::Status(code, resp)) => {
                if !matches!(code, 429 | 503 | 529) {
                    return Err(failed(&format!("HTTP {code}")));
                }
                (
                    code,
                    resp.header("retry-after-ms").map(String::from),
                    resp.header("retry-after").map(String::from),
                )
            }
            Err(e) if is_timeout(&e) => return Err(failed("délai dépassé")),
            Err(e) => {
                return Err(match certificate_error(&e) {
                    Some(cause) => CreateError::Other(certificate_refused(provider, &url, &cause)),
                    None => failed("réseau"),
                })
            }
        };
        k += 1;
        let delay = if k > MAX_RETRIES || stop.load(Ordering::SeqCst) {
            None
        } else {
            retry_delay(
                k,
                retry.base,
                retry_after_ms.as_deref(),
                retry_after.as_deref(),
                SystemTime::now(),
                jitter(),
            )
        };
        match delay {
            Some(d) if !wait(d, Some(stop)) => {}
            _ => return Err(failed(&format!("HTTP {code}"))),
        }
    }
}

/// Durée maximale d'une tranche d'attente (REQ-095).
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// Pause partagée et limite d'envois d'un adaptateur (REQ-254, REQ-255, DEC-186) : instant avant
/// lequel aucun envoi n'a lieu, posé après un HTTP 429 ou 529 suivi d'un nouvel essai ; nombre
/// d'envois simultanés permis, entre 1 et `concurrency`.
#[derive(Debug)]
pub struct Pause {
    state: Mutex<State>,
    freed: Condvar,
}

#[derive(Debug)]
struct State {
    end: Option<Instant>,
    concurrency: usize,
    limit: usize,
    taken: usize,
    successes: usize,
}

/// Place d'envoi prise ; rendue à la destruction, aussi sur erreur.
pub struct Place<'a>(&'a Pause);

impl Drop for Place<'_> {
    fn drop(&mut self) {
        self.0.lock().taken -= 1;
        self.0.freed.notify_all();
    }
}

impl Default for Pause {
    /// Sans limite pratique (tests de transport).
    fn default() -> Pause {
        Pause::new(usize::MAX)
    }
}

impl Pause {
    pub fn new(concurrency: usize) -> Pause {
        let concurrency = concurrency.max(1);
        Pause {
            state: Mutex::new(State {
                end: None,
                concurrency,
                limit: concurrency,
                taken: 0,
                successes: 0,
            }),
            freed: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn end(&self) -> Option<Instant> {
        self.lock().end
    }

    /// Limite d'envois en cours.
    #[cfg(test)]
    pub fn limit(&self) -> usize {
        self.lock().limit
    }

    pub fn concurrency(&self) -> usize {
        self.lock().concurrency
    }

    /// Garde la fin la plus tardive ; vrai si la fin a reculé (pause nouvelle ou prolongée).
    #[cfg(test)]
    pub fn poser(&self, fin: Instant) -> bool {
        self.poser_limite(Instant::now(), fin).0
    }

    /// Comme `poser`, à l'instant `now`; rend aussi la nouvelle limite si elle a baissé (pause
    /// qui commence : limite / 2, au moins 1).
    pub fn poser_limite(&self, now: Instant, fin: Instant) -> (bool, Option<usize>) {
        let mut st = self.lock();
        if st.end.is_some_and(|e| e >= fin) {
            return (false, None);
        }
        let started = st.end.is_none_or(|e| e <= now);
        st.end = Some(fin);
        if started {
            let lower = (st.limit / 2).max(1);
            if lower != st.limit {
                st.limit = lower;
                st.successes = 0;
                return (true, Some(lower));
            }
        }
        (true, None)
    }

    /// Compte une réussite ; rend `(nouvelle limite, limite d'avant)` si elle monte.
    pub fn reussir(&self) -> Option<(usize, usize)> {
        let mut st = self.lock();
        if st.limit >= st.concurrency {
            return None;
        }
        st.successes += 1;
        if st.successes < st.limit {
            return None;
        }
        let old = st.limit;
        st.limit += 1;
        st.successes = 0;
        drop(st);
        self.freed.notify_all();
        Some((old + 1, old))
    }

    /// Prend une place d'envoi, par tranches de `WAIT_SLICE`, l'arrêt lu avant chaque tranche ;
    /// `None` si l'arrêt est posé (aucune place prise).
    pub fn prendre(&self, stop: Option<&AtomicBool>) -> Option<Place<'_>> {
        let mut st = self.lock();
        loop {
            let rest = st.end.map_or(Duration::ZERO, |e| {
                e.saturating_duration_since(Instant::now())
            });
            if st.taken < st.limit && rest.is_zero() {
                st.taken += 1;
                return Some(Place(self));
            }
            if is_stopped(stop) {
                return None;
            }
            st = self
                .freed
                .wait_timeout(
                    st,
                    WAIT_SLICE.min(if rest.is_zero() { WAIT_SLICE } else { rest }),
                )
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Attend la fin de la pause par tranches de `WAIT_SLICE` ; vrai si l'arrêt est posé pendant
    /// la pause (aucun envoi ne doit suivre), l'arrêt étant lu avant chaque tranche. Sans pause en
    /// cours, rend faux aussitôt.
    pub fn attendre(&self, stop: Option<&AtomicBool>) -> bool {
        loop {
            let rest = self.end().map_or(Duration::ZERO, |e| {
                e.saturating_duration_since(Instant::now())
            });
            if rest.is_zero() {
                return false;
            }
            if is_stopped(stop) {
                return true;
            }
            std::thread::sleep(rest.min(WAIT_SLICE));
        }
    }
}

fn is_stopped(stop: Option<&AtomicBool>) -> bool {
    stop.is_some_and(|s| s.load(Ordering::SeqCst))
}

/// Attend `d` par tranches de `min(100 ms, reste)`, le drapeau d'arrêt relu avant chaque tranche
/// et à la fin (avant un nouvel envoi). Retourne `true` si l'attente a été interrompue par
/// l'arrêt (aucun nouvel envoi), `false` si elle est allée à son terme.
pub fn wait(d: Duration, stop: Option<&AtomicBool>) -> bool {
    let start = Instant::now();
    loop {
        if is_stopped(stop) {
            return true;
        }
        let rest = d.saturating_sub(start.elapsed());
        if rest.is_zero() {
            return false;
        }
        std::thread::sleep(rest.min(WAIT_SLICE));
    }
}

/// Clé d'API refusée (E-12) : texte unique.
fn invalid_key(provider: &str, code: u16) -> Error {
    Error::InvalidKey(format!(
        "clé d'API refusée par {provider} (HTTP {code}) ; vérifiez la variable désignée par api_key_env"
    ))
}

/// Certificat du serveur refusé : texte unique.
fn certificate_refused(provider: &str, url: &str, cause: &rustls::Error) -> Error {
    Error::Network(format!(
        "certificat du serveur refusé par {provider} sur {url} : {cause} ; \
         ajoutez l'autorité avec tls_ca_cert, ou désactivez le contrôle avec tls_insecure = true"
    ))
}

/// Corps de la réponse en texte : `into_string` s'arrête à 10 Mo, `into_reader` non.
fn read_text(resp: ureq::Response, unlimited: bool) -> std::io::Result<String> {
    if !unlimited {
        return resp.into_string();
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut resp.into_reader(), &mut text)?;
    Ok(text)
}

/// Plafond de durée de la lecture d'un flux (REQ-250).
const STREAM_CAP: Duration = Duration::from_secs(3600);

/// Issue d'une lecture de réponse qui échoue : délai, lecture interrompue (un seul nouvel envoi,
/// REQ-097) ou erreur réseau définitive.
fn read_failure(provider: &str, url: &str, e: &std::io::Error) -> Attempt {
    use std::io::ErrorKind::{
        BrokenPipe, ConnectionAborted, ConnectionReset, InvalidInput, TimedOut, UnexpectedEof,
        WouldBlock,
    };
    match e.kind() {
        TimedOut | WouldBlock => Attempt::Done(Err(Error::Timeout(format!(
            "délai dépassé avec {provider} sur {url} ; augmentez timeout_secs"
        )))),
        kind => {
            let error = Error::Network(format!("{provider} : lecture de la réponse : {e}"));
            // `InvalidInput` : le décodeur `chunked` d'ureq refuse un corps coupé en plein flux.
            if matches!(
                kind,
                UnexpectedEof | BrokenPipe | ConnectionReset | ConnectionAborted | InvalidInput
            ) {
                Attempt::Retry {
                    error,
                    retry_after_ms: None,
                    retry_after: None,
                    cause: INTERRUPTED_CAUSE.into(),
                    last: true,
                }
            } else {
                Attempt::Done(Err(error))
            }
        }
    }
}

/// Un envoi de la requête.
fn attempt(
    provider: &str,
    req: ureq::Request,
    body: Option<&serde_json::Value>,
    unlimited: bool,
) -> Attempt {
    let url = req.url().to_string();
    let started = Instant::now();
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
        Ok(resp) if !unlimited && resp.content_type().starts_with("text/event-stream") => {
            let reader = std::io::BufReader::new(resp.into_reader());
            match sse::read_body(reader, provider, STREAM_CAP, started) {
                Ok(text) => Attempt::Done(Ok(text)),
                Err(sse::SseError::Cap) => read_failure(
                    provider,
                    &url,
                    &std::io::Error::from(std::io::ErrorKind::TimedOut),
                ),
                Err(sse::SseError::Overloaded) => Attempt::Retry {
                    error: Error::Saturated(format!("erreur de {provider} (HTTP 529) sur {url}")),
                    retry_after_ms: None,
                    retry_after: None,
                    cause: "HTTP 529".into(),
                    last: false,
                },
                Err(sse::SseError::Api) => Attempt::Retry {
                    error: Error::Network(format!("erreur de {provider} (HTTP 500) sur {url}")),
                    retry_after_ms: None,
                    retry_after: None,
                    cause: "HTTP 500".into(),
                    last: false,
                },
                Err(sse::SseError::Io(e)) => read_failure(provider, &url, &e),
            }
        }
        Ok(resp) => match read_text(resp, unlimited) {
            Ok(text) => Attempt::Done(Ok(text)),
            Err(e) => read_failure(provider, &url, &e),
        },
        Err(ureq::Error::Status(code @ (401 | 403), _)) => {
            Attempt::Done(Err(invalid_key(provider, code)))
        }
        Err(ureq::Error::Status(code @ (400 | 404 | 413 | 422), resp)) => {
            // Corps lu une fois ; une lecture qui échoue donne un message sans partie `error.message`.
            let detail = resp
                .into_string()
                .ok()
                .and_then(|body| provider_error_message(&body));
            Attempt::Done(Err(rejected(provider, code, &url, detail.as_deref())))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let text = format!("erreur de {provider} (HTTP {code}) sur {url}");
            let error = if matches!(code, 429 | 529) {
                Error::Saturated(text)
            } else {
                Error::Network(text)
            };
            if is_retryable_status(code) {
                Attempt::Retry {
                    error,
                    retry_after_ms: resp.header("retry-after-ms").map(String::from),
                    retry_after: resp.header("retry-after").map(String::from),
                    cause: format!("HTTP {code}"),
                    last: false,
                }
            } else {
                Attempt::Done(Err(error))
            }
        }
        Err(e) if is_timeout(&e) => Attempt::Done(Err(Error::Timeout(format!(
            "délai dépassé avec {provider} sur {url} ({e}) ; augmentez timeout_secs"
        )))),
        Err(e) => match certificate_error(&e) {
            Some(cause) => Attempt::Done(Err(certificate_refused(provider, &url, &cause))),
            None => {
                let error =
                    Error::Network(format!("erreur réseau avec {provider} sur {url} : {e}"));
                if is_transient(&e) {
                    Attempt::Retry {
                        error,
                        retry_after_ms: None,
                        retry_after: None,
                        cause: NETWORK_CAUSE.into(),
                        last: false,
                    }
                } else {
                    Attempt::Done(Err(error))
                }
            }
        },
    }
}

/// Repli de `structured_output = "auto"` (REQ-203, DEC-163) : `true` si `e` est un HTTP 400 dont
/// le message du fournisseur cite `format` ou `schema` et que `structured` est `Auto` ; la trace
/// n'est écrite qu'au premier refus de la commande (`fallback`). L'appelant renvoie alors le corps
/// sans le schéma.
pub fn schema_refused(
    provider: &str,
    e: &Error,
    structured: super::Structured,
    fallback: &AtomicBool,
    key: &str,
) -> bool {
    let d = match e {
        Error::Rejected {
            code: 400,
            detail: Some(d),
            ..
        }
        | Error::BatchItem {
            refused: Some(d), ..
        } => d,
        _ => return false,
    };
    let d = d.to_ascii_lowercase();
    if structured != super::Structured::Auto || !(d.contains("format") || d.contains("schema")) {
        return false;
    }
    if !fallback.swap(true, Ordering::SeqCst) {
        // Écrite même pendant une passe de collecte (traces tues), sinon le repli se perdrait.
        crate::trace::emit_unmuted(format_args!(
            "sortie structurée: schéma JSON refusé par {provider} (HTTP 400), suite de la commande sans schéma ({key} = \"auto\")"
        ));
    }
    true
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
        code,
        detail: detail.map(String::from),
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

/// Du premier `{` au dernier `}` du texte (heuristique de dernier recours).
fn span(text: &str) -> Result<&str> {
    match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if a < b => Ok(&text[a..=b]),
        _ => Err(Error::Format(
            "réponse LLM non conforme : aucun objet JSON dans le texte".into(),
        )),
    }
}

/// Isole du texte d'un modèle l'objet JSON conforme (REQ-176, DEC-142).
///
/// À chaque `{`, de gauche à droite, une valeur JSON complète est lue (texte
/// éventuel après elle toléré) puis sautée en entier. Un objet portant `key`
/// au premier niveau est un candidat, validé par `parse` sur sa tranche exacte.
/// Résultat : le premier candidat validé si tous les candidats validés sont
/// égaux ; sinon (candidats validés différents) l'erreur de `parse` sur la
/// tranche du premier `{` au dernier `}` ; candidats tous refusés : l'erreur du
/// premier ; aucun candidat : `parse` sur cette même tranche.
pub fn parse_reply<T>(text: &str, key: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    let mut first_ok: Option<(serde_json::Value, T)> = None;
    let mut first_err: Option<Error> = None;
    let mut ambiguous = false;
    let mut pos = 0;
    while let Some(off) = text[pos..].find('{') {
        let a = pos + off;
        let mut stream =
            serde_json::Deserializer::from_str(&text[a..]).into_iter::<serde_json::Value>();
        match stream.next() {
            Some(Ok(value)) => {
                let end = a + stream.byte_offset();
                if value.as_object().is_some_and(|o| o.contains_key(key)) {
                    match parse(&text[a..end]) {
                        Ok(t) => match &first_ok {
                            None => first_ok = Some((value, t)),
                            Some((v, _)) if *v != value => ambiguous = true,
                            Some(_) => {}
                        },
                        Err(e) => {
                            first_err.get_or_insert(e);
                        }
                    }
                }
                pos = end;
            }
            _ => pos = a + 1,
        }
    }
    match (first_ok, first_err) {
        (Some((_, t)), _) if !ambiguous => Ok(t),
        (Some(_), _) => match parse(span(text)?) {
            Err(e) => Err(e),
            Ok(_) => Err(Error::Format(
                "réponse LLM non conforme : plusieurs objets JSON différents".into(),
            )),
        },
        (None, Some(e)) => Err(e),
        (None, None) => parse(span(text)?),
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
    fn req250_ac4_stream_cap_is_3600_seconds() {
        assert_eq!(STREAM_CAP, Duration::from_secs(3600));
    }

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

    /// Mercredi 21 octobre 2015, 07:28:00 GMT.
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_445_412_480)
    }

    fn delay(k: u32, base: Duration, after: Option<&str>, alea: f64) -> Option<Duration> {
        retry_delay(k, base, None, after, now(), alea)
    }

    fn full(ms: Option<&str>, after: Option<&str>) -> Option<Duration> {
        retry_delay(1, S, ms, after, now(), 1.0)
    }

    #[test]
    fn req052_ac5_delay_is_within_half_and_full_of_exponential_base() {
        for (k, d) in [(1, 1), (2, 2), (3, 4), (4, 8)] {
            let d = Duration::from_secs(d);
            for alea in [0.0, 0.3, 0.999, 1.0] {
                let w = delay(k, S, None, alea).unwrap();
                assert!(w >= d / 2 && w <= d, "k={k} alea={alea} w={w:?}");
            }
        }
    }

    #[test]
    fn req052_ac5_delay_is_capped_at_30_seconds() {
        let base = Duration::from_secs(20);
        let w = delay(4, base, None, 1.0).unwrap();
        assert_eq!(w, Duration::from_secs(30));
        let w = delay(4, base, None, 0.0).unwrap();
        assert_eq!(w, Duration::from_secs(15));
    }

    #[test]
    fn req052_ac5_different_sources_give_different_delays() {
        let differs = (1..=4).any(|k| delay(k, S, None, 0.1) != delay(k, S, None, 0.9));
        assert!(differs);
        // La vraie source varie d'un appel à l'autre.
        let draws: Vec<f64> = (0..8).map(|_| jitter()).collect();
        assert!(draws.iter().all(|a| (0.0..1.0).contains(a)));
        assert!(draws.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn req052_ac5_retry_after_seconds_is_respected() {
        assert_eq!(delay(1, S, Some("7"), 0.5), Some(Duration::from_secs(7)));
        assert_eq!(delay(3, S, Some("0"), 0.5), Some(Duration::ZERO));
        assert_eq!(delay(1, S, Some("60"), 0.5), Some(Duration::from_secs(60)));
    }

    #[test]
    fn req052_ac5_retry_after_above_60_gives_no_retry() {
        assert_eq!(delay(1, S, Some("61"), 0.5), None);
        assert_eq!(delay(1, S, Some("120"), 0.5), None);
    }

    #[test]
    fn req052_ac5_http_date_follows_req253_and_garbage_falls_back() {
        let w = delay(2, S, Some("Wed, 21 Oct 2015 07:28:07 GMT"), 1.0);
        assert_eq!(w, Some(Duration::from_secs(7)));
        for v in ["bientôt", "-3", "1.5", ""] {
            let w = delay(2, S, Some(v), 1.0).unwrap();
            assert_eq!(w, Duration::from_secs(2), "{v}");
        }
    }

    #[test]
    fn req253_ac1_retry_after_ms_is_read_first() {
        let ms = |v| full(Some(v), None);
        assert_eq!(ms("250"), Some(Duration::from_millis(250)));
        assert_eq!(ms("1500.7"), Some(Duration::from_millis(1500)));
        assert_eq!(ms("0"), Some(Duration::ZERO));
        assert_eq!(
            full(Some("250"), Some("7")),
            Some(Duration::from_millis(250))
        );
        assert_eq!(full(Some("abc"), Some("7")), Some(Duration::from_secs(7)));
        assert_eq!(full(Some("-5"), None), Some(S));
        assert_eq!(ms("60000"), Some(Duration::from_secs(60)));
        assert_eq!(ms("60001"), None);
        assert_eq!(ms("99999999999999999999999"), None);
    }

    #[test]
    fn req253_ac1_imf_fixdate_is_the_date_minus_now() {
        let d = |v| full(None, Some(v));
        assert_eq!(
            d("Wed, 21 Oct 2015 07:28:07 GMT"),
            Some(Duration::from_secs(7))
        );
        assert_eq!(d("Wed, 21 Oct 2015 07:27:00 GMT"), Some(Duration::ZERO));
        assert_eq!(d("Wed, 21 Oct 2015 07:29:01 GMT"), None);
        assert_eq!(
            d("Wed, 21 Oct 2015 07:29:00 GMT"),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn req253_ac1_other_date_forms_give_the_computed_delay() {
        for v in [
            "Wednesday, 21-Oct-15 07:28:07 GMT",
            "Wed Oct 21 07:28:07 2015",
            "demain",
            "Thu, 21 Oct 2015 07:28:07 GMT",
            "Wed, 21 Oct 2015 07:28:07 UTC",
        ] {
            assert_eq!(full(None, Some(v)), Some(S), "{v}");
        }
        assert_eq!(full(None, None), Some(S));
    }

    #[test]
    fn req052_ac5_zero_base_means_no_wait() {
        assert_eq!(delay(4, Duration::ZERO, None, 0.7), Some(Duration::ZERO));
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

    mod shared_pause {
        use super::*;

        const SCHED_SLACK: Duration = Duration::from_millis(200);

        fn ms(n: u64) -> Duration {
            Duration::from_millis(n)
        }

        #[test]
        fn req254_ac4_pause_keeps_the_latest_end_and_says_if_it_moved() {
            let p = Pause::default();
            let t = Instant::now();
            assert!(p.poser(t + ms(1000)));
            assert!(!p.poser(t + ms(300)));
            assert_eq!(p.end(), Some(t + ms(1000)));
            assert!(p.poser(t + ms(2000)));
            assert_eq!(p.end(), Some(t + ms(2000)));
        }

        #[test]
        fn req254_ac4_wait_without_pause_returns_at_once() {
            let p = Pause::default();
            let t = Instant::now();
            assert!(!p.attendre(None));
            assert!(t.elapsed() < ms(50));
            // Une pause déjà finie n'attend pas non plus.
            p.poser(Instant::now());
            assert!(!p.attendre(Some(&AtomicBool::new(false))));
            assert!(t.elapsed() < ms(50));
        }

        #[test]
        fn req254_ac4_wait_lasts_until_the_end_of_the_pause() {
            let p = Pause::default();
            let t = Instant::now();
            p.poser(t + ms(350));
            assert!(!p.attendre(None));
            let e = t.elapsed();
            assert!(
                e >= ms(350) && e < ms(350) + WAIT_SLICE + SCHED_SLACK,
                "{e:?}"
            );
        }

        #[test]
        fn req254_ac4_stop_set_by_another_thread_interrupts_the_wait() {
            let p = Pause::default();
            let stop = AtomicBool::new(false);
            p.poser(Instant::now() + Duration::from_secs(5));
            let t = Instant::now();
            let (interrupted, ended, set_at) = std::thread::scope(|scope| {
                let setter = scope.spawn(|| {
                    std::thread::sleep(ms(250));
                    stop.store(true, Ordering::SeqCst);
                    Instant::now()
                });
                let interrupted = p.attendre(Some(&stop));
                (interrupted, Instant::now(), setter.join().unwrap())
            });
            assert!(interrupted);
            assert!(ended - t >= ms(250));
            let lag = ended.saturating_duration_since(set_at);
            assert!(lag <= ms(100) + SCHED_SLACK, "{lag:?}");
        }

        #[test]
        fn req254_ac4_stop_already_set_returns_at_once_and_sends_nothing() {
            let p = Pause::default();
            p.poser(Instant::now() + Duration::from_secs(5));
            let stop = AtomicBool::new(true);
            let t = Instant::now();
            assert!(p.attendre(Some(&stop)));
            assert!(t.elapsed() < ms(50));
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/x", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let r = send_with(
                "openai",
                ureq::agent().post(&url),
                None,
                &Retry::from_millis(1),
                None,
                Some(&stop),
                &CallStats::default(),
                Some(&p),
                false,
            );
            assert_eq!(r, Err(Error::Unsent));
            assert!(listener.accept().is_err(), "une requête est partie");
        }
    }

    mod send_limit {
        use super::*;

        const SCHED_SLACK: Duration = Duration::from_millis(200);

        fn ms(n: u64) -> Duration {
            Duration::from_millis(n)
        }

        fn limits(concurrency: usize) -> Vec<usize> {
            let p = Pause::new(concurrency);
            let t = Instant::now();
            let mut seen = vec![];
            let mut pose = |at: Duration, end: Duration| {
                p.poser_limite(t + at, t + end);
                seen.push(p.limit());
            };
            pose(ms(0), ms(1000));
            pose(ms(500), ms(1500));
            pose(ms(2000), ms(3000));
            pose(ms(4000), ms(5000));
            for _ in 0..16 {
                p.reussir();
                seen.push(p.limit());
            }
            seen
        }

        #[test]
        fn req255_ac1_limit_halves_per_pause_and_climbs_back() {
            let seen = limits(4);
            assert_eq!(seen[..4], [2, 2, 1, 1]);
            // 1 réussite -> 2, 2 -> 3, 3 -> 4, puis plus rien.
            let climb: Vec<usize> = seen[4..].to_vec();
            let mut expected = vec![2, 2, 3, 3, 3, 4];
            expected.extend([4; 10]);
            assert_eq!(climb, expected);
        }

        #[test]
        fn req255_ac1_climb_reports_new_and_old_limit() {
            let p = Pause::new(4);
            let t = Instant::now();
            assert_eq!(p.poser_limite(t, t + ms(10)), (true, Some(2)));
            assert_eq!(p.poser_limite(t + ms(5), t + ms(20)), (true, None));
            assert_eq!(p.reussir(), None);
            assert_eq!(p.reussir(), Some((3, 2)));
        }

        #[test]
        fn req255_ac1_concurrency_one_never_moves() {
            let p = Pause::new(1);
            let t = Instant::now();
            assert_eq!(p.poser_limite(t, t + ms(10)), (true, None));
            assert_eq!(p.poser_limite(t + ms(50), t + ms(60)), (true, None));
            for _ in 0..5 {
                assert_eq!(p.reussir(), None);
            }
            assert_eq!(p.limit(), 1);
        }

        #[test]
        fn req255_ac2_third_place_waits_for_a_freed_one() {
            let p = Pause::new(4);
            let t = Instant::now();
            p.poser_limite(t, t + ms(1)); // limite 2
            assert_eq!(p.limit(), 2);
            let a = p.prendre(None).unwrap();
            let _b = p.prendre(None).unwrap();
            let (got, freed_at, served_at) = std::thread::scope(|scope| {
                let waiter = scope.spawn(|| {
                    let r = p.prendre(None).is_some();
                    (r, Instant::now())
                });
                std::thread::sleep(ms(250));
                let freed_at = Instant::now();
                drop(a);
                let (r, served_at) = waiter.join().unwrap();
                (r, freed_at, served_at)
            });
            assert!(got);
            assert!(served_at >= freed_at);
            assert!(served_at - freed_at <= ms(100) + SCHED_SLACK);
        }

        #[test]
        fn req255_ac2_waiting_thread_is_not_served_during_the_pause() {
            let p = Pause::new(2);
            let t = Instant::now();
            p.poser_limite(t, t + ms(1)); // limite 1
            let a = p.prendre(None).unwrap();
            let (served_at, pause_end) = std::thread::scope(|scope| {
                let waiter = scope.spawn(|| {
                    assert!(p.prendre(None).is_some());
                    Instant::now()
                });
                std::thread::sleep(ms(100));
                let end = Instant::now() + ms(300);
                p.poser(end);
                drop(a);
                (waiter.join().unwrap(), end)
            });
            assert!(served_at >= pause_end, "servi pendant la pause");
        }

        #[test]
        fn req255_ac2_stop_interrupts_the_wait_and_takes_no_place() {
            let p = Pause::new(2);
            let t = Instant::now();
            p.poser_limite(t, t + ms(1)); // limite 1
            let _a = p.prendre(None).unwrap();
            let stop = AtomicBool::new(false);
            let (got, ended, set_at) = std::thread::scope(|scope| {
                let setter = scope.spawn(|| {
                    std::thread::sleep(ms(250));
                    stop.store(true, Ordering::SeqCst);
                    Instant::now()
                });
                let got = p.prendre(Some(&stop)).is_some();
                (got, Instant::now(), setter.join().unwrap())
            });
            assert!(!got);
            assert!(ended.saturating_duration_since(set_at) <= ms(100) + SCHED_SLACK);
            assert_eq!(p.lock().taken, 1);
        }
    }

    mod interrupted_wait {
        use super::*;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::AtomicUsize;

        /// Marge d'ordonnancement : 453 et 487 ms constatés sous macOS en CI pour 250 + 100 ms attendus.
        const SCHED_SLACK: Duration = Duration::from_millis(200);

        fn ms(n: u64) -> Duration {
            Duration::from_millis(n)
        }

        #[test]
        fn req095_ac2_stop_set_by_another_thread_ends_the_wait_within_a_slice() {
            let stop = AtomicBool::new(false);
            let t = Instant::now();
            let (interrupted, ended, set_at) = std::thread::scope(|scope| {
                let setter = scope.spawn(|| {
                    std::thread::sleep(ms(250));
                    stop.store(true, Ordering::SeqCst);
                    Instant::now()
                });
                let interrupted = wait(Duration::from_secs(5), Some(&stop));
                let ended = Instant::now();
                (interrupted, ended, setter.join().unwrap())
            });
            assert!(interrupted);
            assert!(ended - t >= ms(250), "{:?}", ended - t);
            let lag = ended.saturating_duration_since(set_at);
            // Borne littérale (100 ms) : relative à `WAIT_SLICE`, elle suivrait une tranche allongée.
            let bound = ms(100) + SCHED_SLACK;
            assert!(lag <= bound, "{lag:?} après l'arrêt, borne {bound:?}");
        }

        #[test]
        fn req095_ac2_wait_without_stop_lasts_its_duration() {
            let stop = AtomicBool::new(false);
            for stop in [None, Some(&stop)] {
                let t = Instant::now();
                assert!(!wait(ms(350), stop));
                let e = t.elapsed();
                assert!(
                    e >= ms(350) && e < ms(350) + WAIT_SLICE + SCHED_SLACK,
                    "{e:?}"
                );
            }
        }

        #[test]
        fn req095_ac2_stop_already_set_gives_no_new_send() {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/x", listener.local_addr().unwrap());
            let count = Arc::new(AtomicUsize::new(0));
            let served = Arc::clone(&count);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut s) = stream else { return };
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    served.fetch_add(1, Ordering::SeqCst);
                    let _ = s.write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nretry-after: 30\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                }
            });
            let agent = agent(10, 1, &Tls::default());
            let stop = AtomicBool::new(true);
            let t = Instant::now();
            let r = send(
                "test",
                agent.post(&url),
                Some(&serde_json::json!({})),
                &Retry::default(),
                None,
                Some(&stop),
                &CallStats::default(),
                &Pause::default(),
            );
            assert!(t.elapsed() < Duration::from_secs(2));
            assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
            assert_eq!(count.load(Ordering::SeqCst), 1);
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

        fn serve<S: Read + Write>(mut stream: S, arrived: &(Mutex<usize>, Condvar), sse: bool) {
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
                let out = if sse {
                    // Flux OpenAI complet, corps `chunked` : un morceau par ligne de flux.
                    let events = [
                        "data: {\"choices\":[{\"delta\":{\"content\":\"{}\"}}]}\n\n",
                        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n",
                    ];
                    let mut out = String::from(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                    );
                    for e in events {
                        out += &format!("{:x}\r\n{e}\r\n", e.len());
                    }
                    out + "0\r\n\r\n"
                } else {
                    let reply = "{}";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    )
                };
                if stream.write_all(out.as_bytes()).is_err() || stream.flush().is_err() {
                    return;
                }
            }
        }

        fn server(tls: bool, sse: bool) -> (String, Arc<AtomicUsize>) {
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
                            serve(stream, &arrived, sse)
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
                            serve(s, &arrived, sse)
                        }
                    });
                }
            });
            let scheme = if tls { "https" } else { "http" };
            (format!("{scheme}://{addr}/x"), conns)
        }

        /// `THREADS` fils : une requête, puis (une fois les réponses toutes lues) une seconde.
        fn connections_for(tls: bool, sse: bool) -> usize {
            let (url, conns) = server(tls, sse);
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
                            send(
                                "test",
                                req,
                                Some(&body),
                                retry,
                                None,
                                None,
                                &CallStats::default(),
                                &Pause::default(),
                            )
                            .unwrap();
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
            assert_eq!(connections_for(false, false), THREADS);
        }

        #[test]
        fn req053_ac3_idle_pool_keeps_one_tls_connection_per_thread() {
            assert_eq!(connections_for(true, false), THREADS);
        }

        #[test]
        fn req249_ac6_chunked_event_stream_keeps_one_connection_per_thread() {
            assert_eq!(connections_for(false, true), THREADS);
            assert_eq!(connections_for(true, true), THREADS);
        }
    }

    mod counted_sends {
        use super::*;
        use crate::llm::usage::fake_server;

        fn run(replies: Vec<(u16, String)>) -> (Result<String>, u32) {
            let base = fake_server(replies);
            let agent = agent(10, 1, &Tls::default());
            let stats = CallStats::default();
            let r = send(
                "test",
                agent.post(&format!("{base}/x")),
                Some(&serde_json::json!({})),
                &Retry::from_millis(1),
                None,
                None,
                &stats,
                &Pause::default(),
            );
            (r, stats.sends())
        }

        #[test]
        fn req165_ac5_every_attempt_is_counted() {
            let (r, n) = run(vec![
                (503, "{}".into()),
                (503, "{}".into()),
                (200, "{}".into()),
            ]);
            assert!(r.is_ok(), "{r:?}");
            assert_eq!(n, 3);
        }

        #[test]
        fn req165_ac5_a_refused_key_is_one_send() {
            let (r, n) = run(vec![(401, "{}".into())]);
            assert!(matches!(r, Err(Error::InvalidKey(_))), "{r:?}");
            assert_eq!(n, 1);
        }

        #[test]
        fn req165_ac5_exhausted_retries_count_five_sends() {
            let (r, n) = run(vec![(500, "{}".into())]);
            assert!(r.is_err());
            assert_eq!(n, 5);
        }
    }

    // ---- REQ-176 : objet JSON conforme isolé du texte autour ----

    mod reply {
        use super::super::parse_reply;
        use crate::error::Error;
        use crate::llm::enrich::{parse_lot, Enrichment};
        use crate::llm::{ask::ask_system_prompt, AskReply, LotItem};

        const EXTRACTION: &str =
            r#"{"version":2,"summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]}"#;

        fn ids() -> Vec<String> {
            vec!["type:A".to_string()]
        }

        fn enrich(text: &str) -> crate::error::Result<Enrichment> {
            parse_reply(text, "summaries", |j| {
                Enrichment::parse(j, "A.java", &ids())
            })
        }

        /// Message de la 0.26.0 en cours : `extract_json` puis `Enrichment::parse`.
        fn old(text: &str) -> String {
            match (text.find('{'), text.rfind('}')) {
                (Some(a), Some(b)) if a < b => Enrichment::parse(&text[a..=b], "A.java", &ids())
                    .unwrap_err()
                    .to_string(),
                _ => "réponse LLM non conforme : aucun objet JSON dans le texte".to_string(),
            }
        }

        #[test]
        fn req176_ac1_object_is_accepted_with_text_around() {
            let with_braces = EXTRACTION.replace("Le A.", "Un { et } dans le texte");
            let cases = [
                EXTRACTION.to_string(),
                format!("```json\n{EXTRACTION}\n```"),
                format!("{EXTRACTION}\nRemarque : la méthode {{x}} est triviale."),
                format!("Voir {{x}} :\n{EXTRACTION}"),
                format!("{{\"note\": \"a\"}}\n{EXTRACTION}"),
                format!("{EXTRACTION}{{x}}"),
                format!("{EXTRACTION}\n{EXTRACTION}"),
                format!("{with_braces}}}"),
            ];
            for c in cases {
                let e = enrich(&c).unwrap_or_else(|e| panic!("{c} : {e}"));
                assert_eq!(e.summaries.len(), 1, "{c}");
            }
        }

        #[test]
        fn req176_ac2_non_conforming_texts_are_format_errors() {
            let e = enrich("aucun objet ici").unwrap_err();
            assert!(matches!(e, Error::Format(_)));
            assert_eq!(
                e.to_string(),
                "réponse LLM non conforme : aucun objet JSON dans le texte"
            );
            let v1 = r#"{"version":1,"summaries":[],"concepts":[]}"#;
            let e = enrich(&format!("{v1}{{x}}")).unwrap_err();
            assert!(matches!(e, Error::Format(_)));
            assert!(
                e.to_string().contains("version 1 non prise en charge"),
                "{e}"
            );
            let other = r#"{"version":2,"summaries":[],"concepts":[]}"#;
            let e = enrich(&format!("{EXTRACTION}\n{other}")).unwrap_err();
            assert!(matches!(e, Error::Format(_)));
        }

        #[test]
        fn req176_ac2_single_object_messages_are_those_of_the_old_path() {
            let wrapped = format!(r#"{{"result":{EXTRACTION}}}"#);
            let cases = [
                r#"{"version":2,"summaries":["#.to_string(),
                r#"{"version":1,"summaries":[],"concepts":[]}"#.to_string(),
                r#"{"version":2,"summaries":[],"concepts":[],"extra":1}"#.to_string(),
                r#"{"version":2,"summaries":[]}"#.to_string(),
                wrapped,
            ];
            for c in cases {
                let e = enrich(&c).unwrap_err();
                assert!(matches!(e, Error::Format(_)), "{c}");
                assert_eq!(e.to_string(), old(&c), "{c}");
            }
        }

        fn lot_items() -> Vec<LotItem> {
            vec![LotItem {
                file: "A.java".into(),
                text: "class A {}".into(),
                node_ids: vec!["type:A".into()],
            }]
        }

        #[test]
        fn req176_ac3_lot_and_ask_keys() {
            let items = lot_items();
            let lot = |t: &str| parse_reply(t, "files", |j| parse_lot(j, &items));
            let v3 = r#"{"version":3,"files":[{"file":"A.java","summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]}]}"#;
            assert!(lot(&format!("{v3}\n{{x}}")).is_ok());
            let v2 = r#"{"version":2,"files":[]}"#;
            assert!(matches!(lot(v2), Err(Error::Format(_))));

            let found_false = |t: &str| parse_reply(t, "found", AskReply::parse);
            assert_eq!(
                found_false("{\"found\": false}\n(aucun nœud {x} ne répond)").unwrap(),
                AskReply::NotFound
            );
            let p = ask_system_prompt();
            let ex = p
                .lines()
                .find(|l| l.starts_with("Exemple : "))
                .expect("exemple");
            let e = found_false(&format!("{ex}\n{{\"found\": false}}")).unwrap_err();
            assert!(matches!(e, Error::Format(_)));
        }

        #[test]
        fn req201_ac3_keys_summaries_and_files() {
            assert!(enrich(r#"Voici : {"summaries": [], "concepts": []} fin"#).is_ok());
            let items = lot_items();
            let lot = |t: &str| parse_reply(t, "files", |j| parse_lot(j, &items));
            let entry = r#"{"file":"A.java","summaries":[],"concepts":[]}"#;
            assert!(lot(&format!("{{\"files\":[{entry}]}}\n{{x}}")).is_ok());
            assert!(matches!(
                lot(r#"{"version": 2, "files": []}"#),
                Err(Error::Format(_))
            ));
        }

        #[test]
        fn req176_ac3_many_objects_and_unclosed_openers_stay_fast() {
            let many = format!("{}{EXTRACTION}", "{\"a\":1}".repeat(300));
            assert_eq!(enrich(&many).unwrap().summaries.len(), 1);
            let t = std::time::Instant::now();
            let deep = format!("{}{EXTRACTION}", "{\"a\":[".repeat(2000));
            assert_eq!(enrich(&deep).unwrap().summaries.len(), 1);
            assert!(t.elapsed() < std::time::Duration::from_secs(2));
        }
    }
}
