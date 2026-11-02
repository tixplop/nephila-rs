//! Faux serveur du mode lot : routes (méthode + chemin) à état, chacune avec son script de
//! réponses ; chaque requête est consignée (méthode, chemin, en-têtes, corps). À utiliser avec
//! `fake_server` déclaré à la racine du test (types `Reply` et `Recorded`).

use crate::fake_server::{read_request, record, Recorded, Reply};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Routes = Arc<Mutex<HashMap<(String, String), VecDeque<Reply>>>>;

pub struct BatchServer {
    pub base_url: String,
    routes: Routes,
    requests: Arc<Mutex<Vec<Recorded>>>,
    sim: Arc<Mutex<Option<Sim>>>,
}

/// Issue d'une requête d'un lot simulé.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Texte de la réponse du modèle.
    Text(String),
    /// Réponse tronquée par la limite de jetons.
    Truncated(String),
    /// Requête en échec (`type`, `message`).
    Errored(String, String),
    /// Requête expirée (anthropic seulement).
    Expired,
    /// Aucune ligne de résultat.
    Missing,
}

type Answer = Box<dyn Fn(&Value) -> Outcome + Send + Sync>;

/// Faux fournisseur de lots : crée les lots, les sonde et rend les résultats que `answer` donne
/// pour le corps de chaque requête reçue.
struct Sim {
    provider: String,
    answer: Answer,
    /// Sondages rendus `en cours` avant la fin.
    in_progress: usize,
    /// Statut final imposé à `openai` (`failed`, `expired`) et son message.
    final_status: Option<(String, String)>,
    /// Pour chaque lot : `(custom_id, corps)` des requêtes.
    lots: Vec<Vec<(String, Value)>>,
    /// Fichiers d'entrée déposés (`openai`).
    files: Vec<Vec<(String, Value)>>,
    polls: HashMap<usize, usize>,
    /// Résultats rendus dans l'ordre inverse de l'envoi.
    reverse: bool,
    /// Réponses données aux premières créations, avant que l'une aboutisse.
    create_failures: VecDeque<Reply>,
}

