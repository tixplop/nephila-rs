//! Faux serveur HTTP local : enregistre les requêtes, renvoie des réponses programmées.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Recorded {
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
        FakeServer::spawn(responses, None)
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
            responses.into_iter().map(|(s, b)| (s, b, 0)).collect(),
            Some(Arc::new(config)),
        )
    }

    fn spawn(
        responses: Vec<(u16, String, u64)>,
        tls: Option<Arc<rustls::ServerConfig>>,
    ) -> FakeServer {
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

type Queue = Arc<Mutex<VecDeque<(u16, String, u64)>>>;

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
    rec.lock().unwrap().push(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let (status, payload, delay_ms) =
        queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or((500, "{}".into(), 0));
    if delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}
