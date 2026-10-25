//! `query neighbors` (REQ-014) : voisins par parcours en largeur, séparés « utilise »
//! (liens sortants) et « utilisé par » (liens entrants).
//!
//! Un type est traité avec les méthodes qu'il contient : leurs liens comptent comme les
//! siens, et les liens internes au groupe ne sont pas des voisins.

use std::collections::HashSet;
use std::path::Path;

use crate::out::Out;

use serde_json::{json, Value};

use super::graph::{Graph, Lazy};
use super::lookup;
use super::search::DEFAULT_LIMIT;
use crate::error::{Error, Result};
use crate::model::{EdgeKind, Node, NodeKind};

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
fn group(id: &str, g: &impl Graph) -> Result<Vec<String>> {
    let mut out = vec![id.to_string()];
    if g.node(id)?.is_some_and(|n| n.kind == NodeKind::Type) {
        for &(ref target, kind) in g.edges_at(id, true)?.iter() {
            if kind == EdgeKind::Contains
                && g.node(target)?.is_some_and(|n| n.kind == NodeKind::Method)
            {
                out.push(target.clone());
            }
        }
    }
    Ok(out)
}

fn explore(start: &Node, dir: Direction, depth: usize, g: &impl Graph) -> Result<Vec<Found>> {
    let outgoing = dir == Direction::Out;
    let mut visited: HashSet<String> = group(&start.id, g)?.into_iter().collect();
    let mut frontier = vec![start.id.clone()];
    let mut out = Vec::new();
    for level in 1..=depth {
        let mut level_found: Vec<Found> = Vec::new();
        // Couples (nœud, genre) et nœuds retenus à ce niveau, frontière suivante : des ensembles
        // plutôt que des recherches linéaires (temps linéaire autour d'un nœud très relié).
        let mut kept: HashSet<(String, EdgeKind)> = HashSet::new();
        let mut kept_nodes: HashSet<String> = HashSet::new();
        let mut next: Vec<String> = Vec::new();
        let mut in_next: HashSet<String> = HashSet::new();
        for id in &frontier {
            let members: HashSet<String> = group(id, g)?.into_iter().collect();
            for member in &members {
                for &(ref far, kind) in g.edges_at(member, outgoing)?.iter() {
                    if members.contains(far) {
                        continue;
                    }
                    let Some(node) = g.node(far)? else {
                        continue;
                    };
                    // Une même cible peut apparaître par plusieurs genres de lien.
                    if kept.contains(&(far.clone(), kind)) {
                        continue;
                    }
                    if visited.contains(far) && !kept_nodes.contains(far) {
                        continue;
                    }
                    kept.insert((far.clone(), kind));
                    kept_nodes.insert(far.clone());
                    level_found.push(Found {
                        kind,
                        depth: level,
                        node,
                    });
                    if in_next.insert(far.clone()) {
                        next.push(far.clone());
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
            visited.extend(group(id, g)?);
        }
        out.extend(level_found);
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    Ok(out)
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
    check_stale: bool,
    out: &mut Out,
) -> Result<()> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::Message("--limit doit valoir au moins 1".to_string()));
    }
    if depth == 0 {
        return Err(Error::Message("--depth doit valoir au moins 1".to_string()));
    }
    let store = super::open(repo, check_stale)?;
    let start = lookup::require(&store, name)?;
    let graph = Lazy::new(&store);
    let uses = explore(&start, Direction::Out, depth, &graph)?;
    let used_by = explore(&start, Direction::In, depth, &graph)?;

    let total = uses.len() + used_by.len();
    let remaining = total.saturating_sub(limit);
    let n_uses = uses.len().min(limit);
    let n_used_by = used_by.len().min(limit - n_uses);
    let (uses, used_by) = (&uses[..n_uses], &used_by[..n_used_by]);

    if as_json {
        writeln!(
            out,
            "{}",
            json!({
                "node": start.qualified,
                "uses": uses.iter().map(to_json).collect::<Vec<_>>(),
                "used_by": used_by.iter().map(to_json).collect::<Vec<_>>(),
                "remaining": remaining,
            })
        )?;
        return Ok(());
    }
    if total == 0 {
        writeln!(
            out,
            "{}",
            crate::escape::data(&format!("aucun voisin pour {}", start.qualified))
        )?;
        return Ok(());
    }
    for (title, list) in [("utilise :", uses), ("utilisé par :", used_by)] {
        if list.is_empty() {
            continue;
        }
        writeln!(out, "{title}")?;
        for f in list {
            let at = if depth > 1 {
                format!(" [profondeur {}]", f.depth)
            } else {
                String::new()
            };
            let l = format!("  {} {}{at}", f.kind, super::describe(&f.node));
            writeln!(out, "{}", crate::escape::data(&l))?;
        }
    }
    if remaining > 0 {
        writeln!(out, "{}", super::remaining_line(remaining))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    /// Graphe en mémoire qui rend les liens dans l'ordre de `Store` (autre extrémité, genre).
    struct Mem {
        nodes: HashMap<String, Node>,
        edges: BTreeSet<(String, String, &'static str)>,
    }

    impl Graph for Mem {
        fn node(&self, id: &str) -> Result<Option<Node>> {
            Ok(self.nodes.get(id).cloned())
        }

        fn edges_at(&self, id: &str, outgoing: bool) -> Result<crate::query::graph::Adjacent> {
            let mut v: Vec<(String, &'static str)> = self
                .edges
                .iter()
                .filter_map(|(s, t, k)| match outgoing {
                    true if s == id => Some((t.clone(), *k)),
                    false if t == id => Some((s.clone(), *k)),
                    _ => None,
                })
                .collect();
            v.sort();
            v.into_iter()
                .map(|(o, k)| Ok((o, EdgeKind::parse(k)?)))
                .collect::<Result<Vec<_>>>()
                .map(Into::into)
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    const KINDS: [EdgeKind; 9] = [
        EdgeKind::DependsOn,
        EdgeKind::Imports,
        EdgeKind::Extends,
        EdgeKind::Implements,
        EdgeKind::Contains,
        EdgeKind::LinksTo,
        EdgeKind::Mentions,
        EdgeKind::Calls,
        EdgeKind::References,
    ];

    fn random_graph(rng: &mut Rng) -> Mem {
        let n_types = 3 + rng.next(8);
        let n_methods = 2 + rng.next(10);
        let n_other = rng.next(4);
        let mut nodes = HashMap::new();
        let mut ids = Vec::new();
        let mut add = |kind: NodeKind, q: String| {
            let node = Node::new(kind, &q, &q, None, None, None);
            ids.push(node.id.clone());
            nodes.insert(node.id.clone(), node);
        };
        for i in 0..n_types {
            add(NodeKind::Type, format!("T{i}"));
        }
        for i in 0..n_methods {
            add(NodeKind::Method, format!("T.m{i}"));
        }
        for i in 0..n_other {
            add(NodeKind::Document, format!("d{i}"));
        }
        let mut edges = BTreeSet::new();
        // Méthodes contenues dans un type, parfois avec un lien de la méthode vers son type.
        for i in 0..n_methods {
            let m = format!("method:T.m{i}");
            let t = format!("type:T{}", rng.next(n_types));
            if rng.next(4) > 0 {
                edges.insert((t.clone(), m.clone(), "contains"));
            }
            if rng.next(4) == 0 {
                edges.insert((m, t, KINDS[rng.next(KINDS.len())].as_str()));
            }
        }
        for _ in 0..rng.next(ids.len() * 3) + 1 {
            let (a, b) = (rng.next(ids.len()), rng.next(ids.len()));
            let k = KINDS[rng.next(KINDS.len())];
            // Plusieurs genres entre deux mêmes nœuds : on réinsère parfois la même paire.
            edges.insert((ids[a].clone(), ids[b].clone(), k.as_str()));
            if rng.next(3) == 0 {
                let k2 = KINDS[rng.next(KINDS.len())];
                edges.insert((ids[a].clone(), ids[b].clone(), k2.as_str()));
            }
        }
        // Un lien vers un nœud absent du graphe doit être ignoré.
        edges.insert((ids[0].clone(), "type:Absent".to_string(), "imports"));
        Mem { nodes, edges }
    }

    /// `explore` de la 0.18.0 (recherches linéaires dans des vecteurs), conservé comme oracle.
    fn explore_oracle(
        start: &Node,
        dir: Direction,
        depth: usize,
        g: &impl Graph,
    ) -> Result<Vec<Found>> {
        let outgoing = dir == Direction::Out;
        let mut visited: HashSet<String> = group(&start.id, g)?.into_iter().collect();
        let mut frontier = vec![start.id.clone()];
        let mut out = Vec::new();
        for level in 1..=depth {
            let mut level_found: Vec<Found> = Vec::new();
            let mut next: Vec<String> = Vec::new();
            for id in &frontier {
                let members: HashSet<String> = group(id, g)?.into_iter().collect();
                for member in &members {
                    for &(ref far, kind) in g.edges_at(member, outgoing)?.iter() {
                        if members.contains(far) {
                            continue;
                        }
                        let Some(node) = g.node(far)? else {
                            continue;
                        };
                        if level_found
                            .iter()
                            .any(|f| &f.node.id == far && f.kind == kind)
                        {
                            continue;
                        }
                        if visited.contains(far) && !level_found.iter().any(|f| &f.node.id == far) {
                            continue;
                        }
                        level_found.push(Found {
                            kind,
                            depth: level,
                            node,
                        });
                        if !next.contains(far) {
                            next.push(far.clone());
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
                visited.extend(group(id, g)?);
            }
            out.extend(level_found);
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    fn flat(v: Vec<Found>) -> Vec<(String, &'static str, usize)> {
        v.into_iter()
            .map(|f| (f.node.id, f.kind.as_str(), f.depth))
            .collect()
    }

    #[test]
    fn req056_ac1_same_list_as_the_0_18_oracle() {
        let mut rng = Rng(0x5eed_0056);
        let mut non_empty = 0;
        for case in 0..200 {
            let g = random_graph(&mut rng);
            let ids: Vec<&String> = {
                let mut v: Vec<&String> = g.nodes.keys().collect();
                v.sort();
                v
            };
            let start = g.nodes[ids[rng.next(ids.len())]].clone();
            let depth = 1 + rng.next(3);
            for dir in [Direction::Out, Direction::In] {
                let want = flat(explore_oracle(&start, dir, depth, &g).unwrap());
                let got = flat(explore(&start, dir, depth, &g).unwrap());
                assert_eq!(
                    got, want,
                    "graphe {case}, départ {}, profondeur {depth}",
                    start.id
                );
                non_empty += usize::from(!want.is_empty());
            }
        }
        assert!(non_empty > 100, "graphes trop pauvres : {non_empty}");
    }
}
