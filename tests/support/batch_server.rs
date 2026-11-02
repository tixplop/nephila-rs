//! Faux serveur du mode lot : routes (méthode + chemin) à état, chacune avec son script de
//! réponses ; chaque requête est consignée (méthode, chemin, en-têtes, corps). À utiliser avec
//! `fake_server` déclaré à la racine du test (types `Reply` et `Recorded`).

use crate::fake_server::{read_request, record, Recorded, Reply};
use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Routes = Arc<Mutex<HashMap<(String, String), VecDeque<Reply>>>>;

pub struct BatchServer {
    pub base_url: String,
    routes: Routes,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl BatchServer {
    pub fn start() -> BatchServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let (r, q) = (routes.clone(), requests.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (r, q) = (r.clone(), q.clone());
                std::thread::spawn(move || serve(stream, &r, &q));
            }
        });
        BatchServer {
            base_url,
            routes,
            requests,
        }
    }

    /// Script de la route : une réponse par requête, la dernière se répète ; route sans script : 404.
    pub fn route(&self, method: &str, path: &str, script: Vec<Reply>) {
        self.routes
            .lock()
            .unwrap()
            .insert((method.into(), path.into()), script.into());
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// Requêtes reçues pour la route.
    pub fn hits(&self, method: &str, path: &str) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == method && r.path == path)
            .collect()
    }
}

fn serve(stream: std::net::TcpStream, routes: &Routes, requests: &Arc<Mutex<Vec<Recorded>>>) {
    let mut reader = BufReader::new(stream);
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    let (method, path) = (request.0.clone(), request.1.clone());
    record(requests, request);
    let reply = {
        let mut routes = routes.lock().unwrap();
        match routes.get_mut(&(method, path)) {
            Some(q) if q.len() > 1 => q.pop_front().unwrap(),
            Some(q) if q.len() == 1 => q[0].clone(),
            _ => Reply::status(404, "{}"),
        }
    };
    let (status, body, extra, declared) = match reply {
        Reply::Http {
            status,
            body,
            headers,
            ..
        } => {
            let n = body.len();
            (status, body, headers, n)
        }
        // Corps coupé : `declared_len` annoncé, `sent` octets écrits, puis fermeture.
        Reply::Partial {
            status,
            declared_len,
            sent,
        } => (status, "x".repeat(sent), vec![], declared_len),
        Reply::Close => return, // connexion fermée sans réponse
    };
    let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}\
         Content-Length: {declared}\r\nConnection: close\r\n\r\n"
    );
    let out = reader.get_mut();
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(body.as_bytes());
    let _ = out.flush();
}
