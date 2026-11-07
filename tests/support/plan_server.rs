//! Serveur HTTP programmable : la réaction à chaque requête est choisie par son rang.
//! Partagé par `tests/max_calls.rs` et `tests/interrupt.rs`.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Instant d'arrivée d'une requête et instant d'écriture de sa réponse.
pub type Times = Vec<(Instant, Option<Instant>)>;

pub enum Act {
    /// Réponse conforme après `delay_ms`.
    Ok(u64),
    /// Réponse HTTP d'erreur, avec `retry-after` éventuel.
    Status(u16, Option<&'static str>),
    /// Réponse HTTP d'erreur écrite une fois `n` requêtes reçues (attente plafonnée à 10 s).
    StatusAfter(u16, usize),
    /// Réponse HTTP d'erreur avec `retry-after`, écrite une fois `n` requêtes reçues (attente
    /// plafonnée à 10 s).
    HeldRetry(u16, usize, &'static str),
    /// Réponse 200 qui n'est pas un JSON conforme.
    Garbage,
    /// Réponse 200 de JSON valide mais hors schéma, après `delay_ms`.
    SlowNonConforming(u64),
}

pub struct Server {
    pub base_url: String,
    count: Arc<AtomicUsize>,
    served: Arc<AtomicUsize>,
    times: Arc<Mutex<Times>>,
}

impl Server {
    /// `plan(i)` : réaction à la requête numéro `i` (0 pour la première), tous clients confondus.
    pub fn start(plan: impl Fn(usize) -> Act + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let served = Arc::new(AtomicUsize::new(0));
        let plan = Arc::new(plan);
        let times = Arc::new(Mutex::new(Vec::new()));
        let (c, sv, tm) = (count.clone(), served.clone(), times.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (c, sv, plan, tm) = (c.clone(), sv.clone(), plan.clone(), tm.clone());
                std::thread::spawn(move || serve(stream, &c, &sv, &tm, &*plan));
            }
        });
        Server {
            base_url,
            count,
            served,
            times,
        }
    }

    /// Arrivée et réponse de chaque requête, dans l'ordre d'arrivée.
    pub fn times(&self) -> Times {
        self.times.lock().unwrap().clone()
    }

    /// Requêtes entièrement reçues.
    pub fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    /// Réponses entièrement écrites.
    pub fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

fn read_more(stream: &mut std::net::TcpStream, buf: &mut Vec<u8>) -> bool {
    let mut chunk = [0u8; 8192];
    match stream.read(&mut chunk) {
        Ok(0) | Err(_) => false,
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            true
        }
    }
}

fn serve(
    mut stream: std::net::TcpStream,
    count: &AtomicUsize,
    served: &AtomicUsize,
    times: &Mutex<Times>,
    plan: &dyn Fn(usize) -> Act,
) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if !read_more(&mut stream, &mut buf) {
                return;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        while buf.len() < head_end + len {
            if !read_more(&mut stream, &mut buf) {
                return;
            }
        }
        let body: Vec<u8> = buf[head_end..head_end + len].to_vec();
        buf.drain(..head_end + len);
        let i = {
            let mut t = times.lock().unwrap();
            t.push((Instant::now(), None));
            count.fetch_add(1, Ordering::SeqCst)
        };
        let act = plan(i);
        let (status, extra, payload) = match act {
            Act::Ok(delay) => {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                (200, String::new(), chat(&conformant(&body)))
            }
            Act::SlowNonConforming(delay) => {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                (200, String::new(), chat(r#"{"inattendu": true}"#))
            }
            Act::Garbage => (200, String::new(), chat("ceci n'est pas du JSON")),
            Act::StatusAfter(s, n) => {
                let start = std::time::Instant::now();
                while count.load(Ordering::SeqCst) < n
                    && start.elapsed() < std::time::Duration::from_secs(10)
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                (
                    s,
                    String::new(),
                    serde_json::json!({"error":{"message":"simulé"}}).to_string(),
                )
            }
            Act::HeldRetry(s, n, retry) => {
                let start = Instant::now();
                while count.load(Ordering::SeqCst) < n
                    && start.elapsed() < std::time::Duration::from_secs(10)
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                (
                    s,
                    format!("retry-after: {retry}\r\n"),
                    serde_json::json!({"error":{"message":"simulé"}}).to_string(),
                )
            }
            Act::Status(s, retry) => (
                s,
                retry
                    .map(|r| format!("retry-after: {r}\r\n"))
                    .unwrap_or_default(),
                serde_json::json!({"error":{"message":"simulé"}}).to_string(),
            ),
        };
        let resp = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\n\r\n{payload}",
            payload.len()
        );
        if stream.write_all(resp.as_bytes()).is_err() {
            return;
        }
        times.lock().unwrap()[i].1 = Some(Instant::now());
        served.fetch_add(1, Ordering::SeqCst);
    }
}

pub fn chat(content: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":content}}]}).to_string()
}

/// Réponse conforme d'un appel d'un seul fichier : un résumé par nœud de la liste de la requête.
fn conformant(body: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap();
    let user = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    let mut ids = Vec::new();
    let mut in_list = false;
    for line in user.lines() {
        if line == "Nœuds du fichier :" {
            in_list = true;
        } else if in_list {
            match line
                .strip_prefix('[')
                .and_then(|r| r.split_once(" — "))
                .map(|(_, id)| id)
            {
                Some(id) => ids.push(id.to_string()),
                None => in_list = false,
            }
        }
    }
    let summaries: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({"node": id, "summary": "résumé"}))
        .collect();
    serde_json::json!({"version": 2, "summaries": summaries, "concepts": []}).to_string()
}
