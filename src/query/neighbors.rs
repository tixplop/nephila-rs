//! `query neighbors` (REQ-014) : voisins par parcours en largeur, séparés « utilise »
//! (liens sortants) et « utilisé par » (liens entrants).
//!
//! Un type est traité avec les méthodes qu'il contient : leurs liens comptent comme les
//! siens, et les liens internes au groupe ne sont pas des voisins.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{json, Value};

use super::lookup;
use super::search::DEFAULT_LIMIT;
use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, Node, NodeKind};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Out,
    In,
}

struct Found {
    kind: EdgeKind,
    depth: usize,
    node: Node,
}

/// Un nœud et, pour un type, ses méthodes contenues.
fn group(
    id: &str,
    nodes: &HashMap<&str, &Node>,
    members: &HashMap<&str, Vec<&str>>,
) -> Vec<String> {
    let mut g = vec![id.to_string()];
    if nodes.get(id).is_some_and(|n| n.kind == NodeKind::Type) {
        if let Some(ms) = members.get(id) {
            g.extend(ms.iter().map(|m| m.to_string()));
        }
    }
    g
}

fn explore(
    start: &Node,
    dir: Direction,
    depth: usize,
    edges: &[Edge],
    nodes: &HashMap<&str, &Node>,
    members: &HashMap<&str, Vec<&str>>,
) -> Vec<Found> {
    let mut visited: HashSet<String> = group(&start.id, nodes, members).into_iter().collect();
    // Adjacence indexée : nœud proche -> (nœud lointain, genre de lien).
    let mut adj: HashMap<&str, Vec<(&str, EdgeKind)>> = HashMap::new();
    for e in edges {
        let (near, far) = match dir {
            Direction::Out => (&e.source, &e.target),
            Direction::In => (&e.target, &e.source),
        };
        adj.entry(near.as_str())
            .or_default()
            .push((far.as_str(), e.kind));
    }
    let mut frontier = vec![start.id.clone()];
    let mut out = Vec::new();
    for level in 1..=depth {
        let mut level_found: Vec<Found> = Vec::new();
        let mut next = Vec::new();
        for id in &frontier {
            let g: HashSet<String> = group(id, nodes, members).into_iter().collect();
            for member in &g {
                for &(far, kind) in adj
                    .get(member.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    if g.contains(far) {
                        continue;
                    }
                    let Some(node) = nodes.get(far) else {
                        continue;
                    };
                    // Une même cible peut apparaître par plusieurs genres de lien.
                    if level_found
                        .iter()
                        .any(|f| f.node.id == far && f.kind == kind)
                    {
                        continue;
                    }
                    if visited.contains(far) && !level_found.iter().any(|f| f.node.id == far) {
                        continue;
                    }
                    level_found.push(Found {
                        kind,
                        depth: level,
                        node: (*node).clone(),
                    });
                    if !next.iter().any(|n| n == far) {
                        next.push(far.to_string());
                    }
                }
            }
        }
        level_found.sort_by(|a, b| {
            a.kind
                .as_str()
                .cmp(b.kind.as_str())
                .then_with(|| a.node.qualified.cmp(&b.node.qualified))
                .then_with(|| a.node.id.cmp(&b.node.id))
        });
        for id in &next {
            for m in group(id, nodes, members) {
                visited.insert(m);
            }
        }
        out.extend(level_found);
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    out
}

fn to_json(f: &Found) -> Value {
    json!({
        "kind": f.kind.as_str(),
        "depth": f.depth,
        "node_kind": f.node.kind.as_str(),
        "subkind": f.node.subkind.map(|s| s.as_str()),
        "qualified": f.node.qualified,
        "file": f.node.file,
    })
}

pub fn run(
    repo: &Path,
    name: &str,
    depth: usize,
    limit: Option<usize>,
    as_json: bool,
) -> Result<()> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::Message("--limit doit valoir au moins 1".to_string()));
    }
    if depth == 0 {
        return Err(Error::Message("--depth doit valoir au moins 1".to_string()));
    }
    let store = super::open(repo)?;
    let start = lookup::require(&store, name)?;
    let all = store.nodes()?;
    let edges = store.edges()?;
    let nodes: HashMap<&str, &Node> = all.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut members: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &edges {
        let method = nodes
            .get(e.target.as_str())
            .is_some_and(|n| n.kind == NodeKind::Method);
        if e.kind == EdgeKind::Contains && method {
            members.entry(&e.source).or_default().push(&e.target);
        }
    }
    let uses = explore(&start, Direction::Out, depth, &edges, &nodes, &members);
    let used_by = explore(&start, Direction::In, depth, &edges, &nodes, &members);

    let total = uses.len() + used_by.len();
    let remaining = total.saturating_sub(limit);
    let n_uses = uses.len().min(limit);
    let n_used_by = used_by.len().min(limit - n_uses);
    let (uses, used_by) = (&uses[..n_uses], &used_by[..n_used_by]);

    if as_json {
        println!(
            "{}",
            json!({
                "node": start.qualified,
                "uses": uses.iter().map(to_json).collect::<Vec<_>>(),
                "used_by": used_by.iter().map(to_json).collect::<Vec<_>>(),
                "remaining": remaining,
            })
        );
        return Ok(());
    }
    if total == 0 {
        println!("aucun voisin pour {}", start.qualified);
        return Ok(());
    }
    for (title, list) in [("utilise :", uses), ("utilisé par :", used_by)] {
        if list.is_empty() {
            continue;
        }
        println!("{title}");
        for f in list {
            let at = if depth > 1 {
                format!(" [profondeur {}]", f.depth)
            } else {
                String::new()
            };
            println!("  {} {}{at}", f.kind, super::describe(&f.node));
        }
    }
    if remaining > 0 {
        println!("{}", super::remaining_line(remaining));
    }
    Ok(())
}
