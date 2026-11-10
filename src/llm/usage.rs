//! Relevé d'un appel LLM : envois HTTP, réponse reçue et `usage` normalisé (REQ-165, DEC-134).

use std::cell::{Cell, RefCell};

use serde_json::Value;

/// Jetons d'un appel, valeurs normalisées : `input` compte le cache, `output` la réflexion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: Option<u64>,
}

/// Entier ≥ 0 d'un champ, ou `None`.
fn count(v: &Value) -> Option<u64> {
    v.as_u64()
}

/// Lit `body["usage"]` d'une réponse Anthropic Messages.
pub fn anthropic_usage(body: &Value) -> Option<Usage> {
    let u = &body["usage"];
    let input = count(&u["input_tokens"])?;
    let output = count(&u["output_tokens"])?;
    let cache_read = count(&u["cache_read_input_tokens"]).unwrap_or(0);
    let cache_write = count(&u["cache_creation_input_tokens"]).unwrap_or(0);
    Some(Usage {
        input: input.saturating_add(cache_read).saturating_add(cache_write),
        cache_read,
        cache_write,
        output,
        reasoning: None,
    })
}

/// Lit `body["usage"]` d'une réponse OpenAI Chat Completions (ou compatible).
pub fn openai_usage(body: &Value) -> Option<Usage> {
    let u = &body["usage"];
    let input = count(&u["prompt_tokens"])?;
    let output = count(&u["completion_tokens"])?;
    Some(Usage {
        input,
        cache_read: count(&u["prompt_tokens_details"]["cached_tokens"]).unwrap_or(0),
        cache_write: 0,
        output,
        reasoning: count(&u["completion_tokens_details"]["reasoning_tokens"]),
    })
}

/// Nom du modèle qui a répondu : `body["model"]`, texte de 1 à 256 octets (REQ-278), sinon `None`.
pub fn served_model(body: &Value) -> Option<String> {
    body["model"]
        .as_str()
        .filter(|m| !m.is_empty() && m.len() <= 256)
        .map(String::from)
}

/// Relevé d'un appel : le fil de l'appel l'écrit, l'appelant le lit après coup.
#[derive(Debug)]
pub struct CallStats {
    sends: Cell<u32>,
    responded: Cell<bool>,
    usage: Cell<Option<Usage>>,
    served: RefCell<Option<String>>,
    /// Marque d'un sous-appel, ajoutée en fin de la ligne d'attente (REQ-231).
    mark: String,
    /// Début des traces d'attente et de pause : `llm enrich`, ou `llm sous-système` (REQ-261).
    word: &'static str,
}

impl Default for CallStats {
    fn default() -> Self {
        CallStats {
            sends: Cell::new(0),
            responded: Cell::new(false),
            usage: Cell::new(None),
            served: RefCell::new(None),
            mark: String::new(),
            word: "llm enrich",
        }
    }
}

impl CallStats {
    /// Relevé d'un appel dont les traces d'attente et de pause commencent par `word`.
    pub fn with_word(word: &'static str) -> Self {
        CallStats {
            word,
            ..CallStats::default()
        }
    }
    pub(crate) fn word(&self) -> &'static str {
        self.word
    }
    /// Relevé d'un appel dont les traces d'attente portent `mark`.
    pub fn with_mark(mark: String) -> Self {
        CallStats {
            mark,
            ..CallStats::default()
        }
    }
    pub(crate) fn mark(&self) -> &str {
        &self.mark
    }
    /// Envois HTTP réels de l'appel, nouveaux essais compris.
    pub fn sends(&self) -> u32 {
        self.sends.get()
    }
    /// Une réponse 2xx dont le corps est un JSON lisible a été reçue.
    pub fn responded(&self) -> bool {
        self.responded.get()
    }
    pub fn usage(&self) -> Option<Usage> {
        self.usage.get()
    }
    /// Nom du modèle lu dans la réponse (REQ-278), s'il y en a un.
    pub fn served(&self) -> Option<String> {
        self.served.borrow().clone()
    }
    pub(crate) fn add_send(&self) {
        self.sends.set(self.sends.get().saturating_add(1));
    }
    pub(crate) fn record_response(&self, usage: Option<Usage>, served: Option<String>) {
        self.responded.set(true);
        self.usage.set(usage);
        *self.served.borrow_mut() = served;
    }
}

