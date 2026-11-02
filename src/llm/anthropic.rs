//! Adaptateur Anthropic Messages (REQ-002).

use super::schema::Reply;
use super::{anthropic_usage, http, CallStats, Complete, Generation, Retry, Structured, Tls};
use crate::config::Effort;
use crate::error::{Error, Result, TRUNCATED};
use std::sync::atomic::{AtomicBool, Ordering};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Vrai si `model` est un ancien modèle Claude, qui refuse `output_config.effort` (REQ-157).
///
/// Normalisation : minuscules, `.`, `@`, `:` et `_` remplacés par `-`. On lit ce qui suit la
/// dernière occurrence de `claude-`, découpé sur `-`. Liste fermée : seuls les modèles sortis
/// avant Opus 4.5 sont anciens.
pub fn is_legacy_claude(model: &str) -> bool {
    let norm: String = model
        .to_lowercase()
        .chars()
        .map(|c| {
            if matches!(c, '.' | '@' | ':' | '_') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let Some(at) = norm.rfind("claude-") else {
        return false;
    };
    let segs: Vec<&str> = norm[at + "claude-".len()..].split('-').collect();
    let number = |s: &str| -> Option<u32> {
        if !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let s0 = segs[0];
    if let Some(n) = number(s0) {
        return n <= 3;
    }
    if s0 == "instant" {
        return true;
    }
    if !matches!(s0, "opus" | "sonnet" | "haiku") {
        return false;
    }
    let Some(major) = segs.get(1).and_then(|s| number(s)) else {
        return false;
    };
    let minor = segs
        .get(2)
        .filter(|s| s.len() <= 2)
        .and_then(|s| number(s))
        .unwrap_or(0);
    if s0 == "opus" && major == 4 && minor == 5 {
        return false;
    }
    major <= 3 || (major == 4 && minor <= 5)
}

/// Corps de la requête `/v1/messages` (REQ-058, REQ-157, DEC-126) : jamais de `thinking`
/// désactivé ; `thinking` adaptatif si demandé ; `output_config.effort` sauf `effort = "default"`
/// ou ancien modèle Claude sans `thinking` ; `output_config.format` porte le schéma s'il est donné
/// (REQ-202).
pub fn request_body(
    model: &str,
    gen: &Generation,
    system: &str,
    user: &str,
    format: Option<(&str, &serde_json::Value)>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": gen.max_output_tokens,
        "system": system,
        "messages": [{"role": "user", "content": user}],
    });
    if gen.thinking {
        body["thinking"] = serde_json::json!({"type": "adaptive"});
    }
    if gen.effort != Effort::ModelDefault && (gen.thinking || !is_legacy_claude(model)) {
        body["output_config"] = serde_json::json!({"effort": gen.effort.as_str()});
    }
    if let Some((_, schema)) = format {
        body["output_config"]["format"] =
            serde_json::json!({"type": "json_schema", "schema": schema});
    }
    body
}

pub struct AnthropicLlm {
    base_url: Option<String>,
    model: String,
    key: String,
    agent: ureq::Agent,
    retry: http::Retry,
    gen: Generation,
    structured: Structured,
    structured_key: &'static str,
    fallback: AtomicBool,
    summary_language: String,
}

impl AnthropicLlm {
    pub fn new(
        base_url: Option<String>,
        model: String,
        key: String,
        timeout_secs: u64,
        tls: &Tls,
        concurrency: usize,
        gen: Generation,
    ) -> AnthropicLlm {
        AnthropicLlm {
            base_url,
            model,
            key,
            agent: http::agent(timeout_secs, concurrency, tls),
            retry: http::Retry::default(),
            gen,
            structured: Structured::Off,
            structured_key: "structured_output",
            fallback: AtomicBool::new(false),
            summary_language: crate::config::DEFAULT_SUMMARY_LANGUAGE.to_string(),
        }
    }

    /// Réglage effectif de `structured_output` (REQ-202).
    pub fn with_structured(mut self, structured: Structured) -> AnthropicLlm {
        self.structured = structured;
        self
    }

    /// Clé qui a donné le réglage, nommée par la trace de repli (REQ-203).
    pub fn with_structured_key(mut self, key: &'static str) -> AnthropicLlm {
        self.structured_key = key;
        self
    }

    /// Langue des résumés demandée dans les prompts système (REQ-172).
    pub fn with_summary_language(mut self, language: String) -> AnthropicLlm {
        self.summary_language = language;
        self
    }

    /// Base d'attente des nouveaux essais HTTP (REQ-052).
    pub fn with_retry(mut self, retry: Retry) -> AnthropicLlm {
        self.retry = retry;
        self
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &http::url(&self.base_url, DEFAULT_BASE_URL, path))
            .set("x-api-key", &self.key)
            .set("anthropic-version", API_VERSION)
    }
}

