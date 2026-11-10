//! Adaptateur Anthropic Messages (REQ-002).

use super::batch::{Answer, BatchApi, BatchLink, Collector, CreateError, LotStatus, Pending};
use super::schema::Reply;
use super::{
    anthropic_usage, http, served_model, CallStats, Complete, Generation, Retry, Structured, Tls,
};
use crate::config::Effort;
use crate::error::{Error, Result, BLOCKED, TRUNCATED};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Vrai si `model` est un ancien modèle Claude, qui refuse `output_config.effort` (REQ-157).
///
/// Normalisation : minuscules, `.`, `@`, `:` et `_` remplacés par `-`. On lit ce qui suit la
/// dernière occurrence de `claude-`, découpé sur `-`. Liste fermée : seuls les modèles sortis
/// avant Opus 4.5 sont anciens.
pub fn is_legacy_claude(model: &str) -> bool {
    let Some(segs) = claude_segments(model) else {
        return false;
    };
    let s0 = segs[0].as_str();
    if let Some(n) = number(s0) {
        return n <= 3;
    }
    if s0 == "instant" {
        return true;
    }
    if !matches!(s0, "opus" | "sonnet" | "haiku") {
        return false;
    }
    let (major, minor) = version(&segs);
    let Some(major) = major else {
        return false;
    };
    if s0 == "opus" && major == 4 && minor == 5 {
        return false;
    }
    major <= 3 || (major == 4 && minor <= 5)
}

/// Segments qui suivent la dernière occurrence de `claude-` dans le nom normalisé du modèle
/// (minuscules, `.`, `@`, `:` et `_` remplacés par `-`), découpés sur `-`.
fn claude_segments(model: &str) -> Option<Vec<String>> {
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
    let at = norm.rfind("claude-")?;
    Some(
        norm[at + "claude-".len()..]
            .split('-')
            .map(String::from)
            .collect(),
    )
}