/// Totaux des appels d'une commande (REQ-165, DEC-134).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmTotals {
    pub calls: u64,
    pub format_retries: u64,
    pub sends: u64,
    pub responded: u64,
    pub without_usage: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: Option<u64>,
}

impl LlmTotals {
    /// Ajoute le relevé d'un appel ; `format_retry` : l'appel est un nouvel essai de format.
    pub fn add(&mut self, stats: &CallStats, format_retry: bool) {
        self.calls = self.calls.saturating_add(1);
        if format_retry {
            self.format_retries = self.format_retries.saturating_add(1);
        }
        self.sends = self.sends.saturating_add(u64::from(stats.sends()));
        if !stats.responded() {
            return;
        }
        self.responded = self.responded.saturating_add(1);
        match stats.usage() {
            None => self.without_usage = self.without_usage.saturating_add(1),
            Some(u) => {
                self.input = self.input.saturating_add(u.input);
                self.cache_read = self.cache_read.saturating_add(u.cache_read);
                self.cache_write = self.cache_write.saturating_add(u.cache_write);
                self.output = self.output.saturating_add(u.output);
                if let Some(r) = u.reasoning {
                    self.reasoning = Some(self.reasoning.unwrap_or(0).saturating_add(r));
                }
            }
        }
    }
}

/// Construction commune du segment de jetons.
fn tokens_text(input: u64, cache: u64, output: u64, reasoning: Option<u64>) -> String {
    let mut s = format!("jetons : entrée {input} dont cache {cache}, sortie {output}");
    if let Some(r) = reasoning {
        s.push_str(&format!(" dont réflexion {r}"));
    }
    s
}

/// Segment de jetons d'un appel : `jetons : entrée <e> dont cache <c>, sortie <s>[ dont réflexion <r>]`.
pub fn tokens_segment(u: &Usage) -> String {
    tokens_text(u.input, u.cache_read, u.output, u.reasoning)
}

/// Ligne de bilan `appels: …` d'une commande.
pub fn calls_line(t: &LlmTotals) -> String {
    let tokens = if t.responded > 0 && t.responded == t.without_usage {
        "jetons : inconnus".to_string()
    } else {
        let mut s = tokens_text(t.input, t.cache_read, t.output, t.reasoning);
        if t.without_usage > 0 && t.without_usage < t.responded {
            s.push_str(&format!(
                " ({} appels sans compte de jetons)",
                t.without_usage
            ));
        }
        s
    };
    format!(
        "appels: {} appels, {} nouveaux essais de format, {} envois HTTP, {tokens}",
        t.calls, t.format_retries, t.sends
    )
}

/// Faux serveur HTTP des tests : une réponse par connexion, dans l'ordre ; la dernière se répète.
#[cfg(test)]
pub(crate) fn fake_server(replies: Vec<(u16, String)>) -> String {
    recording_server(replies, 0).0
}

/// Faux serveur qui répond toujours 200 en `text/event-stream` avec `body` (flux SSE complet).
#[cfg(test)]
pub(crate) fn sse_server(body: String) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { return };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if s.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let text = String::from_utf8_lossy(&head).to_lowercase();
            let len: usize = text
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let _ = s.read_exact(&mut vec![0u8; len]);
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    base
}

