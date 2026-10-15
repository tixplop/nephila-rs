//! `query path` (REQ-015) : plus court chemin entre deux nœuds en ignorant le sens des liens ;
//! chaque lien est affiché dans son sens réel.

use std::collections::HashMap;
use std::path::Path;

use serde_json::json;

use super::graph::Lazy;
use super::lookup;
use crate::error::{Error, Result};
use crate::model::{EdgeKind, Node};

struct Step {
    /// Nœud de départ du pas dans le sens du parcours.
    from: Node,
    to: Node,
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

/// Voisins de `cur` sans tenir compte du sens, triés (résultat déterministe) : (voisin, genre,
/// vrai si le lien réel va du voisin vers `cur`).
fn undirected(g: &Lazy, cur: &str) -> Result<Vec<(String, EdgeKind, bool)>> {
    let mut adj: Vec<(String, EdgeKind, bool)> = g
        .edges_at(cur, true)?
        .into_iter()
        .map(|(o, k)| (o, k, false))
        .collect();
    adj.extend(
        g.edges_at(cur, false)?
            .into_iter()
            .map(|(o, k)| (o, k, true)),
    );
    adj.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.as_str().cmp(b.1.as_str())));
    Ok(adj)
}

/// Un côté de la recherche bidirectionnelle : parents des nœuds atteints et frontière courante.
struct Side {
    /// nœud -> (nœud précédent vers l'origine du côté, genre, `reversed` du pas dans le sens
    /// from -> to).
    parent: HashMap<String, (String, EdgeKind, bool)>,
    origin: String,
    frontier: Vec<String>,
}

impl Side {
    fn new(origin: &Node) -> Self {
        Side {
            parent: HashMap::new(),
            origin: origin.id.clone(),
            frontier: vec![origin.id.clone()],
        }
    }

    fn reached(&self, id: &str) -> bool {
        id == self.origin || self.parent.contains_key(id)
    }
}

/// Plus court chemin par recherche en largeur bidirectionnelle (le côté à la plus petite
/// frontière avance d'un niveau). Au premier niveau où les deux côtés se rejoignent, le nœud de
/// jonction d'identifiant minimal est retenu.
fn shortest(from: &Node, to: &Node, g: &Lazy) -> Result<Option<Vec<Step>>> {
    if from.id == to.id {
        return Ok(Some(Vec::new()));
    }
    let mut fwd = Side::new(from);
    let mut bwd = Side::new(to);
    let meeting = loop {
        if fwd.frontier.is_empty() || bwd.frontier.is_empty() {
            return Ok(None);
        }
        let forward = fwd.frontier.len() <= bwd.frontier.len();
        let (side, other) = if forward {
            (&mut fwd, &bwd)
        } else {
            (&mut bwd, &fwd)
        };
        let mut next = Vec::new();
        let mut met: Vec<String> = Vec::new();
        for cur in &side.frontier {
            for (far, kind, rev) in undirected(g, cur)? {
                if side.reached(&far) {
                    continue;
                }
                // `reversed` est exprimé dans le sens from -> to.
                let reversed = if forward { rev } else { !rev };
                side.parent
                    .insert(far.clone(), (cur.clone(), kind, reversed));
                if other.reached(&far) {
                    met.push(far.clone());
                }
                next.push(far);
            }
        }
        side.frontier = next;
        if let Some(m) = met.into_iter().min() {
            break m;
        }
    };
    let node = |id: &str| -> Result<Node> {
        g.node(id)?
            .ok_or_else(|| Error::Message(format!("nœud {id} introuvable")))
    };
    // Moitié from -> jonction, reconstituée à rebours.
    let mut steps = Vec::new();
    let mut cur = meeting.clone();
    while let Some((p, kind, reversed)) = fwd.parent.get(&cur).cloned() {
        steps.push(Step {
            from: node(&p)?,
            to: node(&cur)?,
            kind,
            reversed,
        });
        cur = p;
    }
    steps.reverse();
    // Moitié jonction -> to.
    let mut cur = meeting;
    while let Some((p, kind, reversed)) = bwd.parent.get(&cur).cloned() {
        steps.push(Step {
            from: node(&cur)?,
            to: node(&p)?,
            kind,
            reversed,
        });
        cur = p;
    }
    Ok(Some(steps))
}

pub fn run(repo: &Path, from: &str, to: &str, as_json: bool) -> Result<()> {
    let store = super::open(repo)?;
    let a = lookup::require(&store, from)?;
    let b = lookup::require(&store, to)?;
    let graph = Lazy::new(&store);
    let steps = shortest(&a, &b, &graph)?.ok_or_else(|| {
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
                    (&s.to, &s.from)
                } else {
                    (&s.from, &s.to)
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