impl Complete for AnthropicLlm {
    fn summary_language(&self) -> &str {
        &self.summary_language
    }

    /// Appelle `/v1/messages` et retourne le texte de la réponse.
    fn complete(
        &self,
        label: Option<&str>,
        stop: Option<&AtomicBool>,
        stats: &CallStats,
        system: String,
        user: String,
        reply: Reply,
    ) -> Result<String> {
        let send = |with_schema: bool| {
            let schema = with_schema.then(|| reply.schema());
            let format = schema.as_ref().map(|s| (reply.name(), s));
            let body = request_body(&self.model, &self.gen, &system, &user, format);
            http::send(
                "anthropic",
                self.request("POST", "/v1/messages"),
                Some(&body),
                &self.retry,
                label,
                stop,
                stats,
            )
        };
        let with_schema = match self.structured {
            Structured::Off => false,
            Structured::On => true,
            Structured::Auto => !self.fallback.load(Ordering::SeqCst),
        };
        let mut sent = send(with_schema);
        if let Err(e) = &sent {
            if with_schema
                && http::schema_refused(
                    "anthropic",
                    e,
                    self.structured,
                    &self.fallback,
                    self.structured_key,
                )
            {
                sent = send(false);
            }
        }
        let raw = sent?;
        let v = http::parse_body("anthropic", &raw)?;
        stats.record_response(anthropic_usage(&v));
        if v["stop_reason"] == "max_tokens" {
            return Err(Error::Truncated(TRUNCATED.into()));
        }
        v["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .map(String::from)
            .ok_or_else(|| Error::Format("réponse de anthropic sans bloc texte".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn gen(thinking: bool, effort: Effort, max: u32) -> Generation {
        Generation {
            effort,
            effort_explicit: false,
            thinking,
            max_output_tokens: max,
        }
    }

    fn base_fields() -> serde_json::Value {
        serde_json::json!({
            "model": "claude-opus-5-5", "max_tokens": 1234, "system": "s",
            "messages": [{"role": "user", "content": "u"}],
        })
    }

    #[test]
    fn req157_ac1_body_without_thinking_sends_effort_on_current_model() {
        let m = "claude-opus-5-5";
        let b = request_body(m, &gen(false, Effort::Low, 1234), "s", "u", None);
        let mut want = base_fields();
        want["output_config"] = serde_json::json!({"effort": "low"});
        assert_eq!(b, want);
        assert_eq!(b.as_object().unwrap().len(), 5);
        assert!(b.get("thinking").is_none());

        let b = request_body(m, &gen(false, Effort::High, 1234), "s", "u", None);
        assert_eq!(b["output_config"], serde_json::json!({"effort": "high"}));
        assert!(b.get("thinking").is_none());

        let b = request_body(m, &gen(false, Effort::ModelDefault, 1234), "s", "u", None);
        assert_eq!(b, base_fields());
    }

    #[test]
    fn req157_ac1_body_with_thinking_default_has_only_thinking() {
        let b = request_body(
            "claude-opus-5-5",
            &gen(true, Effort::ModelDefault, 1234),
            "s",
            "u",
            None,
        );
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert!(b.get("output_config").is_none());
        assert_eq!(b.as_object().unwrap().len(), 5);
    }

    #[test]
    fn req157_ac1_legacy_model_gets_no_effort_without_thinking() {
        let m = "claude-sonnet-4-5";
        let b = request_body(m, &gen(false, Effort::High, 1234), "s", "u", None);
        let mut want = base_fields();
        want["model"] = m.into();
        assert_eq!(b, want);
        let b = request_body(m, &gen(true, Effort::Medium, 1234), "s", "u", None);
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(b["output_config"], serde_json::json!({"effort": "medium"}));
        assert_eq!(b.as_object().unwrap().len(), 6);
    }

    #[test]
    fn body_with_thinking_adds_adaptive_and_effort() {
        let b = request_body("m", &gen(true, Effort::Medium, 16384), "s", "u", None);
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(b["output_config"], serde_json::json!({"effort": "medium"}));
        assert_eq!(b["max_tokens"], 16384);
        assert_eq!(b.as_object().unwrap().len(), 6);
    }

    #[test]
    fn req157_ac2_is_legacy_claude() {
        let legacy = [
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-1",
            "claude-opus-4-1-20250805",
            "claude-opus-4-0",
            "claude-opus-4-20250514",
            "claude-opus-4",
            "claude-sonnet-4-0",
            "claude-sonnet-4-20250514",
            "claude-sonnet-4",
            "claude-3-7-sonnet-latest",
            "claude-3-5-haiku-20241022",
            "claude-3-opus-20240229",
            "claude-3-haiku-20240307",
            "claude-2.1",
            "claude-instant-1.2",
            "CLAUDE-SONNET-4-5",
            "claude-sonnet-4.5",
            "anthropic/claude-sonnet-4.5",
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "claude-opus-4@20250514",
        ];
        for m in legacy {
            assert!(is_legacy_claude(m), "{m} devrait etre ancien");
        }
        let current = [
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-opus-5-5",
            "claude-sonnet-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "claude-mythos-preview",
            "claude-x",
            "m",
            "x",
            "mock",
            "gpt-4o-mini",
            "",
        ];
        for m in current {
            assert!(!is_legacy_claude(m), "{m:?} ne devrait pas etre ancien");
        }
    }

    use crate::llm::usage::{anthropic_usage, fake_server, CallStats, Usage};
    use serde_json::json;

    fn au(u: serde_json::Value) -> Option<Usage> {
        anthropic_usage(&json!({ "usage": u }))
    }

    #[test]
    fn req165_ac1_anthropic_usage_is_normalized() {
        let full = au(json!({"input_tokens":100,"output_tokens":20,
            "cache_read_input_tokens":30,"cache_creation_input_tokens":5}));
        assert_eq!(
            full,
            Some(Usage {
                input: 135,
                cache_read: 30,
                cache_write: 5,
                output: 20,
                reasoning: None
            })
        );
        let plain = au(json!({"input_tokens":100,"output_tokens":20}));
        assert_eq!(
            plain,
            Some(Usage {
                input: 100,
                cache_read: 0,
                cache_write: 0,
                output: 20,
                reasoning: None
            })
        );
        let null =
            au(json!({"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":null}));
        assert_eq!(null, plain);
        let text =
            au(json!({"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":"5"}));
        assert_eq!(text, plain);
    }

    #[test]
    fn req165_ac1_anthropic_usage_missing_or_invalid_gives_none() {
        assert_eq!(anthropic_usage(&json!({})), None);
        assert_eq!(anthropic_usage(&json!({"usage": null})), None);
        assert_eq!(au(json!({"output_tokens":20})), None);
        for bad in [json!("100"), json!(-1), json!(1.5)] {
            assert_eq!(
                au(json!({"input_tokens":bad,"output_tokens":20})),
                None,
                "{bad}"
            );
        }
    }

    #[test]
    fn req165_ac1_anthropic_sums_saturate() {
        let u = au(json!({"input_tokens":u64::MAX,"output_tokens":1,"cache_read_input_tokens":5}));
        assert_eq!(u.unwrap().input, u64::MAX);
    }

    fn llm(base: String) -> AnthropicLlm {
        AnthropicLlm::new(
            Some(base),
            "m".into(),
            "k".into(),
            10,
            &Tls::default(),
            1,
            gen(false, Effort::Low, 100),
        )
    }

    #[test]
    fn req165_ac3_truncated_and_textless_answers_are_still_measured() {
        let usage = r#""usage":{"input_tokens":10,"output_tokens":3,"cache_read_input_tokens":4}"#;
        let want = Usage {
            input: 14,
            cache_read: 4,
            cache_write: 0,
            output: 3,
            reasoning: None,
        };
        let cases = [
            (
                format!(r#"{{"stop_reason":"max_tokens","content":[],{usage}}}"#),
                true,
            ),
            (
                format!(r#"{{"stop_reason":"end_turn","content":[],{usage}}}"#),
                false,
            ),
        ];
        for (body, truncated) in cases {
            let l = llm(fake_server(vec![(200, body)]));
            let stats = CallStats::default();
            let e = l
                .complete(None, None, &stats, "s".into(), "u".into(), Reply::File)
                .unwrap_err();
            if truncated {
                assert!(matches!(&e, Error::Truncated(m) if m == TRUNCATED), "{e:?}");
            } else {
                assert!(
                    matches!(&e, Error::Format(m) if m == "réponse de anthropic sans bloc texte"),
                    "{e:?}"
                );
            }
            assert!(stats.responded());
            assert_eq!(stats.usage(), Some(want));
            assert_eq!(stats.sends(), 1);
        }
    }

    #[test]
    fn req202_ac4_corps_anthropic_avec_schema() {
        let schema = crate::llm::schema::file();
        let fmt = json!({"type": "json_schema", "schema": schema});
        let f = Some(("nephila_fichier", &schema));
        let b = request_body(
            "claude-opus-5-5",
            &gen(false, Effort::ModelDefault, 1234),
            "s",
            "u",
            f,
        );
        let mut want = base_fields();
        want["output_config"] = json!({"format": fmt});
        assert_eq!(b, want);
        let b = request_body(
            "claude-opus-5-5",
            &gen(false, Effort::Low, 1234),
            "s",
            "u",
            f,
        );
        want["output_config"] = json!({"effort": "low", "format": fmt});
        assert_eq!(b, want);
        let b = request_body(
            "claude-sonnet-4-5",
            &gen(false, Effort::High, 1234),
            "s",
            "u",
            f,
        );
        assert_eq!(b["output_config"], json!({"format": fmt}));
        assert!(b.get("thinking").is_none());
        let b = request_body(
            "claude-opus-5-5",
            &gen(true, Effort::Medium, 1234),
            "s",
            "u",
            f,
        );
        assert_eq!(
            b["output_config"],
            json!({"effort": "medium", "format": fmt})
        );
        assert_eq!(b["thinking"], json!({"type": "adaptive"}));
    }

    use crate::llm::usage::recording_server;

    const REFUSED_ANTHROPIC: &str =
        r#"{"error":{"message":"output_config.format: not supported for this model"}}"#;
    const OK_ANTHROPIC: &str =
        r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"x"}]}"#;

    fn llm_structured(base: String, structured: Structured, key: &'static str) -> AnthropicLlm {
        AnthropicLlm::new(
            Some(base),
            "m".into(),
            "k".into(),
            10,
            &Tls::default(),
            2,
            gen(false, Effort::Low, 100),
        )
        .with_structured(structured)
        .with_structured_key(key)
    }

    fn expected_body(with_schema: bool) -> String {
        let schema = Reply::File.schema();
        let format = with_schema.then(|| (Reply::File.name(), &schema));
        request_body("m", &gen(false, Effort::Low, 100), "s", "u", format).to_string()
    }

    fn same_json(a: &str, b: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(a).unwrap()
            == serde_json::from_str::<serde_json::Value>(b).unwrap()
    }

    fn call(l: &AnthropicLlm, stats: &CallStats) -> Result<String> {
        l.complete(None, None, stats, "s".into(), "u".into(), Reply::File)
    }

    fn rejected_code(e: &Error) -> u16 {
        match e {
            Error::Rejected { code, .. } => *code,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn req203_ac1_refus_du_schema_puis_appels_sans_schema() {
        let (base, bodies) = recording_server(
            vec![(400, REFUSED_ANTHROPIC.into()), (200, OK_ANTHROPIC.into())],
            0,
        );
        let l = llm_structured(base, Structured::Auto, "structured_output");
        for _ in 0..3 {
            let stats = CallStats::default();
            assert_eq!(call(&l, &stats).unwrap(), "x");
        }
        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 4);
        assert!(same_json(&b[0], &expected_body(true)));
        for later in &b[1..] {
            assert!(same_json(later, &expected_body(false)), "{later}");
        }
        assert_ne!(expected_body(true), expected_body(false));
    }

    #[test]
    fn req203_ac2_refus_qui_ne_replie_pas() {
        let cases: [(u16, &str, Structured); 4] = [
            (400, r#"{"error":{}}"#, Structured::Auto),
            (
                400,
                r#"{"error":{"message":"model unknown"}}"#,
                Structured::Auto,
            ),
            (
                422,
                r#"{"error":{"message":"bad schema"}}"#,
                Structured::Auto,
            ),
            (400, r#"{"error":{"message":"bad schema"}}"#, Structured::On),
        ];
        for (code, body, structured) in cases {
            let (base, bodies) = recording_server(vec![(code, body.into())], 0);
            let l = llm_structured(base, structured, "structured_output");
            let stats = CallStats::default();
            let (r, events) = crate::trace::capture(|| call(&l, &stats));
            assert_eq!(rejected_code(&r.unwrap_err()), code, "{body}");
            assert_eq!(bodies.lock().unwrap().len(), 1, "{code} {body}");
            assert!(events.is_empty(), "{events:?}");
        }
    }

    #[test]
    fn req203_ac2_mots_cites_sans_casse() {
        for message in ["Bad FORMAT", "invalid Schema here"] {
            let body = format!(r#"{{"error":{{"message":"{message}"}}}}"#);
            let (base, bodies) = recording_server(vec![(400, body), (200, OK_ANTHROPIC.into())], 0);
            let l = llm_structured(base, Structured::Auto, "structured_output");
            assert_eq!(call(&l, &CallStats::default()).unwrap(), "x", "{message}");
            assert_eq!(bodies.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn req203_ac3_renvoi_refuse_a_son_tour() {
        let (base, bodies) = recording_server(vec![(400, REFUSED_ANTHROPIC.into())], 0);
        let l = llm_structured(base, Structured::Auto, "structured_output");
        let e = call(&l, &CallStats::default()).unwrap_err();
        assert_eq!(rejected_code(&e), 400);
        assert!(e
            .to_string()
            .starts_with("requête refusée par anthropic (HTTP 400) sur "));
        assert_eq!(bodies.lock().unwrap().len(), 2);
    }

    #[test]
    fn req203_ac4_deux_fils_refuses_une_seule_trace() {
        let (base, bodies) = recording_server(
            vec![
                (400, REFUSED_ANTHROPIC.into()),
                (400, REFUSED_ANTHROPIC.into()),
                (200, OK_ANTHROPIC.into()),
            ],
            2,
        );
        let l = llm_structured(base, Structured::Auto, "structured_output");
        let traces: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        let (r, events) = crate::trace::capture(|| call(&l, &CallStats::default()));
                        assert_eq!(r.unwrap(), "x");
                        events.len()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(traces, 1);
        assert_eq!(call(&l, &CallStats::default()).unwrap(), "x");
        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 5);
        assert!(b[..2].iter().all(|x| same_json(x, &expected_body(true))));
        assert!(b[2..].iter().all(|x| same_json(x, &expected_body(false))));
    }

    #[test]
    fn req203_ac5_comptes_d_un_appel_replie() {
        let (base, _) = recording_server(
            vec![(400, REFUSED_ANTHROPIC.into()), (200, OK_ANTHROPIC.into())],
            0,
        );
        let l = llm_structured(base, Structured::Auto, "structured_output");
        let stats = CallStats::default();
        assert_eq!(call(&l, &stats).unwrap(), "x");
        assert_eq!(stats.sends(), 2);
        assert!(stats.responded());
    }

    #[test]
    fn req203_ac6_trace_de_repli() {
        for key in ["structured_output", "ask.structured_output"] {
            let (base, _) = recording_server(
                vec![(400, REFUSED_ANTHROPIC.into()), (200, OK_ANTHROPIC.into())],
                0,
            );
            let l = llm_structured(base, Structured::Auto, key);
            let (r, events) = crate::trace::capture(|| {
                call(&l, &CallStats::default()).unwrap();
                call(&l, &CallStats::default()).unwrap()
            });
            assert_eq!(r, "x");
            assert_eq!(
                events,
                [format!("sortie structurée: schéma JSON refusé par anthropic (HTTP 400), suite de la commande sans schéma ({key} = \"auto\")")]
            );
        }
    }
}
