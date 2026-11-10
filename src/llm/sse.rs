//! Lecture d'une réponse LLM en flux SSE, assemblée en corps JSON (REQ-249, REQ-250, REQ-251).

use std::io::{self, BufRead};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

/// Échec de la lecture d'un flux.
#[derive(Debug)]
pub enum SseError {
    /// Plafond de durée dépassé (REQ-250).
    Cap,
    /// Événement `error` `overloaded_error` : traité comme un HTTP 529 (REQ-251).
    Overloaded,
    /// Autre événement ou morceau d'erreur : traité comme un HTTP 500 (REQ-251).
    Api,
    /// Erreur d'E/S ; `UnexpectedEof` pour un flux qui finit avant sa fin.
    Io(io::Error),
}

impl From<io::Error> for SseError {
    fn from(e: io::Error) -> Self {
        SseError::Io(e)
    }
}

/// Lit le flux jusqu'à sa fin et rend le corps JSON de la réponse non diffusée équivalente.
/// `provider` : `"anthropic"`, sinon format OpenAI. `cap` est mesuré depuis `start`, vérifié
/// après chaque ligne.
pub fn read_body(
    mut reader: impl BufRead,
    provider: &str,
    cap: Duration,
    start: Instant,
) -> Result<String, SseError> {
    let anthropic = provider == "anthropic";
    let mut asm = Assembler::default();
    let mut data: Vec<String> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if start.elapsed() > cap {
            return Err(SseError::Cap);
        }
        if n == 0 {
            // Fin du corps : complet seulement si la fin du flux a été vue.
            return match asm.finish(anthropic) {
                Some(body) => Ok(body),
                None => Err(SseError::Io(io::ErrorKind::UnexpectedEof.into())),
            };
        }
        let text = line.strip_suffix('\n').unwrap_or(&line);
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.is_empty() {
            if !data.is_empty() {
                let payload = data.join("\n");
                data.clear();
                let done = if anthropic {
                    asm.anthropic(&payload)?
                } else {
                    asm.openai(&payload)?
                };
                if done && anthropic {
                    // Le corps est lu jusqu'à sa fin pour que la connexion serve encore (REQ-053).
                    drain(&mut reader, cap, start)?;
                    return Ok(asm.finish(true).unwrap_or_default());
                }
            }
        } else if let Some(value) = text.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
        // Commentaires, `event:` et champs inconnus : ignorés.
    }
}

fn drain(reader: &mut impl BufRead, cap: Duration, start: Instant) -> Result<(), SseError> {
    let mut sink = String::new();
    loop {
        sink.clear();
        if reader.read_line(&mut sink)? == 0 {
            return Ok(());
        }
        if start.elapsed() > cap {
            return Err(SseError::Cap);
        }
    }
}

#[derive(Default)]
struct Assembler {
    text: String,
    /// Anthropic : indice du premier bloc `text`.
    text_block: Option<i64>,
    has_content: bool,
    /// OpenAI : `delta.refusal` mis bout à bout (absent si aucun morceau).
    refusal: Option<String>,
    stop: Value,
    usage: Value,
    done: bool,
}

