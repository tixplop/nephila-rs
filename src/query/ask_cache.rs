//! Cache des réponses de `query ask` (REQ-281, DEC-212) : clé, durée, fichier SQLite à part.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::config::Config;
use crate::error::Result;
use crate::llm::ask::{ask_system_prompt, ASK_PROMPT_VERSION};
use crate::llm::AskReply;

/// Durée de validité d'une entrée, en secondes (REQ-281).
pub const TTL_SECS: i64 = 3600;

const SCHEMA_VERSION: i64 = 1;
const CACHE_REL: &str = ".nephila/ask-cache.db";
const JOURNAL_REL: &str = ".nephila/ask-cache.db-journal";

fn path(repo: &Path) -> PathBuf {
    repo.join(CACHE_REL)
}

fn put(hasher: &mut blake3::Hasher, field: &str) {
    hasher.update(&(field.len() as u64).to_le_bytes());
    hasher.update(field.as_bytes());
}

fn key_of(
    prompt_version: u32,
    system: &str,
    cfg: &Config,
    question: &str,
    context: &str,
) -> [u8; 32] {
    let question = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let temperature = cfg.temperature.map(|t| t.to_string()).unwrap_or_default();
    let mut h = blake3::Hasher::new();
    for field in [
        prompt_version.to_string().as_str(),
        system,
        &question,
        context,
        cfg.provider.as_str(),
        cfg.base_url.as_deref().unwrap_or(""),
        &cfg.model,
        if cfg.thinking { "true" } else { "false" },
        cfg.effort.as_str(),
        &temperature,
        &cfg.max_output_tokens.to_string(),
        cfg.structured_output.as_str(),
    ] {
        put(&mut h, field);
    }
    *h.finalize().as_bytes()
}

/// Clé d'une entrée pour la question et le contexte envoyés.
pub fn key(cfg: &Config, question: &str, context: &str) -> [u8; 32] {
    key_of(
        ASK_PROMPT_VERSION,
        &ask_system_prompt(),
        cfg,
        question,
        context,
    )
}

/// Refuse les fichiers du cache qui sont des liens symboliques (REQ-098, E-23).
pub fn ensure_no_symlink(repo: &Path) -> Result<()> {
    for rel in [CACHE_REL, JOURNAL_REL] {
        let is_link = std::fs::symlink_metadata(repo.join(rel))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_link {
            return Err(crate::store::symlink_refusal(rel));
        }
    }
    Ok(())
}

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

fn user_version(conn: &Connection) -> std::result::Result<i64, String> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql)
}

