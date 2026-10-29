//! Relevé d'un appel LLM : envois HTTP, réponse reçue et `usage` normalisé (REQ-165, DEC-134).

use std::cell::Cell;

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

/// Relevé d'un appel : le fil de l'appel l'écrit, l'appelant le lit après coup.
#[derive(Debug, Default)]
pub struct CallStats {
    sends: Cell<u32>,
    responded: Cell<bool>,
    usage: Cell<Option<Usage>>,
}

impl CallStats {
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
    pub(crate) fn add_send(&self) {
        self.sends.set(self.sends.get().saturating_add(1));
    }
    pub(crate) fn record_response(&self, usage: Option<Usage>) {
        self.responded.set(true);
        self.usage.set(usage);
    }
}

/// Faux serveur HTTP des tests : une réponse par connexion, dans l'ordre ; la dernière se répète.
#[cfg(test)]
pub(crate) fn fake_server(replies: Vec<(u16, String)>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
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
            let (code, body) = &replies[i.min(replies.len() - 1)];
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    base
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
        s.record_response(Some(Usage::default()));
        assert_eq!((s.sends(), s.responded()), (2, true));
        assert_eq!(s.usage(), Some(Usage::default()));
    }
}
