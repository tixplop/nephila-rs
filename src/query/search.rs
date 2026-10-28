//! `query search` (REQ-013) : recherche insensible à la casse sur le nom court, le nom
//! qualifié et le résumé ; tri exact, puis préfixe, puis contenu.

use std::path::Path;

use crate::out::Out;

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

/// Formes en minuscules des champs recherchés d'un nœud, calculées une fois par commande.
struct Folded<'a> {
    node: &'a Node,
    short: String,
    qualified: String,
    summary: Option<String>,
}

/// Nœuds préparés pour la recherche : les formes en minuscules sont calculées une fois, puis
/// servent à chaque terme sans copier de nœud (DEC-067).
pub struct Prepared<'a> {
    items: Vec<Folded<'a>>,
}

impl<'a> Prepared<'a> {
    pub fn new(nodes: &'a [Node]) -> Prepared<'a> {
        let items = nodes
            .iter()
            .map(|n| Folded {
                node: n,
                short: n.short.to_lowercase(),
                qualified: n.qualified.to_lowercase(),
                summary: n.summary.as_deref().map(str::to_lowercase),
            })
            .collect();
        Prepared { items }
    }

    /// Pour chaque nœud correspondant au terme (déjà en minuscules) : sa position dans le
    /// tableau et son rang (0 exact, 1 préfixe, 2 contenu).
    pub fn ranks(&self, term: &str) -> Vec<(usize, u8)> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                rank(f, term).map(|r| {
                    (
                        i,
                        match r {
                            Rank::Exact => 0,
                            Rank::Prefix => 1,
                            Rank::Content => 2,
                        },
                    )
                })
            })
            .collect()
    }

    /// Positions des nœuds dont le nom qualifié en minuscules vaut `name` ou s'y termine à une
    /// frontière de segment (règle de `lookup`).
    pub fn qualified_suffix(&self, name: &str) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, f)| f.qualified == name || super::lookup::is_suffix(&f.qualified, name))
            .map(|(i, _)| i)
            .collect()
    }

    /// Nœuds correspondants, triés ; départage par nom qualifié puis identifiant.
    pub fn matches(&self, term: &str) -> Vec<&'a Node> {
        let term = term.trim().to_lowercase();
        let mut hits: Vec<(Rank, &'a Node)> = self
            .items
            .iter()
            .filter_map(|f| rank(f, &term).map(|r| (r, f.node)))
            .collect();
        hits.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.qualified.cmp(&b.1.qualified))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        hits.into_iter().map(|(_, n)| n).collect()
    }
}

fn rank(f: &Folded, term: &str) -> Option<Rank> {
    if f.short == term || f.qualified == term {
        Some(Rank::Exact)
    } else if f.short.starts_with(term) || f.qualified.starts_with(term) {
        Some(Rank::Prefix)
    } else if f.short.contains(term)
        || f.qualified.contains(term)
        || f.summary.as_deref().is_some_and(|s| s.contains(term))
    {
        Some(Rank::Content)
    } else {
        None
    }
}

fn label(n: &Node) -> &'static str {
    n.subkind.map_or(n.kind.as_str(), |s| s.as_str())
}

pub(crate) fn line(n: &Node) -> String {
    let place = match &n.file {
        Some(f) => format!("{}, {f}", label(n)),
        None => label(n).to_string(),
    };
    match &n.summary {
        Some(s) => format!("{} ({place}) - {s}", n.qualified),
        None => format!("{} ({place})", n.qualified),
    }
}

pub(crate) fn to_json(n: &Node) -> Value {
    json!({
        "kind": n.kind.as_str(),
        "subkind": n.subkind.map(|s| s.as_str()),
        "qualified": n.qualified,
        "file": n.file,
        "summary": n.summary,
    })
}