/// Comme `fake_server`, et rend aussi les corps reçus, dans l'ordre d'arrivée. Les `hold`
/// premières requêtes sont lues toutes avant que la première réponse parte (appels simultanés).
#[cfg(test)]
pub(crate) fn recording_server(
    replies: Vec<(u16, String)>,
    hold: usize,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = bodies.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let reply = |i: usize, s: &mut std::net::TcpStream| {
            let (code, body) = &replies[i.min(replies.len() - 1)];
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        };
        let mut held = Vec::new();
        for (i, stream) in listener.incoming().enumerate() {
            let Ok(mut s) = stream else { return };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if s.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let text = String::from_utf8_lossy(&head).to_lowercase();
            let len: usize = text
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let mut rest = vec![0u8; len];
            let _ = s.read_exact(&mut rest);
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&rest).into_owned());
            if i < hold {
                held.push(s);
                if held.len() == hold {
                    for (j, mut h) in held.drain(..).enumerate() {
                        reply(j, &mut h);
                    }
                }
            } else {
                reply(i, &mut s);
            }
        }
    });
    (base, bodies)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_start_empty_and_record() {
        let s = CallStats::default();
        assert_eq!((s.sends(), s.responded(), s.usage()), (0, false, None));
        s.add_send();
        s.add_send();
        s.record_response(Some(Usage::default()), None);
        assert_eq!((s.sends(), s.responded()), (2, true));
        assert_eq!(s.usage(), Some(Usage::default()));
    }

    fn stats(sends: u32, usage: Option<Option<Usage>>) -> CallStats {
        let s = CallStats::default();
        for _ in 0..sends {
            s.add_send();
        }
        if let Some(u) = usage {
            s.record_response(u, None);
        }
        s
    }

    fn usage(i: u64, c: u64, o: u64, r: Option<u64>) -> Usage {
        Usage {
            input: i,
            cache_read: c,
            cache_write: 7,
            output: o,
            reasoning: r,
        }
    }

    #[test]
    fn tokens_segment_with_and_without_reasoning() {
        assert_eq!(
            tokens_segment(&usage(1000, 200, 50, None)),
            "jetons : entrée 1000 dont cache 200, sortie 50"
        );
        assert_eq!(
            tokens_segment(&usage(1000, 0, 50, Some(10))),
            "jetons : entrée 1000 dont cache 0, sortie 50 dont réflexion 10"
        );
    }

    #[test]
    fn calls_line_four_states() {
        let mut t = LlmTotals::default();
        assert_eq!(
            calls_line(&t),
            "appels: 0 appels, 0 nouveaux essais de format, 0 envois HTTP, jetons : entrée 0 dont cache 0, sortie 0"
        );
        t.add(&stats(3, Some(None)), false);
        t.add(&stats(1, Some(None)), false);
        assert_eq!(
            calls_line(&t),
            "appels: 2 appels, 0 nouveaux essais de format, 4 envois HTTP, jetons : inconnus"
        );
        let mut t = LlmTotals::default();
        for _ in 0..2 {
            t.add(&stats(1, Some(Some(usage(1000, 200, 50, Some(10))))), false);
        }
        assert_eq!(
            calls_line(&t),
            "appels: 2 appels, 0 nouveaux essais de format, 2 envois HTTP, jetons : entrée 2000 dont cache 400, sortie 100 dont réflexion 20"
        );
        t.add(&stats(1, Some(None)), true);
        assert_eq!(
            calls_line(&t),
            "appels: 3 appels, 1 nouveaux essais de format, 3 envois HTTP, jetons : entrée 2000 dont cache 400, sortie 100 dont réflexion 20 (1 appels sans compte de jetons)"
        );
    }

    #[test]
    fn totals_add_counts_without_response_and_cache_write() {
        let mut t = LlmTotals::default();
        t.add(&stats(5, None), false);
        assert_eq!((t.calls, t.sends, t.responded), (1, 5, 0));
        t.add(&stats(1, Some(Some(usage(10, 2, 3, None)))), true);
        assert_eq!((t.format_retries, t.cache_write, t.reasoning), (1, 7, None));
        assert!(!calls_line(&t).contains('7'));
        t.add(&stats(1, Some(Some(usage(1, 0, 1, Some(4))))), false);
        assert_eq!(t.reasoning, Some(4));
    }

    #[test]
    fn totals_add_saturates() {
        let mut t = LlmTotals {
            input: u64::MAX - 1,
            ..Default::default()
        };
        t.add(&stats(1, Some(Some(usage(10, 0, 0, None)))), false);
        assert_eq!(t.input, u64::MAX);
    }
}