fn number(s: &str) -> Option<u32> {
    if !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// Version majeure et mineure (0 si absente ou si le segment est une date) d'après les segments.
fn version(segs: &[String]) -> (Option<u32>, u32) {
    let major = segs.get(1).and_then(|s| number(s));
    let minor = segs
        .get(2)
        .filter(|s| s.len() <= 2)
        .and_then(|s| number(s))
        .unwrap_or(0);
    (major, minor)
}

/// Plus petit nombre de jetons qu'un préfixe doit compter pour être mis en cache (REQ-268,
/// documentation Anthropic relevée le 2026-11-09). Modèle inconnu : 1 024.
pub fn min_cacheable_tokens(model: &str) -> usize {
    let Some(segs) = claude_segments(model) else {
        return 1024;
    };
    if segs[0] == "mythos" && segs.get(1).is_some_and(|s| s == "preview") {
        return 2048;
    }
    match (segs[0].as_str(), version(&segs)) {
        ("opus", (Some(5), 0 | 5))
        | ("sonnet" | "haiku", (Some(5), 5))
        | ("fable" | "mythos", (Some(5), 0 | 1)) => 512,
        ("opus", (Some(4), 7)) => 2048,
        ("opus", (Some(4), 5 | 6)) | ("haiku", (Some(4), 5)) => 4096,
        _ => 1024,
    }
}

/// `content` du premier message en deux blocs de texte, coupés à `upto` octets ; le premier porte
/// `cache_control` (REQ-268). Coupe hors du texte, hors d'une frontière de caractère ou sans
/// reste : le texte seul, inchangé.
fn cached_content(user: &str, upto: usize) -> serde_json::Value {
    match (user.get(..upto), user.get(upto..)) {
        (Some(head), Some(rest)) if !head.is_empty() && !rest.is_empty() => serde_json::json!([
            {"type": "text", "text": head, "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": rest},
        ]),
        _ => serde_json::json!(user),
    }
}

/// Corps de la requête `/v1/messages` (REQ-058, REQ-157, DEC-126) : jamais de `thinking`
/// désactivé ; `thinking` adaptatif si demandé ; `output_config.effort` sauf `effort = "default"`
/// ou ancien modèle Claude sans `thinking` ; `output_config.format` porte le schéma s'il est donné
/// (REQ-202). `temperature` part si elle est écrite et que `thinking` est faux (REQ-275).
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
    if let (Some(t), false) = (gen.temperature, gen.thinking) {
        body["temperature"] = t.into();
    }
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
    pause: http::Pause,
    gen: Generation,
    structured: Structured,
    structured_key: &'static str,
    fallback: AtomicBool,
    summary_language: String,
    collector: Option<Arc<Collector>>,
    /// Marqueur `cache_control` permis : faux avec une `base_url` écrite (REQ-268).
    prefix_cache: bool,
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
            prefix_cache: base_url.is_none(),
            base_url,
            model,
            key,
            agent: http::agent(timeout_secs, concurrency, tls),
            retry: http::Retry::default(),
            pause: http::Pause::new(concurrency),
            gen,
            structured: Structured::Off,
            structured_key: "structured_output",
            fallback: AtomicBool::new(false),
            summary_language: crate::config::DEFAULT_SUMMARY_LANGUAGE.to_string(),
            collector: None,
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

    /// Collecteur du mode lot : les réponses viennent de lui au lieu du réseau (DEC-168).
    pub fn with_collector(mut self, collector: Arc<Collector>) -> AnthropicLlm {
        self.collector = Some(collector);
        self
    }

    /// Permet ou interdit le marqueur `cache_control` (tests sur faux serveur).
    #[cfg(test)]
    pub(crate) fn with_prefix_cache(mut self, on: bool) -> AnthropicLlm {
        self.prefix_cache = on;
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

/// Identifiant de lot sûr à placer dans un chemin d'URL.
fn check_id(id: &str) -> Result<&str> {
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        Ok(id)
    } else {
        Err(Error::Format(
            "identifiant de lot de anthropic illisible".into(),
        ))
    }
}

/// Compteur `name` de `request_counts`.
fn count(v: &serde_json::Value, name: &str) -> Result<usize> {
    v["request_counts"][name]
        .as_u64()
        .map(|n| n as usize)
        .ok_or_else(|| Error::Format(format!("état de lot de anthropic sans {name}")))
}

fn failed(cause: &str) -> Answer {
    Answer::Failed {
        cause: cause.into(),
        refused: None,
    }
}

/// Réponse d'une ligne de résultat (`result`) d'un lot (tableau d'A-17).
fn line_answer(result: &serde_json::Value) -> Answer {
    match result["type"].as_str() {
        Some("succeeded") => Answer::Raw(result["message"].to_string()),
        Some("expired") => failed("expirée"),
        Some("canceled") => failed("annulée"),
        Some("errored") => {
            let e = &result["error"];
            let e = if e["error"].is_object() {
                &e["error"]
            } else {
                e
            };
            let kind = http::provider_text(e["type"].as_str().unwrap_or("erreur"));
            let message = http::provider_text(e["message"].as_str().unwrap_or(""));
            let refused = (kind == "invalid_request_error").then(|| message.clone());
            let cause = if message.is_empty() {
                kind
            } else {
                format!("{kind} : {message}")
            };
            Answer::Failed { cause, refused }
        }
        _ => failed("sans réponse"),
    }
}

impl BatchApi for AnthropicLlm {
    fn provider(&self) -> &str {
        "anthropic"
    }

    fn address(&self) -> String {
        self.base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string()
    }

    fn create(
        &self,
        requests: &[Pending],
        stop: &AtomicBool,
    ) -> std::result::Result<(String, usize), CreateError> {
        let items: Vec<serde_json::Value> = requests
            .iter()
            .map(|p| serde_json::json!({"custom_id": p.id, "params": p.body}))
            .collect();
        let body = serde_json::json!({ "requests": items });
        let text = http::send_create(
            "anthropic",
            self.request("POST", "/v1/messages/batches"),
            &body.to_string(),
            "application/json",
            &self.retry,
            stop,
        )?;
        let id = http::parse_body("anthropic", &text)
            .and_then(|v| match v["id"].as_str() {
                Some(id) => check_id(id).map(String::from),
                None => Err(Error::Format(
                    "réponse de anthropic sans identifiant de lot".into(),
                )),
            })
            .map_err(CreateError::Other)?;
        Ok((id, body.to_string().len()))
    }

    fn status(&self, id: &str, stop: &AtomicBool) -> Result<LotStatus> {
        let id = check_id(id)?;
        let text = http::send_all(
            "anthropic",
            self.request("GET", &format!("/v1/messages/batches/{id}")),
            &self.retry,
            stop,
        )?;
        let v = http::parse_body("anthropic", &text)?;
        let status = v["processing_status"].as_str().ok_or_else(|| {
            Error::Format("état de lot de anthropic sans processing_status".into())
        })?;
        let done = ["succeeded", "errored", "canceled", "expired"]
            .iter()
            .map(|n| count(&v, n))
            .sum::<Result<usize>>()?;
        Ok(LotStatus {
            id: id.into(),
            status: status.into(),
            finished: status == "ended",
            done,
            total: done + count(&v, "processing")?,
            failure: None,
            default_answer: failed("sans réponse"),
            // L'adresse des résultats est construite ici : `results_url` n'est jamais suivie.
            files: vec![format!("/v1/messages/batches/{id}/results")],
        })
    }

    fn results(&self, status: &LotStatus, stop: &AtomicBool) -> Result<Vec<(String, Answer)>> {
        let mut out = Vec::new();
        for file in &status.files {
            let text = http::send_all("anthropic", self.request("GET", file), &self.retry, stop)?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let v = http::parse_body("anthropic", line)?;
                let custom_id = v["custom_id"].as_str().ok_or_else(|| {
                    Error::Format("ligne de résultat de anthropic sans custom_id".into())
                })?;
                out.push((custom_id.to_string(), line_answer(&v["result"])));
            }
        }
        Ok(out)
    }
}

