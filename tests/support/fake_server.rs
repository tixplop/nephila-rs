//! Faux serveur HTTP local : enregistre les requêtes, renvoie des réponses programmées.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Réponse programmée du faux serveur.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Reply {
    /// Réponse HTTP avec en-têtes supplémentaires et délai avant l'envoi.
    Http {
        status: u16,
        body: String,
        headers: Vec<(String, String)>,
        delay_ms: u64,
    },
    /// Ferme la connexion sans répondre (après lecture de la requête).
    Close,
    /// En-tête `Content-Length: <declared_len>`, puis `sent` octets du corps, puis fermeture.
    Partial {
        status: u16,
        declared_len: usize,
        sent: usize,
    },
}

impl Reply {
    #[allow(dead_code)]
    pub fn status(status: u16, body: &str) -> Reply {
        Reply::Http {
            status,
            body: body.into(),
            headers: vec![],
            delay_ms: 0,
        }
    }

    /// Réponse avec un seul en-tête supplémentaire (`retry-after`, par exemple).
    #[allow(dead_code)]
    pub fn with_header(status: u16, body: &str, name: &str, value: &str) -> Reply {
        Reply::Http {
            status,
            body: body.into(),
            headers: vec![(name.into(), value.into())],
            delay_ms: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Recorded {
    /// Instant d'arrivée de la requête.
    #[allow(dead_code)]
    pub at: Instant,
    /// Instant où la réponse a été écrite (None : connexion fermée sans réponse).
    #[allow(dead_code)]
    pub answered_at: Option<Instant>,
    pub method: String,
    pub path: String,
    /// Noms d'en-têtes en minuscules.
    pub headers: HashMap<String, String>,
    pub body: String,
}

pub struct FakeServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    connections: Arc<AtomicUsize>,
    handshakes: Arc<AtomicUsize>,
}

/// Mode keep-alive : plusieurs requêtes par connexion, connexions servies en parallèle, même
/// réponse 200 à chaque requête.
#[derive(Clone)]
#[allow(dead_code)]
pub struct KeepAlive {
    /// Corps de chaque réponse 200.
    pub body: String,
    /// Délai avant chaque réponse.
    pub delay_ms: u64,
    /// Requêtes servies par connexion avant que le serveur ne la ferme (None : jamais).
    pub max_per_connection: Option<usize>,
    /// Certificat et clé PEM : HTTPS.
    pub tls: Option<(Vec<u8>, Vec<u8>)>,
}

impl FakeServer {
    /// Démarre sur 127.0.0.1:0 ; chaque requête consomme la réponse suivante
    /// `(statut, corps JSON)` ; file vide : 500.
    pub fn start(responses: Vec<(u16, String)>) -> FakeServer {
        FakeServer::start_with_delays(responses.into_iter().map(|(s, b)| (s, b, 0)).collect())
    }

    /// Comme `start`, avec un délai en millisecondes avant chaque réponse.
    pub fn start_with_delays(responses: Vec<(u16, String, u64)>) -> FakeServer {
        FakeServer::spawn(into_replies(responses), None)
    }

    /// Comme `start`, avec des réponses programmées complètes (en-têtes, fermeture sans réponse).
    #[allow(dead_code)]
    pub fn start_replies(replies: Vec<Reply>) -> FakeServer {
        FakeServer::spawn(replies, None)
    }

    /// Comme `start`, en HTTPS (`https://127.0.0.1:<port>`) avec le certificat et la clé PEM
    /// donnés. Une poignée de main refusée n'enregistre aucune requête ; le serveur continue.
    #[allow(dead_code)]
    pub fn start_tls(responses: Vec<(u16, String)>, cert_pem: &[u8], key_pem: &[u8]) -> FakeServer {
        let config = tls_config(cert_pem, key_pem);
        FakeServer::spawn(
            into_replies(responses.into_iter().map(|(s, b)| (s, b, 0)).collect()),
            Some(config),
        )
    }

    /// Serveur keep-alive (voir `KeepAlive`).
    #[allow(dead_code)]
    pub fn start_keep_alive(opts: KeepAlive) -> FakeServer {
        let tls = opts.tls.as_ref().map(|(c, k)| tls_config(c, k));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let scheme = if tls.is_some() { "https" } else { "http" };
        let base_url = format!("{scheme}://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let handshakes = Arc::new(AtomicUsize::new(0));
        let (rec, conns, hs) = (requests.clone(), connections.clone(), handshakes.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                conns.fetch_add(1, Ordering::SeqCst);
                let (rec, hs, opts, tls) = (rec.clone(), hs.clone(), opts.clone(), tls.clone());
                std::thread::spawn(move || match tls {
                    None => serve_keep_alive(stream, &rec, &opts),
                    Some(cfg) => {
                        if let Some(tls_stream) = handshake(stream, &cfg) {
                            hs.fetch_add(1, Ordering::SeqCst);
                            serve_keep_alive(tls_stream, &rec, &opts);
                        }
                    }
                });
            }
        });
        FakeServer {
            base_url,
            requests,
            connections,
            handshakes,
        }
    }

    /// Connexions TCP acceptées.
    #[allow(dead_code)]
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// Poignées de main TLS menées à bien.
    #[allow(dead_code)]
    pub fn handshakes(&self) -> usize {
        self.handshakes.load(Ordering::SeqCst)
    }

    fn spawn(responses: Vec<Reply>, tls: Option<Arc<rustls::ServerConfig>>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let scheme = if tls.is_some() { "https" } else { "http" };
        let base_url = format!("{scheme}://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let rec = requests.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let handshakes = Arc::new(AtomicUsize::new(0));
        let (conns, hs) = (connections.clone(), handshakes.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                conns.fetch_add(1, Ordering::SeqCst);
                match &tls {
                    None => serve(stream, &rec, &queue),
                    Some(cfg) => {
                        // Poignée de main explicite : un refus du client ne laisse aucune trace.
                        let Some(tls_stream) = handshake(stream, cfg) else {
                            continue;
                        };
                        hs.fetch_add(1, Ordering::SeqCst);
                        serve(tls_stream, &rec, &queue);
                    }
                }
            }
        });
        FakeServer {
            base_url,
            requests,
            connections,
            handshakes,
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

type Queue = Arc<Mutex<VecDeque<Reply>>>;

fn into_replies(responses: Vec<(u16, String, u64)>) -> Vec<Reply> {
    responses
        .into_iter()
        .map(|(status, body, delay_ms)| Reply::Http {
            status,
            body,
            headers: vec![],
            delay_ms,
        })
        .collect()
}

fn tls_config(cert_pem: &[u8], key_pem: &[u8]) -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(key_pem).unwrap();
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap(),
    )
}

type TlsStream = rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>;

/// Poignée de main TLS ; `None` si le client la refuse.
fn handshake(stream: std::net::TcpStream, cfg: &Arc<rustls::ServerConfig>) -> Option<TlsStream> {
    let conn = rustls::ServerConnection::new(cfg.clone()).ok()?;
    let mut tls_stream = rustls::StreamOwned::new(conn, stream);
    while tls_stream.conn.is_handshaking() {
        if tls_stream.conn.complete_io(&mut tls_stream.sock).is_err() {
            return None;
        }
    }
    Some(tls_stream)
}

/// Requête lue : méthode, chemin, en-têtes (minuscules), corps.
type RawRequest = (String, String, HashMap<String, String>, Vec<u8>);

fn read_request<R: BufRead>(reader: &mut R) -> Option<RawRequest> {
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    Some((method, path, headers, body))
}

fn record(rec: &Arc<Mutex<Vec<Recorded>>>, (method, path, headers, body): RawRequest) -> usize {
    let mut r = rec.lock().unwrap();
    r.push(Recorded {
        at: Instant::now(),
        answered_at: None,
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    r.len() - 1
}

/// Sert des requêtes sur une connexion tant que le client l'utilise (mode `KeepAlive`).
fn serve_keep_alive<S: Read + Write>(stream: S, rec: &Arc<Mutex<Vec<Recorded>>>, opts: &KeepAlive) {
    let mut reader = BufReader::new(stream);
    let mut served = 0;
    while let Some(req) = read_request(&mut reader) {
        let index = record(rec, req);
        if opts.delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(opts.delay_ms));
        }
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: keep-alive\r\n\r\n{}",
            opts.body.len(),
            opts.body
        );
        let out = reader.get_mut();
        if out.write_all(resp.as_bytes()).is_err() || out.flush().is_err() {
            return;
        }
        rec.lock().unwrap()[index].answered_at = Some(Instant::now());
        served += 1;
        if opts.max_per_connection.is_some_and(|m| served >= m) {
            return; // ferme la connexion : la suivante, côté client, est périmée
        }
    }
}

/// Lit une requête, l'enregistre, écrit la réponse suivante de la file.
fn serve<S: Read + Write>(mut stream: S, rec: &Arc<Mutex<Vec<Recorded>>>, queue: &Queue) {
    let mut reader = BufReader::new(&mut stream);
    let Some(req) = read_request(&mut reader) else {
        return;
    };
    drop(reader);
    let index = record(rec, req);
    // File vide : 500.
    let reply = queue.lock().unwrap().pop_front().unwrap_or(Reply::Http {
        status: 500,
        body: "{}".into(),
        headers: vec![],
        delay_ms: 0,
    });
    let (status, payload, extra, delay_ms) = match reply {
        Reply::Http {
            status,
            body,
            headers,
            delay_ms,
        } => (status, body, headers, delay_ms),
        Reply::Partial {
            status,
            declared_len,
            sent,
        } => {
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {declared_len}\r\nConnection: close\r\n\r\n{}",
                "x".repeat(sent)
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
            rec.lock().unwrap()[index].answered_at = Some(Instant::now());
            return; // fermeture avant la fin du corps
        }
        Reply::Close => return, // fermeture sans réponse
    };
    if delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
    let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
    rec.lock().unwrap()[index].answered_at = Some(Instant::now());
}
