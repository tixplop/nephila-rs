//! Résumé global (REQ-019) : calculé sans LLM, stocké à la fin de chaque build, lu par `summary`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::out::Out;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, Node, NodeKind};
use crate::store::Store;

/// Nombre de nœuds de plus haut degré et de thèmes retenus.
const TOP: usize = 10;

/// Module du résumé : identifiant, nom court, identifiants de ses dépendances (`depends_on`).
pub(crate) struct ModuleEntry {
    pub id: String,
    pub short: String,
    pub depends_on: Vec<String>,
}

/// Résumé du graphe (modules, nœuds les plus reliés, thèmes), fonction pure de trois entrées
/// (DEC-078) : les modules avec leurs dépendances, le degré des nœuds reliés (au moins les dix
/// plus hauts ; un lien d'un nœud vers lui-même compte deux fois) et le nombre de mentions de
/// chaque concept mentionné. Les égalités sont départagées par identifiant, en ordre d'octets.
pub(crate) fn compute(
    mut modules: Vec<ModuleEntry>,
    mut degrees: Vec<(String, u64)>,
    mut mentions: Vec<(String, u64)>,
) -> Value {
    modules.sort_by(|a, b| a.id.cmp(&b.id));
    let modules: Vec<Value> = modules
        .into_iter()
        .map(|mut m| {
            m.depends_on.sort_unstable();
            json!({"id": m.id, "name": m.short, "depends_on": m.depends_on})
        })
        .collect();

    degrees.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top_nodes: Vec<Value> = degrees
        .iter()
        .take(TOP)
        .map(|(id, d)| json!({"id": id, "degree": d}))
        .collect();

    mentions.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let themes: Vec<Value> = mentions
        .iter()
        .take(TOP)
        .map(|(id, c)| json!({"concept": id, "mentions": c}))
        .collect();

    json!({"modules": modules, "top_nodes": top_nodes, "themes": themes})
}

/// Résumé d'une passe complète, tiré des nœuds chargés par la passe et des liens qu'elle écrit
/// (dédupliqués comme à l'écriture) : ni `nodes` ni `edges` ne sont relus.
pub(crate) fn from_pass(nodes: &[Node], edges: &[Edge]) -> Value {
    let mut seen: HashSet<(&str, &str, &str)> = HashSet::new();
    let mut deps: HashMap<&str, Vec<String>> = HashMap::new();
    let mut degree: HashMap<&str, u64> = HashMap::new();
    let mut mentions: HashMap<&str, u64> = HashMap::new();
    for e in edges {
        if !seen.insert((&e.source, &e.target, e.kind.as_str())) {
            continue;
        }
        *degree.entry(&e.source).or_default() += 1;
        *degree.entry(&e.target).or_default() += 1;
        match e.kind {
            EdgeKind::DependsOn => deps.entry(&e.source).or_default().push(e.target.clone()),
            EdgeKind::Mentions => *mentions.entry(&e.target).or_default() += 1,
            _ => {}
        }
    }
    let modules = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .map(|n| ModuleEntry {
            id: n.id.clone(),
            short: n.short.clone(),
            depends_on: deps.remove(n.id.as_str()).unwrap_or_default(),
        })
        .collect();
    let themes = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Concept)
        .filter_map(|n| mentions.get(n.id.as_str()).map(|c| (n.id.clone(), *c)))
        .collect();
    let degrees = degree
        .into_iter()
        .map(|(i, d)| (i.to_string(), d))
        .collect();
    compute(modules, degrees, themes)
}

/// Résumé d'une passe incrémentale, dans la transaction de la passe et après l'écriture des
/// liens : modules et concepts viennent des nœuds chargés par la passe ; dépendances, dix plus
/// hauts degrés et mentions sont lus par requêtes agrégées (DEC-078).
pub(crate) fn from_store(store: &Store, nodes: &[Node]) -> Result<Value> {
    let mut modules = Vec::new();
    for n in nodes.iter().filter(|n| n.kind == NodeKind::Module) {
        modules.push(ModuleEntry {
            id: n.id.clone(),
            short: n.short.clone(),
            depends_on: store.module_dependencies(&n.id)?,
        });
    }
    let concepts: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Concept)
        .map(|n| n.id.as_str())
        .collect();
    let mentions = store
        .concept_mentions()?
        .into_iter()
        .filter(|(id, _)| concepts.contains(id.as_str()))
        .collect();
    Ok(compute(modules, store.top_degrees(TOP)?, mentions))
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Résumé sérialisé, avec son horodatage (millisecondes Unix).
pub(crate) fn stamped(summary: &Value) -> (String, i64) {
    (summary.to_string(), now_millis())
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
pub fn run(repo: &Path, as_json: bool, out: &mut Out) -> Result<()> {
    let store = Store::open_readonly(repo)?;
    let v = load(&store)?;
    if as_json {
        writeln!(out, "{v}")?;
        return Ok(());
    }
    let list = |key: &str| v[key].as_array().cloned().unwrap_or_default();
    let modules = list("modules");
    writeln!(out, "Modules ({})", modules.len())?;
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
            writeln!(out, "  {name}")?;
        } else {
            writeln!(out, "  {name} -> {}", deps.join(", "))?;
        }
    }
    writeln!(out, "Nœuds les plus reliés")?;
    for n in list("top_nodes") {
        writeln!(
            out,
            "  {} (degré {})",
            n["id"].as_str().unwrap_or("?"),
            n["degree"]
        )?;
    }
    writeln!(out, "Thèmes")?;
    let themes = list("themes");
    if themes.is_empty() {
        writeln!(out, "  aucun thème (graphe non enrichi)")?;
    }
    for t in themes {
        writeln!(
            out,
            "  {} ({} mentions)",
            t["concept"].as_str().unwrap_or("?"),
            t["mentions"]
        )?;
    }
    Ok(())
}
