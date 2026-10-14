//! `query path` (REQ-015) : plus court chemin entre deux nœuds en ignorant le sens des liens ;
//! chaque lien est affiché dans son sens réel.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use serde_json::json;

use super::lookup;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, Node};

struct Step<'a> {
    /// Nœud de départ du pas dans le sens du parcours.
    from: &'a Node,
    to: &'a Node,
    kind: EdgeKind,
    /// Le lien réel va de `to` vers `from`.
    reversed: bool,
}

/// Forme passive d'un genre de lien, utilisée quand on le parcourt à rebours.
fn passive(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::DependsOn => "requis par",
        EdgeKind::Imports => "importé par",
        EdgeKind::Extends => "étendu par",
        EdgeKind::Implements => "implémenté par",
        EdgeKind::Contains => "contenu par",
        EdgeKind::LinksTo => "cible du lien de",
        EdgeKind::Mentions => "mentionné par",
        EdgeKind::Calls => "appelé par",
    }
}

fn shortest<'a>(
    from: &'a Node,
    to: &'a Node,
    edges: &'a [Edge],
    nodes: &'a HashMap<&str, &Node>,
) -> Option<Vec<Step<'a>>> {
    // Adjacence triée : le résultat est déterministe.
    let mut adj: HashMap<&str, Vec<(&str, EdgeKind, bool)>> = HashMap::new();
    for e in edges {
        adj.entry(&e.source)
            .or_default()
            .push((&e.target, e.kind, false));
        adj.entry(&e.target)
            .or_default()
            .push((&e.source, e.kind, true));
    }
    for v in adj.values_mut() {
        v.sort_by(|a, b| a.0.cmp(b.0).then(a.1.as_str().cmp(b.1.as_str())));
    }
    let mut prev: HashMap<&str, (&str, EdgeKind, bool)> = HashMap::new();
    let mut queue = VecDeque::from([from.id.as_str()]);
    let mut seen = std::collections::HashSet::from([from.id.as_str()]);
    while let Some(cur) = queue.pop_front() {
        if cur == to.id {
            break;
        }
        for &(next, kind, rev) in adj.get(cur).map(Vec::as_slice).unwrap_or_default() {
            if seen.insert(next) {
                prev.insert(next, (cur, kind, rev));
                queue.push_back(next);
            }
        }
    }
    if !seen.contains(to.id.as_str()) {
        return None;
    }
    let mut steps = Vec::new();
    let mut cur = to.id.as_str();
    while cur != from.id {
        let (p, kind, reversed) = prev[cur];
        steps.push(Step {
            from: nodes[p],
            to: nodes[cur],
            kind,
            reversed,
        });
        cur = p;
    }
    steps.reverse();
    Some(steps)
}

pub fn run(repo: &Path, from: &str, to: &str, as_json: bool) -> Result<()> {
    let store = super::open(repo)?;
    let a = lookup::require(&store, from)?;
    let b = lookup::require(&store, to)?;
    let all = store.nodes()?;
    let edges = store.edges()?;
    let nodes: HashMap<&str, &Node> = all.iter().map(|n| (n.id.as_str(), n)).collect();
    let steps = shortest(&a, &b, &edges, &nodes).ok_or_else(|| {
        Error::Message(format!(
            "aucun chemin entre {} et {}",
            a.qualified, b.qualified
        ))
    })?;
    if as_json {
        let list: Vec<_> = steps
            .iter()
            .map(|s| {
                let (src, dst) = if s.reversed {
                    (s.to, s.from)
                } else {
                    (s.from, s.to)
                };
                json!({
                    "from": s.from.qualified,
                    "to": s.to.qualified,
                    "kind": s.kind.as_str(),
                    "reversed": s.reversed,
                    "source": src.qualified,
                    "target": dst.qualified,
                })
            })
            .collect();
        println!("{}", json!({"path": list}));
    } else if steps.is_empty() {
        println!("{}", super::describe(&a));
    } else {
        for s in &steps {
            let verb = if s.reversed {
                passive(s.kind)
            } else {
                s.kind.as_str()
            };
            println!("{} {verb} {}", s.from.qualified, s.to.qualified);
        }
    }
    Ok(())
}