/// Réponse gardée pour `key`, avec son âge en secondes ; `None` si absente, périmée ou si le
/// fichier n'existe pas (alors rien n'est créé).
pub fn lookup(
    repo: &Path,
    key: &[u8; 32],
    now: i64,
) -> std::result::Result<Option<(AskReply, i64)>, String> {
    let file = path(repo);
    if !file.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
    if user_version(&conn)? != SCHEMA_VERSION {
        return Ok(None);
    }
    let table: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'answers'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    if table.is_none() {
        return Ok(None);
    }
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT created, reply FROM answers WHERE key = ?1",
            params![&key[..]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((created, reply)) = row else {
        return Ok(None);
    };
    let age = now - created;
    if !(0..TTL_SECS).contains(&age) {
        return Ok(None);
    }
    let reply = AskReply::parse(&reply).map_err(|e| e.to_string())?;
    Ok(Some((reply, age)))
}

fn serialize(reply: &AskReply) -> String {
    match reply {
        AskReply::Found { answer, nodes } => {
            serde_json::json!({"found": true, "answer": answer, "nodes": nodes}).to_string()
        }
        AskReply::NotFound => serde_json::json!({"found": false}).to_string(),
    }
}

/// Garde `reply` sous `key` enregistrée à `now` ; supprime les entrées périmées.
pub fn store(
    repo: &Path,
    key: &[u8; 32],
    reply: &AskReply,
    now: i64,
) -> std::result::Result<(), String> {
    let conn = Connection::open(path(repo)).map_err(sql)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
    if user_version(&conn)? != SCHEMA_VERSION {
        conn.execute_batch("DROP TABLE IF EXISTS answers")
            .map_err(sql)?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS answers (key BLOB PRIMARY KEY, reply TEXT NOT NULL, created INTEGER NOT NULL)",
    )
    .map_err(sql)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(sql)?;
    conn.execute(
        "DELETE FROM answers WHERE created > ?1 OR created <= ?2",
        params![now, now - TTL_SECS],
    )
    .map_err(sql)?;
    conn.execute(
        "INSERT OR REPLACE INTO answers (key, reply, created) VALUES (?1, ?2, ?3)",
        params![&key[..], serialize(reply), now],
    )
    .map_err(sql)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AskOverrides, Effort, ModelOverride, Provider, StructuredOutput, DEFAULT_EFFORT,
        DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_SUMMARY_LANGUAGE,
    };
    use std::collections::BTreeMap;

    fn base() -> Config {
        Config {
            provider: Provider::OpenAi,
            model: "m".into(),
            base_url: Some("http://x".into()),
            api_key_env: None,
            max_chunk_chars: 100,
            concurrency: 2,
            timeout_secs: 30,
            batch_files: 3,
            format_retries: 1,
            max_calls: None,
            enrich_code: false,
            enrich_redact: true,
            enrich_include: Vec::new(),
            enrich_exclude: Vec::new(),
            thinking: false,
            effort: DEFAULT_EFFORT,
            effort_explicit: false,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            temperature: Some(0.5),
            model_aliases: BTreeMap::new(),
            structured_output: StructuredOutput::Auto,
            structured_key: "structured_output",
            tls_ca_cert: None,
            tls_ca_certs: Vec::new(),
            tls_insecure: false,
            summary_language: DEFAULT_SUMMARY_LANGUAGE.to_string(),
            model_override: ModelOverride::Warn,
            ask: AskOverrides::default(),
        }
    }

    fn reference(cfg: &Config) -> [u8; 32] {
        key_of(1, "sys", cfg, "Qui calcule ?", "ctx")
    }

    #[test]
    fn req281_ac5_un_seul_champ_change_la_cle() {
        let cfg = base();
        let r = reference(&cfg);
        let mut others = vec![
            key_of(1, "sys", &cfg, "Autre ?", "ctx"),
            key_of(1, "sys", &cfg, "Qui calcule ?", "ctx2"),
            key_of(1, "sys2", &cfg, "Qui calcule ?", "ctx"),
            key_of(2, "sys", &cfg, "Qui calcule ?", "ctx"),
            key_of(1, "sys", &cfg, "qui calcule ?", "ctx"),
        ];
        type Edit = Box<dyn Fn(&mut Config)>;
        let variants: Vec<Edit> = vec![
            Box::new(|c| c.provider = Provider::Anthropic),
            Box::new(|c| c.base_url = None),
            Box::new(|c| c.base_url = Some("http://y".into())),
            Box::new(|c| c.model = "m2".into()),
            Box::new(|c| c.thinking = true),
            Box::new(|c| c.effort = Effort::High),
            Box::new(|c| c.temperature = None),
            Box::new(|c| c.temperature = Some(0.7)),
            Box::new(|c| c.max_output_tokens += 1),
            Box::new(|c| c.structured_output = StructuredOutput::Off),
        ];
        for v in &variants {
            let mut c = base();
            v(&mut c);
            others.push(reference(&c));
        }
        for k in &others {
            assert_ne!(*k, r);
        }
        // Mêmes champs à espaces près, et réglages hors clé.
        assert_eq!(key_of(1, "sys", &cfg, "  Qui   calcule\n ? ", "ctx"), r);
        let mut c = base();
        c.format_retries = 9;
        c.timeout_secs = 99;
        assert_eq!(reference(&c), r);
        // Découpages différents.
        assert_ne!(
            key_of(1, "ab", &cfg, "c", "ctx"),
            key_of(1, "a", &cfg, "bc", "ctx")
        );
        assert_ne!(
            key_of(1, "sys", &cfg, "ab", "c"),
            key_of(1, "sys", &cfg, "a", "bc")
        );
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".nephila")).unwrap();
        d
    }

    fn found(a: &str) -> AskReply {
        AskReply::Found {
            answer: a.into(),
            nodes: vec!["type:A".into()],
        }
    }

    #[test]
    fn req281_ac6_duree_de_vie_et_purge() {
        let d = repo();
        let t = 1_000_000;
        let k = [1u8; 32];
        store(d.path(), &k, &found("a"), t).unwrap();
        assert_eq!(
            lookup(d.path(), &k, t + 3599).unwrap(),
            Some((found("a"), 3599))
        );
        assert_eq!(lookup(d.path(), &k, t).unwrap(), Some((found("a"), 0)));
        assert_eq!(lookup(d.path(), &k, t + 3600).unwrap(), None);
        assert_eq!(lookup(d.path(), &k, t - 1).unwrap(), None);
        let k2 = [2u8; 32];
        store(d.path(), &k2, &AskReply::NotFound, t + 3600).unwrap();
        assert_eq!(
            lookup(d.path(), &k2, t + 3600).unwrap(),
            Some((AskReply::NotFound, 0))
        );
        let conn = Connection::open(path(d.path())).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM answers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn req281_ac6_fichier_absent_et_version_differente() {
        let d = repo();
        let k = [3u8; 32];
        assert_eq!(lookup(d.path(), &k, 10).unwrap(), None);
        assert!(!path(d.path()).exists());

        let conn = Connection::open(path(d.path())).unwrap();
        conn.execute_batch("CREATE TABLE answers (x); PRAGMA user_version = 7;")
            .unwrap();
        drop(conn);
        assert_eq!(lookup(d.path(), &k, 10).unwrap(), None);
        store(d.path(), &k, &found("b"), 10).unwrap();
        assert_eq!(lookup(d.path(), &k, 11).unwrap(), Some((found("b"), 1)));
        let conn = Connection::open(path(d.path())).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 1);
    }

    #[test]
    fn req281_fichier_corrompu_donne_une_erreur() {
        let d = repo();
        std::fs::write(
            path(d.path()),
            b"pas une base sqlite, du tout, vraiment pas",
        )
        .unwrap();
        assert!(lookup(d.path(), &[0; 32], 1).is_err());
        assert!(store(d.path(), &[0; 32], &AskReply::NotFound, 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn req281_liens_symboliques_refuses() {
        let d = repo();
        ensure_no_symlink(d.path()).unwrap();
        std::os::unix::fs::symlink("/nonexistent", d.path().join(JOURNAL_REL)).unwrap();
        assert_eq!(
            ensure_no_symlink(d.path()).unwrap_err().to_string(),
            ".nephila/ask-cache.db-journal est un lien symbolique ; supprimez-le"
        );
    }
}