pub fn run(
    repo: &Path,
    term: &str,
    limit: Option<usize>,
    as_json: bool,
    check_stale: bool,
    out: &mut Out,
) -> Result<()> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::Message("--limit doit valoir au moins 1".to_string()));
    }
    if term.trim().is_empty() {
        return Err(Error::Message("le terme de recherche est vide".to_string()));
    }
    let store = super::open(repo, check_stale)?;
    let nodes = store.nodes()?;
    let hits = Prepared::new(&nodes).matches(term);
    if hits.is_empty() {
        return Err(Error::Message(format!("aucun résultat pour « {term} »")));
    }
    let remaining = hits.len().saturating_sub(limit);
    let shown = &hits[..hits.len() - remaining];
    if as_json {
        let results: Vec<Value> = shown.iter().map(|n| to_json(n)).collect();
        writeln!(
            out,
            "{}",
            json!({"results": results, "remaining": remaining})
        )?;
    } else {
        for n in shown {
            writeln!(out, "{}", crate::escape::data(&line(n)))?;
        }
        if remaining > 0 {
            writeln!(out, "{}", super::remaining_line(remaining))?;
        }
    }
    Ok(())
}

/// Outils de test partagés : implémentation 0.18.0 conservée comme oracle (DEC-067).
#[cfg(test)]
pub(crate) mod oracle {
    use super::*;
    use crate::model::NodeKind;

    pub struct Lcg(pub u64);

    impl Lcg {
        pub fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    const FRAGMENTS: &[&str] = &[
        "Order",
        "order",
        "ÉCOLE",
        "école",
        "Straße",
        "STRASSE",
        "İstanbul",
        "日本語",
        "Ünï",
        "Billing",
        "ledger",
        "Ǆ",
        "ß",
        "Service",
        "Repo",
        "x",
    ];

    fn phrase(rng: &mut Lcg, max: usize) -> String {
        let n = 1 + rng.next(max);
        (0..n)
            .map(|_| FRAGMENTS[rng.next(FRAGMENTS.len())])
            .collect::<Vec<_>>()
            .join(if rng.next(2) == 0 { "" } else { " " })
    }

    pub fn random_nodes(rng: &mut Lcg) -> Vec<Node> {
        let count = 1 + rng.next(40);
        (0..count)
            .map(|_| {
                let short = phrase(rng, 2);
                let qualified = format!("p{}.{}", rng.next(3), short);
                let summary = (rng.next(3) != 0).then(|| phrase(rng, 4));
                Node::new(
                    NodeKind::Type,
                    &qualified,
                    &short,
                    Some("A.java"),
                    None,
                    summary.as_deref(),
                )
            })
            .collect()
    }

    /// Mots (minuscules) tirés dans les mêmes fragments, parfois tronqués.
    pub fn random_words(rng: &mut Lcg) -> Vec<String> {
        let count = 1 + rng.next(10);
        (0..count)
            .map(|_| {
                let f = FRAGMENTS[rng.next(FRAGMENTS.len())].to_lowercase();
                let chars: Vec<char> = f.chars().collect();
                let end = 1 + rng.next(chars.len());
                chars[..end].iter().collect()
            })
            .collect()
    }

    fn legacy_rank(n: &Node, term: &str) -> Option<Rank> {
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

    /// `search::matches` de la 0.18.0.
    pub fn legacy_matches(nodes: Vec<Node>, term: &str) -> Vec<Node> {
        let term = term.trim().to_lowercase();
        let mut hits: Vec<(Rank, Node)> = nodes
            .into_iter()
            .filter_map(|n| legacy_rank(&n, &term).map(|r| (r, n)))
            .collect();
        hits.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.qualified.cmp(&b.1.qualified))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        hits.into_iter().map(|(_, n)| n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::oracle::*;
    use super::*;

    #[test]
    fn req055_ac1_matches_same_ids_in_same_order_as_0_18_0() {
        let mut rng = Lcg(55);
        for _ in 0..200 {
            let nodes = random_nodes(&mut rng);
            for term in random_words(&mut rng) {
                let old: Vec<String> = legacy_matches(nodes.clone(), &term)
                    .into_iter()
                    .map(|n| n.id)
                    .collect();
                let new: Vec<&str> = Prepared::new(&nodes)
                    .matches(&term)
                    .into_iter()
                    .map(|n| n.id.as_str())
                    .collect();
                assert_eq!(old, new, "terme {term:?}");
            }
        }
    }
}
