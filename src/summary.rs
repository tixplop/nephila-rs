//! Résumé global (REQ-019) : calculé sans LLM, stocké à la fin de chaque build, lu par `summary`.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::model::{EdgeKind, NodeKind};
use crate::store::Store;

/// Nombre de nœuds de plus haut degré et de thèmes retenus.
const TOP: usize = 10;

/// Calcule le résumé du graphe courant (modules, nœuds les plus reliés, thèmes).
pub fn compute(store: &Store) -> Result<Value> {
    let nodes = store.nodes()?;
    let edges = store.edges()?;

    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut degree: HashMap<&str, u64> = HashMap::new();
    let mut mentions: HashMap<&str, u64> = HashMap::new();
    for e in &edges {
        *degree.entry(&e.source).or_default() += 1;
        *degree.entry(&e.target).or_default() += 1;
        match e.kind {
            EdgeKind::DependsOn => deps.entry(&e.source).or_default().push(&e.target),
            EdgeKind::Mentions => *mentions.entry(&e.target).or_default() += 1,
            _ => {}
        }
    }

    // `nodes()` est trié par identifiant : les égalités sont départagées de façon stable.
    let modules: Vec<Value> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .map(|n| {
            let mut d = deps.get(n.id.as_str()).cloned().unwrap_or_default();
            d.sort_unstable();
            json!({"id": n.id, "name": n.short, "depends_on": d})
        })
        .collect();

    let mut ranked: Vec<(&str, u64)> = degree.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let top_nodes: Vec<Value> = ranked
        .iter()
        .take(TOP)
        .map(|(id, d)| json!({"id": id, "degree": d}))
        .collect();

    let mut themes: Vec<(&str, u64)> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Concept)
        .filter_map(|n| mentions.get(n.id.as_str()).map(|c| (n.id.as_str(), *c)))
        .collect();
    themes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let themes: Vec<Value> = themes
        .iter()
        .take(TOP)
        .map(|(id, c)| json!({"concept": id, "mentions": c}))
        .collect();

    Ok(json!({"modules": modules, "top_nodes": top_nodes, "themes": themes}))
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Résumé du graphe courant sérialisé, avec son horodatage (millisecondes Unix).
pub(crate) fn compute_stamped(store: &Store) -> Result<(String, i64)> {
    Ok((compute(store)?.to_string(), now_millis()))
}

/// Résumé stocké, avec `computed_at`.
fn load(store: &Store) -> Result<Value> {
    let (text, at) = store.summary()?.ok_or_else(|| {
        Error::Message("aucun résumé enregistré : lancez `nephila build`".to_string())
    })?;
    let mut v: Value = serde_json::from_str(&text).map_err(|e| {
        Error::Message(format!(
            "résumé enregistré illisible ({e}) : lancez `nephila build`"
        ))
    })?;
    v["computed_at"] = json!(at);
    Ok(v)
}

/// Commande `nephila summary` : lecture seule, sans LLM ni configuration.
pub fn run(repo: &Path, as_json: bool) -> Result<()> {
    let store = Store::open_readonly(repo)?;
    let v = load(&store)?;
    if as_json {
        println!("{v}");
        return Ok(());
    }
    let list = |key: &str| v[key].as_array().cloned().unwrap_or_default();
    let modules = list("modules");
    println!("Modules ({})", modules.len());
    for m in &modules {
        let deps: Vec<&str> = m["depends_on"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        // Nom qualifié (identifiant sans le préfixe de genre) : `@acme/web`, pas `web`.
        // Suivi du nom court entre parenthèses s'il n'est pas déjà la fin du nom qualifié.
        let short = m["name"].as_str();
        let qualified = m["id"]
            .as_str()
            .map(|id| id.strip_prefix("module:").unwrap_or(id))
            .or(short)
            .unwrap_or("?");
        let name = match short {
            Some(sh) if !qualified.ends_with(sh) => format!("{qualified} ({sh})"),
            _ => qualified.to_string(),
        };
        if deps.is_empty() {
            println!("  {name}");
        } else {
            println!("  {name} -> {}", deps.join(", "));
        }
    }
    println!("Nœuds les plus reliés");
    for n in list("top_nodes") {
        println!(
            "  {} (degré {})",
            n["id"].as_str().unwrap_or("?"),
            n["degree"]
        );
    }
    println!("Thèmes");
    let themes = list("themes");
    if themes.is_empty() {
        println!("  aucun thème (graphe non enrichi)");
    }
    for t in themes {
        println!(
            "  {} ({} mentions)",
            t["concept"].as_str().unwrap_or("?"),
            t["mentions"]
        );
    }
    Ok(())
}