impl Complete for AnthropicLlm {
    fn batch(&self) -> Option<BatchLink<'_>> {
        self.collector.as_deref().map(|collector| BatchLink {
            api: self,
            collector,
        })
    }

    fn summary_language(&self) -> &str {
        &self.summary_language
    }

    fn cache_floor(&self) -> Option<usize> {
        (self.prefix_cache && self.collector.is_none()).then(|| min_cacheable_tokens(&self.model))
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
        cache_upto: Option<usize>,
    ) -> Result<String> {
        let send = |with_schema: bool| {
            let schema = with_schema.then(|| reply.schema());
            let format = schema.as_ref().map(|s| (reply.name(), s));
            let mut body = request_body(&self.model, &self.gen, &system, &user, format);
            if let Some(upto) = cache_upto {
                body["messages"][0]["content"] = cached_content(&user, upto);
            }
            // La passe des sous-systèmes part toujours hors mode lot (REQ-261).
            if let (Some(collector), false) = (&self.collector, matches!(reply, Reply::Subsystem)) {
                return collector.exchange(&body, stats);
            }
            // Réponse reçue en flux (REQ-249) ; le mode lot ne passe pas ici.
            body["stream"] = serde_json::json!(true);
            http::send(
                "anthropic",
                self.request("POST", "/v1/messages"),
                Some(&body),
                &self.retry,
                label,
                stop,
                stats,
                &self.pause,
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
        stats.record_response(anthropic_usage(&v), served_model(&v));
        if v["stop_reason"] == "refusal" {
            return Err(Error::Refused(BLOCKED.into()));
        }
        let text = v["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .map(String::from);
        if v["stop_reason"] == "max_tokens" {
            return Err(Error::Truncated {
                message: TRUNCATED.into(),
                partial: text.unwrap_or_default(),
            });
        }
        text.ok_or_else(|| Error::Format("réponse de anthropic sans bloc texte".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn gen(thinking: bool, effort: Effort, max: u32) -> Generation {
        Generation {
            temperature: None,
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
    fn req275_temperature_envoyee_sauf_avec_thinking() {
        let none = request_body(
            "claude-opus-5-5",
            &gen(false, Effort::Low, 1234),
            "s",
            "u",
            None,
        );
        assert!(none.get("temperature").is_none());
        let mut g = gen(false, Effort::Low, 1234);
        g.temperature = Some(0.2);
        let mut want = none.clone();
        want["temperature"] = 0.2.into();
        assert_eq!(request_body("claude-opus-5-5", &g, "s", "u", None), want);
        g.temperature = Some(0.0);
        assert_eq!(
            request_body("claude-opus-5-5", &g, "s", "u", None)["temperature"],
            0.0
        );
        let mut g = gen(true, Effort::Low, 1234);
        g.temperature = Some(0.2);
        assert!(request_body("claude-opus-5-5", &g, "s", "u", None)
            .get("temperature")
            .is_none());
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
                .complete(
                    None,
                    None,
                    &stats,
                    "s".into(),
                    "u".into(),
                    Reply::File,
                    None,
                )
                .unwrap_err();
            if truncated {
                assert!(
                    matches!(&e, Error::Truncated { message: m, .. } if m == TRUNCATED),
                    "{e:?}"
                );
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
    fn req274_ac1_refusal_is_blocked_even_with_conforming_json_or_no_content() {
        let usage = r#""usage":{"input_tokens":10,"output_tokens":3}"#;
        let json_text = r#"{\"version\": 2}"#;
        let cases = [
            format!(
                r#"{{"stop_reason":"refusal","content":[{{"type":"text","text":"{json_text}"}}],{usage}}}"#
            ),
            format!(r#"{{"stop_reason":"refusal","content":[],{usage}}}"#),
        ];
        for body in cases {
            let l = llm(fake_server(vec![(200, body)]));
            let stats = CallStats::default();
            let e = l
                .complete(
                    None,
                    None,
                    &stats,
                    "s".into(),
                    "u".into(),
                    Reply::File,
                    None,
                )
                .unwrap_err();
            assert!(matches!(&e, Error::Refused(m) if m == BLOCKED), "{e:?}");
            assert_eq!(stats.usage().map(|u| (u.input, u.output)), Some((10, 3)));
            assert_eq!(stats.sends(), 1);
        }
        for stop in ["end_turn", "max_tokens"] {
            let body =
                format!(r#"{{"stop_reason":"{stop}","content":[{{"type":"text","text":"x"}}]}}"#);
            let l = llm(fake_server(vec![(200, body)]));
            let r = l.complete(
                None,
                None,
                &CallStats::default(),
                "s".into(),
                "u".into(),
                Reply::File,
                None,
            );
            if stop == "end_turn" {
                assert_eq!(r.unwrap(), "x");
            } else {
                assert!(matches!(&r, Err(Error::Truncated { .. })), "{stop} {r:?}");
            }
        }
    }

    #[test]
    fn req274_ac3_stream_refusal_is_blocked() {
        let l = llm(crate::llm::usage::sse_server(stream(
            Some("abc"),
            "refusal",
        )));
        let e = l
            .complete(
                None,
                None,
                &CallStats::default(),
                "s".into(),
                "u".into(),
                Reply::File,
                None,
            )
            .unwrap_err();
        assert!(matches!(&e, Error::Refused(m) if m == BLOCKED), "{e:?}");
    }

    fn served_of(body: String) -> (String, Option<String>) {
        let l = llm(fake_server(vec![(200, body)]));
        let stats = CallStats::default();
        let r = l.complete(
            None,
            None,
            &stats,
            "s".into(),
            "u".into(),
            Reply::File,
            None,
        );
        (r.unwrap(), stats.served())
    }

    #[test]
    fn req278_ac1_anthropic_served_model_whole_body_and_stream() {
        let body = |model: serde_json::Value| {
            json!({"stop_reason":"end_turn","content":[{"type":"text","text":"x"}],"model":model})
                .to_string()
        };
        assert_eq!(
            served_of(body(json!("claude-x-20250929"))),
            ("x".into(), Some("claude-x-20250929".into()))
        );
        let max = "a".repeat(256);
        assert_eq!(served_of(body(json!(max.clone()))), ("x".into(), Some(max)));
        for bad in [json!(""), json!(3), json!("a".repeat(257))] {
            assert_eq!(served_of(body(bad.clone())), ("x".into(), None), "{bad}");
        }
        let absent = json!({"stop_reason":"end_turn","content":[{"type":"text","text":"x"}]});
        assert_eq!(served_of(absent.to_string()), ("x".into(), None));
        let streamed = stream(Some("abc"), "end_turn").replace(
            r#""message":{"usage""#,
            r#""message":{"model":"claude-x-20250929","usage""#,
        );
        let l = llm(crate::llm::usage::sse_server(streamed));
        let stats = CallStats::default();
        let r = l.complete(
            None,
            None,
            &stats,
            "s".into(),
            "u".into(),
            Reply::File,
            None,
        );
        assert_eq!(r.unwrap(), "abc");
        assert_eq!(stats.served().as_deref(), Some("claude-x-20250929"));
        let l = llm(crate::llm::usage::sse_server(stream(
            Some("abc"),
            "end_turn",
        )));
        let stats = CallStats::default();
        let _ = l.complete(
            None,
            None,
            &stats,
            "s".into(),
            "u".into(),
            Reply::File,
            None,
        );
        assert_eq!(stats.served(), None);
    }

    fn event(name: &str, data: serde_json::Value) -> String {
        format!("event: {name}\ndata: {data}\n\n")
    }

    /// Flux Anthropic : texte en deux deltas (s'il y en a un), `stop_reason`, fin.
    fn stream(text: Option<&str>, stop: &str) -> String {
        let mut s = event(
            "message_start",
            json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1}}}),
        );
        if let Some(t) = text {
            s += &event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            );
            let (a, b) = t.split_at(1);
            for part in [a, b] {
                s += &event(
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":part}}),
                );
            }
        }
        s += &event(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":3}}),
        );
        s + &event("message_stop", json!({"type":"message_stop"}))
    }

    #[test]
    fn req249_ac4_stream_max_tokens_is_truncated_with_the_received_text() {
        let l = llm(crate::llm::usage::sse_server(stream(
            Some("abc"),
            "max_tokens",
        )));
        let stats = CallStats::default();
        let e = l
            .complete(
                None,
                None,
                &stats,
                "s".into(),
                "u".into(),
                Reply::File,
                None,
            )
            .unwrap_err();
        assert!(
            matches!(&e, Error::Truncated { message: m, partial } if m == TRUNCATED && partial == "abc"),
            "{e:?}"
        );
        assert_eq!(stats.usage().map(|u| (u.input, u.output)), Some((10, 3)));
    }

    #[test]
    fn req249_ac4_stream_without_text_block_is_the_format_error() {
        let l = llm(crate::llm::usage::sse_server(stream(None, "end_turn")));
        let e = l
            .complete(
                None,
                None,
                &CallStats::default(),
                "s".into(),
                "u".into(),
                Reply::File,
                None,
            )
            .unwrap_err();
        assert!(
            matches!(&e, Error::Format(m) if m == "réponse de anthropic sans bloc texte"),
            "{e:?}"
        );
    }

    #[test]
    fn req249_ac4_complete_stream_gives_the_text() {
        let l = llm(crate::llm::usage::sse_server(stream(
            Some("abc"),
            "end_turn",
        )));
        let t = l
            .complete(
                None,
                None,
                &CallStats::default(),
                "s".into(),
                "u".into(),
                Reply::File,
                None,
            )
            .unwrap();
        assert_eq!(t, "abc");
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

    /// Corps attendu d'un appel direct : celui du lot, plus `stream` (REQ-249).
    fn live_body(with_schema: bool) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&expected_body(with_schema)).unwrap();
        v["stream"] = serde_json::json!(true);
        v.to_string()
    }

    fn same_json(a: &str, b: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(a).unwrap()
            == serde_json::from_str::<serde_json::Value>(b).unwrap()
    }

    fn call(l: &AnthropicLlm, stats: &CallStats) -> Result<String> {
        l.complete(None, None, stats, "s".into(), "u".into(), Reply::File, None)
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
        assert!(same_json(&b[0], &live_body(true)));
        for later in &b[1..] {
            assert!(same_json(later, &live_body(false)), "{later}");
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
        assert!(b[..2].iter().all(|x| same_json(x, &live_body(true))));
        assert!(b[2..].iter().all(|x| same_json(x, &live_body(false))));
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

    fn batch_llm(c: &std::sync::Arc<crate::llm::batch::Collector>) -> AnthropicLlm {
        llm_structured(
            "http://127.0.0.1:1".into(),
            Structured::Auto,
            "structured_output",
        )
        .with_collector(c.clone())
    }

    #[test]
    fn req214_ac5_refus_du_schema_en_lot_replie_et_met_de_cote_sans_schema() {
        use crate::llm::batch::{body_id, Answer, Collector};
        let c = std::sync::Arc::new(Collector::new());
        let l = batch_llm(&c);
        let with: serde_json::Value = serde_json::from_str(&expected_body(true)).unwrap();
        let without: serde_json::Value = serde_json::from_str(&expected_body(false)).unwrap();
        c.add_answer(
            body_id(&with),
            Answer::Failed {
                cause: "invalid_request_error : output_config.format: not supported".into(),
                refused: Some("output_config.format: not supported".into()),
            },
        );
        // Un test muet les traces : celle du repli doit tout de même sortir.
        struct Unmute;
        impl Drop for Unmute {
            fn drop(&mut self) {
                crate::trace::mute(false);
            }
        }
        let _unmute = Unmute;
        crate::trace::mute(true);
        let (r, events) = crate::trace::capture(|| call(&l, &CallStats::default()));
        crate::trace::mute(false);
        assert_eq!(r, Err(Error::Deferred));
        let pend = c.take_pending();
        assert_eq!(pend.len(), 1);
        assert_eq!(pend[0].body, without);
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(events[0].starts_with("sortie structurée: schéma JSON refusé par anthropic"));
        // Sa réponse connue est rendue.
        c.add_answer(pend[0].id.clone(), Answer::Raw(OK_ANTHROPIC.into()));
        let stats = CallStats::default();
        assert_eq!(call(&l, &stats).unwrap(), "x");
        assert!(stats.responded());
        // Le repli reste posé : les appels suivants partent sans schéma, sans nouvelle trace.
        let (r, events) = crate::trace::capture(|| call(&l, &CallStats::default()));
        assert_eq!(r.unwrap(), "x");
        assert!(events.is_empty());
    }

    #[test]
    fn req214_ac5_refus_sans_mention_de_schema_donne_batch_item() {
        use crate::llm::batch::{body_id, Answer, Collector};
        let c = std::sync::Arc::new(Collector::new());
        let l = batch_llm(&c);
        let with: serde_json::Value = serde_json::from_str(&expected_body(true)).unwrap();
        c.add_answer(
            body_id(&with),
            Answer::Failed {
                cause: "invalid_request_error : model unknown".into(),
                refused: Some("model unknown".into()),
            },
        );
        let e = call(&l, &CallStats::default()).unwrap_err();
        assert_eq!(
            e,
            Error::BatchItem {
                cause: "invalid_request_error : model unknown".into(),
                refused: Some("model unknown".into())
            }
        );
        assert!(c.take_pending().is_empty());
        // Sous `On`, même un refus qui cite le schéma ne replie pas.
        let c = std::sync::Arc::new(Collector::new());
        let l = llm_structured(
            "http://127.0.0.1:1".into(),
            Structured::On,
            "structured_output",
        )
        .with_collector(c.clone());
        c.add_answer(
            body_id(&with),
            Answer::Failed {
                cause: "x".into(),
                refused: Some("bad schema".into()),
            },
        );
        assert!(matches!(
            call(&l, &CallStats::default()),
            Err(Error::BatchItem { .. })
        ));
        assert!(c.take_pending().is_empty());
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::llm::enrich::{cacheable_prefix_len, enrichment_user_prompt};
    use crate::llm::usage::recording_server;
    use crate::llm::{calls_line, FormatFault, Llm, LlmTotals};

    fn gen() -> Generation {
        Generation {
            temperature: None,
            effort: Effort::Low,
            effort_explicit: false,
            thinking: false,
            max_output_tokens: 100,
        }
    }

    fn make(base: Option<String>, model: &str) -> AnthropicLlm {
        AnthropicLlm::new(
            base,
            model.into(),
            "k".into(),
            10,
            &Tls::default(),
            1,
            gen(),
        )
    }

    fn reply(cache_read: u64) -> (u16, String) {
        let text = r#"{\"summaries\":[],\"concepts\":[]}"#;
        (
            200,
            format!(
                r#"{{"stop_reason":"end_turn","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":10,"output_tokens":3,"cache_read_input_tokens":{cache_read}}}}}"#
            ),
        )
    }

    /// Envoie un appel d'enrichissement de `A.java` et vérifie qu'il aboutit.
    fn send(
        l: &AnthropicLlm,
        stats: &CallStats,
        requested: &[String],
        fault: Option<FormatFault>,
        cached: bool,
    ) {
        l.enrich_labeled(
            "l",
            "A.java",
            1,
            None,
            "contenu du morceau",
            requested,
            &[],
            &AtomicBool::new(false),
            stats,
            fault,
            cached,
        )
        .unwrap();
    }

    fn bodies(seen: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<serde_json::Value> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|b| serde_json::from_str(b).unwrap())
            .collect()
    }

    #[test]
    fn req268_ac1_min_cacheable_tokens() {
        let cases = [
            ("claude-opus-5-5", 512),
            ("claude-haiku-5-5", 512),
            ("claude-sonnet-4-6", 1024),
            ("claude-opus-4-1-20250805", 1024),
            ("claude-opus-4-7", 2048),
            ("claude-opus-4-5-20251101", 4096),
            ("claude-haiku-4-5", 4096),
            ("gpt-x", 1024),
        ];
        for (model, want) in cases {
            assert_eq!(min_cacheable_tokens(model), want, "{model}");
        }
    }

    #[test]
    fn req268_ac2_content_is_two_blocks_with_the_marker_on_the_first() {
        let (base, seen) = recording_server(vec![reply(0)], 0);
        let l = make(Some(base), "m").with_prefix_cache(true);
        let one = vec!["class:A".to_string()];
        let two = vec!["class:A".to_string(), "method:A.f".to_string()];
        send(&l, &CallStats::default(), &one, None, true);
        send(&l, &CallStats::default(), &two, None, true);
        send(&l, &CallStats::default(), &one, None, false);
        let b = bodies(&seen);
        let head = "<file path=\"A.java\">\ncontenu du morceau\n</file>";
        assert_eq!(
            head.len(),
            cacheable_prefix_len("A.java", "contenu du morceau")
        );
        for (body, list) in b.iter().zip([&one, &two]) {
            let full = enrichment_user_prompt("A.java", None, "contenu du morceau", list);
            let content = &body["messages"][0]["content"];
            assert_eq!(
                *content,
                serde_json::json!([
                    {"type": "text", "text": head, "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": &full[head.len()..]},
                ])
            );
            let joined: String = content
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x["text"].as_str().unwrap())
                .collect();
            assert_eq!(joined, full);
        }
        // Le premier bloc ne dépend pas de la liste de nœuds.
        assert_eq!(
            b[0]["messages"][0]["content"][0],
            b[1]["messages"][0]["content"][0]
        );
        // Sans marqueur : la chaîne de la requête, inchangée.
        assert_eq!(
            b[2]["messages"][0]["content"],
            enrichment_user_prompt("A.java", None, "contenu du morceau", &one)
        );
    }

    #[test]
    fn req268_ac2_cut_outside_the_text_leaves_the_string() {
        assert_eq!(cached_content("abc", 3), serde_json::json!("abc"));
        assert_eq!(cached_content("abc", 9), serde_json::json!("abc"));
        assert_eq!(cached_content("aé", 2), serde_json::json!("aé"));
    }

    #[test]
    fn req268_ac4_cache_floor_of_anthropic() {
        let m = "claude-opus-4-7";
        assert_eq!(Llm::cache_floor(&make(None, m)), Some(2048));
        assert_eq!(Llm::cache_floor(&make(Some("http://x".into()), m)), None);
        let compat = make(Some("http://x".into()), m).with_prefix_cache(true);
        assert_eq!(Llm::cache_floor(&compat), Some(2048));
        let batch = make(None, m).with_collector(Arc::new(Collector::new()));
        assert_eq!(Llm::cache_floor(&batch), None);
    }

    #[test]
    fn req268_ac6_format_retry_keeps_the_first_block() {
        let (base, seen) = recording_server(vec![reply(0)], 0);
        let l = make(Some(base), "m").with_prefix_cache(true);
        let list = vec!["class:A".to_string()];
        send(&l, &CallStats::default(), &list, None, true);
        send(
            &l,
            &CallStats::default(),
            &list,
            Some(FormatFault::Absent),
            true,
        );
        let b = bodies(&seen);
        let (first, retry) = (
            &b[0]["messages"][0]["content"],
            &b[1]["messages"][0]["content"],
        );
        assert_eq!(first[0], retry[0]);
        let note = FormatFault::Absent.note();
        let tail = retry[1]["text"].as_str().unwrap();
        assert!(tail.ends_with(&format!("\n\n{note}")), "{tail}");
        assert_eq!(
            tail,
            format!("{}\n\n{note}", first[1]["text"].as_str().unwrap())
        );
    }

    #[test]
    fn req268_ac7_cache_read_of_the_second_call_is_counted() {
        let (base, _) = recording_server(vec![reply(0), reply(500)], 0);
        let l = make(Some(base), "m").with_prefix_cache(true);
        let mut totals = LlmTotals::default();
        for _ in 0..2 {
            let stats = CallStats::default();
            send(&l, &stats, &[], None, true);
            totals.add(&stats, false);
        }
        assert!(
            calls_line(&totals).contains("dont cache 500"),
            "{}",
            calls_line(&totals)
        );
    }
}
