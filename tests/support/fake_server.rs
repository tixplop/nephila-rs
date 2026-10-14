//! Faux serveur HTTP local : enregistre les requêtes, renvoie des réponses programmées.

use std::collections::HashMap;
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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(responses)));
        let rec = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
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
                reader.read_exact(&mut body).unwrap();
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
            }
        });
        FakeServer { base_url, requests }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}