impl BatchServer {
    pub fn start() -> BatchServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let sim: Arc<Mutex<Option<Sim>>> = Arc::default();
        let (r, q, m) = (routes.clone(), requests.clone(), sim.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (r, q, m) = (r.clone(), q.clone(), m.clone());
                std::thread::spawn(move || serve(stream, &r, &q, &m));
            }
        });
        BatchServer {
            base_url,
            routes,
            requests,
            sim,
        }
    }

    /// Fait du serveur un fournisseur de lots (`anthropic` ou `openai`) : les routes sans script
    /// lui reviennent. `in_progress` sondages rendent `en cours` avant la fin du lot.
    pub fn simulate(
        &self,
        provider: &str,
        in_progress: usize,
        answer: impl Fn(&Value) -> Outcome + Send + Sync + 'static,
    ) {
        *self.sim.lock().unwrap() = Some(Sim {
            provider: provider.into(),
            answer: Box::new(answer),
            in_progress,
            final_status: None,
            lots: Vec::new(),
            files: Vec::new(),
            polls: HashMap::new(),
            reverse: false,
            create_failures: VecDeque::new(),
        });
    }

    /// Les résultats des lots sont rendus dans l'ordre inverse de l'envoi.
    pub fn reverse_results(&self) {
        if let Some(s) = self.sim.lock().unwrap().as_mut() {
            s.reverse = true;
        }
    }

    /// Les premières créations de lot reçoivent ces réponses, puis la suivante aboutit.
    pub fn fail_creates(&self, replies: Vec<Reply>) {
        if let Some(s) = self.sim.lock().unwrap().as_mut() {
            s.create_failures = replies.into();
        }
    }

    /// Statut final imposé aux lots d'`openai` (`failed` ou `expired`), avec son message.
    pub fn final_status(&self, status: &str, message: &str) {
        if let Some(s) = self.sim.lock().unwrap().as_mut() {
            s.final_status = Some((status.into(), message.into()));
        }
    }

    /// Corps des requêtes de chaque lot créé, dans l'ordre de création.
    pub fn lots(&self) -> Vec<Vec<Value>> {
        let sim = self.sim.lock().unwrap();
        sim.as_ref().map_or_else(Vec::new, |s| {
            s.lots
                .iter()
                .map(|l| l.iter().map(|(_, b)| b.clone()).collect())
                .collect()
        })
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

fn serve(
    stream: std::net::TcpStream,
    routes: &Routes,
    requests: &Arc<Mutex<Vec<Recorded>>>,
    sim: &Arc<Mutex<Option<Sim>>>,
) {
    let mut reader = BufReader::new(stream);
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    let (method, path) = (request.0.clone(), request.1.clone());
    let body = String::from_utf8_lossy(&request.3).into_owned();
    record(requests, request);
    let scripted = {
        let mut routes = routes.lock().unwrap();
        match routes.get_mut(&(method.clone(), path.clone())) {
            Some(q) if q.len() > 1 => Some(q.pop_front().unwrap()),
            Some(q) if q.len() == 1 => Some(q[0].clone()),
            _ => None,
        }
    };
    let reply = scripted
        .or_else(|| {
            let mut sim = sim.lock().unwrap();
            sim.as_mut().and_then(|s| s.handle(&method, &path, &body))
        })
        .unwrap_or_else(|| Reply::status(404, "{}"));
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

fn text_body(provider: &str, text: &str, truncated: bool) -> Value {
    if provider == "anthropic" {
        let stop = if truncated { "max_tokens" } else { "end_turn" };
        json!({"content": [{"type": "text", "text": text}], "stop_reason": stop})
    } else {
        let stop = if truncated { "length" } else { "stop" };
        json!({"choices": [{"message": {"role": "assistant", "content": text},
                            "finish_reason": stop}]})
    }
}

fn ok(body: Value) -> Option<Reply> {
    Some(Reply::status(200, &body.to_string()))
}

/// Numéro `k` (1 pour le premier) à la fin de `prefix<k>`.
fn index_of(path: &str, prefix: &str) -> Option<usize> {
    path.strip_prefix(prefix)?.parse::<usize>().ok()
}

impl Sim {
    fn handle(&mut self, method: &str, path: &str, body: &str) -> Option<Reply> {
        let creation = method == "POST" && matches!(path, "/v1/messages/batches" | "/v1/files");
        if creation {
            if let Some(reply) = self.create_failures.pop_front() {
                return Some(reply);
            }
        }
        if self.provider == "anthropic" {
            self.handle_anthropic(method, path, body)
        } else {
            self.handle_openai(method, path, body)
        }
    }

    fn outcomes(&self, k: usize) -> Vec<(&str, Outcome)> {
        self.lots[k - 1]
            .iter()
            .map(|(id, b)| (id.as_str(), (self.answer)(b)))
            .collect()
    }

    fn joined(&self, mut lines: Vec<String>) -> String {
        if self.reverse {
            lines.reverse();
        }
        lines.join("\n")
    }

    fn polled(&mut self, k: usize) -> bool {
        let n = self.polls.entry(k).or_insert(0);
        *n += 1;
        *n > self.in_progress
    }

    fn handle_anthropic(&mut self, method: &str, path: &str, body: &str) -> Option<Reply> {
        const BASE: &str = "/v1/messages/batches";
        if (method, path) == ("POST", BASE) {
            let v: Value = serde_json::from_str(body).ok()?;
            let lot = v["requests"]
                .as_array()?
                .iter()
                .map(|r| {
                    (
                        r["custom_id"].as_str().unwrap().to_string(),
                        r["params"].clone(),
                    )
                })
                .collect();
            self.lots.push(lot);
            return ok(json!({"id": format!("msgbatch_{}", self.lots.len())}));
        }
        let rest = path.strip_prefix(BASE)?.strip_prefix("/msgbatch_")?;
        let (k, results) = match rest.strip_suffix("/results") {
            Some(k) => (k.parse::<usize>().ok()?, true),
            None => (rest.parse::<usize>().ok()?, false),
        };
        if k == 0 || k > self.lots.len() || method != "GET" {
            return None;
        }
        if results {
            let lines: Vec<String> = self
                .outcomes(k)
                .into_iter()
                .filter_map(|(id, o)| {
                    let result = match o {
                        Outcome::Text(t) => {
                            json!({"type": "succeeded", "message": text_body("anthropic", &t, false)})
                        }
                        Outcome::Truncated(t) => {
                            json!({"type": "succeeded", "message": text_body("anthropic", &t, true)})
                        }
                        Outcome::Errored(kind, message) => {
                            json!({"type": "errored",
                                   "error": {"type": "error", "error": {"type": kind, "message": message}}})
                        }
                        Outcome::Expired => json!({"type": "expired"}),
                        Outcome::Missing => return None,
                    };
                    Some(json!({"custom_id": id, "result": result}).to_string())
                })
                .collect();
            return Some(Reply::status(200, &self.joined(lines)));
        }
        let total = self.lots[k - 1].len();
        if !self.polled(k) {
            let counts = json!({"processing": total, "succeeded": 0, "errored": 0, "canceled": 0, "expired": 0});
            return ok(json!({"processing_status": "in_progress", "request_counts": counts}));
        }
        let o = self.outcomes(k);
        let n = |f: fn(&Outcome) -> bool| o.iter().filter(|(_, x)| f(x)).count();
        let counts = json!({
            "processing": 0,
            "succeeded": n(|x| matches!(x, Outcome::Text(_) | Outcome::Truncated(_))),
            "errored": n(|x| matches!(x, Outcome::Errored(..))),
            "canceled": 0,
            "expired": n(|x| matches!(x, Outcome::Expired)),
        });
        ok(json!({"processing_status": "ended", "request_counts": counts}))
    }

    fn handle_openai(&mut self, method: &str, path: &str, body: &str) -> Option<Reply> {
        if (method, path) == ("POST", "/v1/files") {
            let jsonl = body
                .split("application/jsonl\r\n\r\n")
                .nth(1)?
                .split("\r\n--nephila")
                .next()?;
            let file = jsonl
                .lines()
                .map(|l| {
                    let v: Value = serde_json::from_str(l).unwrap();
                    (
                        v["custom_id"].as_str().unwrap().to_string(),
                        v["body"].clone(),
                    )
                })
                .collect();
            self.files.push(file);
            return ok(json!({"id": format!("file_in_{}", self.files.len())}));
        }
        if (method, path) == ("POST", "/v1/batches") {
            let v: Value = serde_json::from_str(body).ok()?;
            let k = index_of(v["input_file_id"].as_str()?, "file_in_")?;
            self.lots.push(self.files.get(k - 1)?.clone());
            return ok(json!({"id": format!("batch_{}", self.lots.len())}));
        }
        if let Some(k) = path
            .strip_prefix("/v1/files/file_")
            .and_then(|r| r.strip_suffix("/content"))
        {
            let (error_file, k) = match k.strip_prefix("err_") {
                Some(k) => (true, k.parse::<usize>().ok()?),
                None => (false, k.strip_prefix("out_")?.parse::<usize>().ok()?),
            };
            let lines: Vec<String> = self
                .outcomes(k)
                .into_iter()
                .filter_map(|(id, o)| match (o, error_file) {
                    (Outcome::Text(t), false) => Some(json!({"custom_id": id,
                        "response": {"status_code": 200, "body": text_body("openai", &t, false)}})),
                    (Outcome::Truncated(t), false) => Some(json!({"custom_id": id,
                        "response": {"status_code": 200, "body": text_body("openai", &t, true)}})),
                    (Outcome::Errored(kind, message), true) => Some(json!({"custom_id": id,
                        "response": null, "error": {"code": kind, "message": message}})),
                    _ => None,
                })
                .map(|v| v.to_string())
                .collect();
            return Some(Reply::status(200, &self.joined(lines)));
        }
        let k = index_of(path, "/v1/batches/batch_")?;
        if k == 0 || k > self.lots.len() || method != "GET" {
            return None;
        }
        let total = self.lots[k - 1].len();
        let counts = |done: usize, failed: usize| json!({"total": total, "completed": done, "failed": failed});
        if !self.polled(k) {
            return ok(json!({"status": "in_progress", "request_counts": counts(0, 0)}));
        }
        if let Some((status, message)) = self.final_status.clone().filter(|(s, _)| s == "failed") {
            return ok(json!({"status": status, "request_counts": counts(0, 0),
                             "errors": {"data": [{"code": "x", "message": message}]}}));
        }
        let o = self.outcomes(k);
        let done = o
            .iter()
            .filter(|(_, x)| matches!(x, Outcome::Text(_) | Outcome::Truncated(_)))
            .count();
        let failed = o
            .iter()
            .filter(|(_, x)| matches!(x, Outcome::Errored(..)))
            .count();
        let status = self
            .final_status
            .as_ref()
            .map_or("completed", |(s, _)| s.as_str());
        let mut v = json!({"status": status, "request_counts": counts(done, failed)});
        if done > 0 {
            v["output_file_id"] = json!(format!("file_out_{k}"));
        }
        if failed > 0 {
            v["error_file_id"] = json!(format!("file_err_{k}"));
        }
        ok(v)
    }
}
