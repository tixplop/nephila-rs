//! `query search` (REQ-013) : recherche insensible à la casse sur le nom court, le nom
//! qualifié et le résumé ; tri exact, puis préfixe, puis contenu.

use std::path::Path;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::model::Node;

/// Nombre de résultats affichés par défaut.
pub const DEFAULT_LIMIT: usize = 50;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Exact,
    Prefix,
    Content,
}

fn rank(n: &Node, term: &str) -> Option<Rank> {
    let short = n.short.to_lowercase();
    let qualified = n.qualified.to_lowercase();
    if short == term || qualified == term {
        Some(Rank::Exact)
    } else if short.starts_with(term) || qualified.starts_with(term) {
        Some(Rank::Prefix)
    } else if short.contains(term)
        || qualified.contains(term)
        || n.summary
            .as_deref()
            .is_some_and(|s| s.to_lowercase().contains(term))
    {
        Some(Rank::Content)
    } else {
        None
    }
}

/// Nœuds correspondants, triés ; départage par nom qualifié puis identifiant.
pub fn matches(nodes: Vec<Node>, term: &str) -> Vec<Node> {
    let term = term.trim().to_lowercase();
    let mut hits: Vec<(Rank, Node)> = nodes
        .into_iter()
        .filter_map(|n| rank(&n, &term).map(|r| (r, n)))
        .collect();
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.qualified.cmp(&b.1.qualified))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    hits.into_iter().map(|(_, n)| n).collect()
}

fn label(n: &Node) -> &'static str {
    n.subkind.map_or(n.kind.as_str(), |s| s.as_str())
}

fn line(n: &Node) -> String {
    let place = match &n.file {
        Some(f) => format!("{}, {f}", label(n)),
        None => label(n).to_string(),
    };
    match &n.summary {
        Some(s) => format!("{} ({place}) - {s}", n.qualified),
        None => format!("{} ({place})", n.qualified),
    }
}

fn to_json(n: &Node) -> Value {
    json!({
        "kind": n.kind.as_str(),
        "subkind": n.subkind.map(|s| s.as_str()),
        "qualified": n.qualified,
        "file": n.file,
        "summary": n.summary,
    })
}

pub fn run(repo: &Path, term: &str, limit: Option<usize>, as_json: bool) -> Result<()> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::Message("--limit doit valoir au moins 1".to_string()));
    }
    if term.trim().is_empty() {
        return Err(Error::Message("le terme de recherche est vide".to_string()));
    }
    let store = super::open(repo)?;
    let hits = matches(store.nodes()?, term);
    if hits.is_empty() {
        return Err(Error::Message(format!("aucun résultat pour « {term} »")));
    }
    let remaining = hits.len().saturating_sub(limit);
    let shown = &hits[..hits.len() - remaining];
    if as_json {
        let results: Vec<Value> = shown.iter().map(to_json).collect();
        println!("{}", json!({"results": results, "remaining": remaining}));
    } else {
        for n in shown {
            println!("{}", line(n));
        }
        if remaining > 0 {
            println!("{remaining} résultats supplémentaires");
        }
    }
    Ok(())
}
