//! Faux serveur HTTP local : enregistre les requêtes, renvoie des réponses programmées.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
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
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<Result<_, _>>()
            .unwrap();
        let key = PrivateKeyDer::from_pem_slice(key_pem).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
        FakeServer::spawn(
            into_replies(responses.into_iter().map(|(s, b)| (s, b, 0)).collect()),
            Some(Arc::new(config)),
        )
    }

    fn spawn(responses: Vec<Reply>, tls: Option<Arc<rustls::ServerConfig>>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let scheme = if tls.is_some() { "https" } else { "http" };
        let base_url = format!("{scheme}://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let rec = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                match &tls {
                    None => serve(stream, &rec, &queue),
                    Some(cfg) => {
                        let Ok(conn) = rustls::ServerConnection::new(cfg.clone()) else {
                            continue;
                        };
                        let mut tls_stream = rustls::StreamOwned::new(conn, stream);
                        // Poignée de main explicite : un refus du client ne laisse aucune trace.
                        while tls_stream.conn.is_handshaking() {
                            if tls_stream.conn.complete_io(&mut tls_stream.sock).is_err() {
                                break;
                            }
                        }
                        if tls_stream.conn.is_handshaking() {
                            continue;
                        }
                        serve(tls_stream, &rec, &queue);
                    }
                }
            }
        });
        FakeServer { base_url, requests }
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

/// Lit une requête, l'enregistre, écrit la réponse suivante de la file.
fn serve<S: Read + Write>(mut stream: S, rec: &Arc<Mutex<Vec<Recorded>>>, queue: &Queue) {
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
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
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    drop(reader);
    let index = {
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
    };
    // File vide : 500.
    let reply = queue.lock().unwrap().pop_front().unwrap_or(Reply::Http {
        status: 500,
        body: "{}".into(),
        headers: vec![],
        delay_ms: 0,
    });
    let Reply::Http {
        status,
        body: payload,
        headers: extra,
        delay_ms,
    } = reply
    else {
        return; // fermeture sans réponse
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