impl Assembler {
    /// Un événement Anthropic ; `true` à `message_stop`.
    fn anthropic(&mut self, payload: &str) -> Result<bool, SseError> {
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            return Ok(false);
        };
        match v["type"].as_str() {
            Some("message_start") => self.usage = v["message"]["usage"].clone(),
            Some("content_block_start") => {
                if self.text_block.is_none() && v["content_block"]["type"] == "text" {
                    self.text_block = v["index"].as_i64();
                    if let Some(t) = v["content_block"]["text"].as_str() {
                        self.text.push_str(t);
                    }
                }
            }
            Some("content_block_delta") => {
                if self.text_block.is_some()
                    && v["index"].as_i64() == self.text_block
                    && v["delta"]["type"] == "text_delta"
                {
                    if let Some(t) = v["delta"]["text"].as_str() {
                        self.text.push_str(t);
                    }
                }
            }
            Some("message_delta") => {
                if !v["delta"]["stop_reason"].is_null() {
                    self.stop = v["delta"]["stop_reason"].clone();
                }
                if let Some(new) = v["usage"].as_object() {
                    let mut merged: Map<String, Value> =
                        self.usage.as_object().cloned().unwrap_or_default();
                    merged.extend(new.clone());
                    self.usage = Value::Object(merged);
                }
            }
            Some("message_stop") => {
                self.done = true;
                return Ok(true);
            }
            Some("error") => {
                return Err(if v["error"]["type"] == "overloaded_error" {
                    SseError::Overloaded
                } else {
                    SseError::Api
                })
            }
            _ => {}
        }
        Ok(false)
    }

    /// Un morceau OpenAI ; `true` à `[DONE]`.
    fn openai(&mut self, payload: &str) -> Result<bool, SseError> {
        if payload.trim() == "[DONE]" {
            self.done = true;
            return Ok(true);
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            return Ok(false);
        };
        if v["error"].is_object() {
            return Err(SseError::Api);
        }
        let choice = &v["choices"][0];
        if let Some(t) = choice["delta"]["content"].as_str() {
            self.text.push_str(t);
            self.has_content = true;
        }
        if let Some(t) = choice["delta"]["refusal"].as_str() {
            self.refusal.get_or_insert_with(String::new).push_str(t);
        }
        if !choice["finish_reason"].is_null() {
            self.stop = choice["finish_reason"].clone();
        }
        if !v["usage"].is_null() {
            self.usage = v["usage"].clone();
        }
        Ok(false)
    }

    /// Le corps assemblé, ou `None` si le flux n'a pas atteint sa fin.
    fn finish(&self, anthropic: bool) -> Option<String> {
        if anthropic {
            if !self.done {
                return None;
            }
            let content = if self.text_block.is_some() {
                json!([{"type": "text", "text": self.text}])
            } else {
                json!([])
            };
            Some(
                json!({"content": content, "stop_reason": self.stop, "usage": self.usage})
                    .to_string(),
            )
        } else {
            if !self.done && self.stop.is_null() {
                return None;
            }
            let content = if self.has_content {
                json!(self.text)
            } else {
                Value::Null
            };
            let mut message = json!({"content": content});
            if let Some(r) = &self.refusal {
                message["refusal"] = json!(r);
            }
            Some(
                json!({"choices": [{"message": message, "finish_reason": self.stop}],
                    "usage": self.usage})
                .to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor, Read};

    const FAR: Duration = Duration::from_secs(3600);

    /// Rend un octet par `read`.
    struct OneByte(Cursor<Vec<u8>>);
    impl Read for OneByte {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(1);
            self.0.read(&mut buf[..n])
        }
    }

    /// Dort 20 ms, puis rend une ligne entière par `read`.
    struct Slow(Vec<u8>, usize);
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(20));
            let rest = &self.0[self.1..];
            let end = rest
                .iter()
                .position(|&b| b == b'\n')
                .map_or(rest.len(), |p| p + 1);
            let n = end.min(buf.len());
            buf[..n].copy_from_slice(&rest[..n]);
            self.1 += n;
            Ok(n)
        }
    }

    fn ev(data: &str) -> String {
        format!("data: {data}\n\n")
    }

    fn anthropic_stream(stop: &str, with_text: bool) -> String {
        let mut s = String::new();
        s += ": commentaire\n\n";
        s += &ev(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        );
        s += "event: inconnu\ndata: {\"type\":\"autre\"}\n\n";
        s += &ev(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#);
        s += &ev(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#,
        );
        s += &ev(r#"{"type":"ping"}"#);
        if with_text {
            s += &ev(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            );
            // Deux lignes `data:` d'un même événement, jointes par `\n` (JSON valide).
            s += "data: {\"type\":\"content_block_delta\",\"index\":1,\n";
            s += "data: \"delta\":{\"type\":\"text_delta\",\"text\":\"ab\"}}\n\n";
            s += &ev(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"cd"}}"#,
            );
        }
        s += &ev(&format!(
            r#"{{"type":"message_delta","delta":{{"stop_reason":"{stop}"}},"usage":{{"output_tokens":7}}}}"#
        ));
        s += &ev(r#"{"type":"message_stop"}"#);
        s
    }

    fn openai_stream(finish: &str, done: bool) -> String {
        let mut s = ev(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#);
        s += &ev(r#"{"choices":[{"delta":{"content":"ab"}}]}"#);
        s += &ev(r#"{"choices":[{"delta":{"content":"cd"}}]}"#);
        s += &ev(&format!(
            r#"{{"choices":[{{"delta":{{}},"finish_reason":"{finish}"}}]}}"#
        ));
        s += &ev(r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#);
        if done {
            s += "data: [DONE]\n\n";
        }
        s
    }

    fn read(text: &str, provider: &str) -> Result<Value, SseError> {
        read_body(
            Cursor::new(text.as_bytes().to_vec()),
            provider,
            FAR,
            Instant::now(),
        )
        .map(|b| serde_json::from_str(&b).unwrap())
    }

    #[test]
    fn req249_ac5_same_result_whole_byte_by_byte_and_crlf() {
        for provider in ["anthropic", "openai"] {
            let text = if provider == "anthropic" {
                anthropic_stream("end_turn", true)
            } else {
                openai_stream("stop", true)
            };
            let whole = read(&text, provider).unwrap();
            let bytes = BufReader::new(OneByte(Cursor::new(text.clone().into_bytes())));
            let one = read_body(bytes, provider, FAR, Instant::now()).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&one).unwrap(), whole);
            let crlf = read(&text.replace('\n', "\r\n"), provider).unwrap();
            assert_eq!(crlf, whole);
            if provider == "anthropic" {
                assert_eq!(whole["content"][0]["text"], "abcd");
                assert_eq!(whole["stop_reason"], "end_turn");
                assert_eq!(whole["usage"]["input_tokens"], 10);
                assert_eq!(whole["usage"]["output_tokens"], 7);
            } else {
                assert_eq!(whole["choices"][0]["message"]["content"], "abcd");
                assert_eq!(whole["choices"][0]["finish_reason"], "stop");
                assert_eq!(whole["usage"]["completion_tokens"], 2);
            }
        }
    }

    #[test]
    fn req249_ac4_truncation_and_missing_text_in_assembled_body() {
        let v = read(&anthropic_stream("max_tokens", true), "anthropic").unwrap();
        assert_eq!(v["stop_reason"], "max_tokens");
        assert_eq!(v["content"][0]["text"], "abcd");
        let v = read(&openai_stream("length", true), "openai").unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "length");
        assert_eq!(v["choices"][0]["message"]["content"], "abcd");
        let v = read(&anthropic_stream("end_turn", false), "anthropic").unwrap();
        assert_eq!(v["content"], json!([]));
    }

    #[test]
    fn req249_ac4_openai_without_content_is_null() {
        let text = ev(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        let v = read(&text, "openai").unwrap();
        assert!(v["choices"][0]["message"]["content"].is_null());
    }

    #[test]
    fn req274_ac3_refusal_deltas_are_concatenated_and_absent_otherwise() {
        let text = ev(r#"{"choices":[{"delta":{"refusal":"I can"}}]}"#)
            + &ev(r#"{"choices":[{"delta":{"refusal":"'t"}}]}"#)
            + &ev(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        let v = read(&text, "openai").unwrap();
        assert_eq!(v["choices"][0]["message"]["refusal"], "I can't");
        let v = read(&openai_stream("stop", true), "openai").unwrap();
        assert!(v["choices"][0]["message"].get("refusal").is_none());
        let v = read(&anthropic_stream("refusal", false), "anthropic").unwrap();
        assert_eq!(v["stop_reason"], "refusal");
    }

    #[test]
    fn req250_ac4_cap_is_hit_while_lines_keep_coming() {
        let text = ": x\n".repeat(50);
        let reader = BufReader::new(Slow(text.into_bytes(), 0));
        let t = Instant::now();
        let r = read_body(
            reader,
            "anthropic",
            Duration::from_millis(100),
            Instant::now(),
        );
        assert!(matches!(r, Err(SseError::Cap)));
        assert!(t.elapsed() < Duration::from_millis(300));
    }

    #[test]
    fn req251_overloaded_is_529_and_other_errors_are_500() {
        let over = ev(r#"{"type":"message_start","message":{}}"#)
            + &ev(r#"{"type":"error","error":{"type":"overloaded_error","message":"x"}}"#);
        assert!(matches!(
            read(&over, "anthropic"),
            Err(SseError::Overloaded)
        ));
        let api = ev(r#"{"type":"error","error":{"type":"api_error","message":"x"}}"#);
        assert!(matches!(read(&api, "anthropic"), Err(SseError::Api)));
        let chunk = ev(r#"{"error":{"message":"x"}}"#);
        assert!(matches!(read(&chunk, "openai"), Err(SseError::Api)));
    }

    #[test]
    fn req251_end_before_the_end_of_the_stream_is_incomplete() {
        let cut =
            anthropic_stream("end_turn", true).replace("data: {\"type\":\"message_stop\"}\n\n", "");
        let eof = |r: Result<Value, SseError>| match r {
            Err(SseError::Io(e)) => e.kind() == io::ErrorKind::UnexpectedEof,
            _ => false,
        };
        assert!(eof(read(&cut, "anthropic")));
        let no_finish = ev(r#"{"choices":[{"delta":{"content":"a"}}]}"#);
        assert!(eof(read(&no_finish, "openai")));
        assert!(eof(read("", "openai")));
    }

    #[test]
    fn req251_openai_without_done_but_with_finish_reason_is_complete() {
        let v = read(&openai_stream("stop", false), "openai").unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "abcd");
        assert_eq!(v["usage"]["prompt_tokens"], 5);
    }
}
