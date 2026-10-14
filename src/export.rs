//! Commande `export` (REQ-018) : le graphe entier en JSON, lecture seule, sans LLM.

use std::path::Path;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::resolve;
use crate::store::Store;

/// Version du format d'export.
pub const EXPORT_VERSION: u32 = 1;

pub fn build_json(store: &Store) -> Result<Value> {
    let nodes: Vec<Value> = store
        .nodes()?
        .into_iter()
        .map(|n| {
            json!({
                "id": n.id,
                "kind": n.kind.as_str(),
                "qualified": n.qualified,
                "short": n.short,
                "file": n.file,
                "subkind": n.subkind.map(|s| s.as_str()),
                "summary": n.summary,
            })
        })
        .collect();
    let edges: Vec<Value> = store
        .edges()?
        .into_iter()
        .map(|e| json!({"source": e.source, "target": e.target, "kind": e.kind.as_str()}))
        .collect();
    let unresolved: Vec<Value> = resolve::unresolved(store)?
        .into_iter()
        .map(|r| json!({"source": r.source, "kind": r.kind.as_str(), "target": r.target_name}))
        .collect();
    Ok(json!({"version": EXPORT_VERSION, "nodes": nodes, "edges": edges, "unresolved": unresolved}))
}

pub fn run(repo: &Path, output: Option<&Path>) -> Result<()> {
    let store = Store::open_readonly(repo)?;
    let text = build_json(&store)?.to_string();
    match output {
        None => println!("{text}"),
        Some(path) => std::fs::write(path, format!("{text}\n")).map_err(|e| {
            Error::Message(format!("écriture de {} impossible : {e}", path.display()))
        })?,
    }
    Ok(())
}
