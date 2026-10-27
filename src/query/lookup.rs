//! Résolution d'un nom saisi par l'utilisateur vers un nœud du graphe (partagée par les
//! requêtes `neighbors`, `path`, ...).
//!
//! Formes acceptées, sensibles à la casse : identifiant complet (`type:com.acme.A`), nom
//! qualifié, nom court, ou suffixe du nom qualifié commençant à une frontière de segment
//! (`OrderRepository.save`, `core.OrderService`). Un nom qualifié exact l'emporte sur les
//! correspondances par nom court ou par suffixe.

use crate::error::{Error, Result};
use crate::model::Node;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(Node),
    /// Plusieurs nœuds correspondent ; triés par identifiant.
    Ambiguous(Vec<Node>),
    NotFound,
}

pub(super) fn is_suffix(qualified: &str, name: &str) -> bool {
    qualified
        .strip_suffix(name)
        .and_then(|head| head.chars().next_back())
        .is_some_and(|c| matches!(c, '.' | '#' | '/' | ':'))
}

fn classify(mut found: Vec<Node>) -> Lookup {
    found.sort_by(|a, b| a.id.cmp(&b.id));
    match found.len() {
        0 => Lookup::NotFound,
        1 => Lookup::Found(found.remove(0)),
        _ => Lookup::Ambiguous(found),
    }
}

pub fn resolve(store: &Store, name: &str) -> Result<Lookup> {
    if name.is_empty() {
        return Ok(Lookup::NotFound);
    }
    let nodes = store.nodes_matching(name)?;
    if let Some(n) = nodes.iter().find(|n| n.id == name) {
        return Ok(Lookup::Found(n.clone()));
    }
    let exact: Vec<Node> = nodes
        .iter()
        .filter(|n| n.qualified == name)
        .cloned()
        .collect();
    if !exact.is_empty() {
        return Ok(classify(exact));
    }
    let loose: Vec<Node> = nodes
        .into_iter()
        .filter(|n| n.short == name || is_suffix(&n.qualified, name))
        .collect();
    Ok(classify(loose))
}

/// Comme [`resolve`], mais l'ambiguïté (avec les candidats) et l'absence sont des erreurs.
pub fn require(store: &Store, name: &str) -> Result<Node> {
    match resolve(store, name)? {
        Lookup::Found(n) => Ok(n),
        Lookup::NotFound => Err(Error::Message(format!(
            "aucun résultat : nœud « {name} » introuvable (essayez `nephila query search`)"
        ))),
        Lookup::Ambiguous(c) => {
            let list: Vec<String> = c
                .iter()
                .map(|n| format!("  {}", crate::escape::data(&n.id)))
                .collect();
            Err(Error::Message(format!(
                "« {name} » est ambigu, précisez avec le nom qualifié :\n{}",
                list.join("\n")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileRecord, FileState, NodeKind, SubKind};
    use tempfile::tempdir;

    /// Requête `nodes_matching` de la 0.18.0 (copie).
    const LEGACY_SQL: &str = "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes
                 WHERE id = ?1 OR qualified = ?1 OR short = ?1
                    OR (length(qualified) > length(?1) AND substr(qualified, -length(?1)) = ?1)
                 ORDER BY id";

    /// `resolve` de la 0.18.0, sur la requête de la 0.18.0.
    fn oracle(store: &Store, name: &str) -> Lookup {
        if name.is_empty() {
            return Lookup::NotFound;
        }
        let nodes = store.run_resolve(LEGACY_SQL, &[name]).unwrap();
        if let Some(n) = nodes.iter().find(|n| n.id == name) {
            return Lookup::Found(n.clone());
        }
        let exact: Vec<Node> = nodes
            .iter()
            .filter(|n| n.qualified == name)
            .cloned()
            .collect();
        if !exact.is_empty() {
            return classify(exact);
        }
        let loose: Vec<Node> = nodes
            .into_iter()
            .filter(|n| n.short == name || is_suffix(&n.qualified, name))
            .collect();
        classify(loose)
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    #[test]
    fn req057_ac2_resolve_matches_the_0_18_0_oracle() {
        let segs = [
            "a",
            "OrderService",
            "XOrderService",
            "Écran",
            "日本語",
            "save",
            "x y",
            "Order",
            "é",
            "a.b",
        ];
        let seps = [".", "#", "/", ":"];
        let kinds = [
            NodeKind::Type,
            NodeKind::Method,
            NodeKind::Section,
            NodeKind::Key,
            NodeKind::Document,
        ];
        let mut rng = Lcg(42);
        let mut nodes: Vec<Node> = vec![
            Node::new(
                NodeKind::Type,
                "a.OrderService",
                "OrderService",
                Some("f"),
                Some(SubKind::Class),
                None,
            ),
            Node::new(
                NodeKind::Type,
                "a.XOrderService",
                "XOrderService",
                Some("f"),
                Some(SubKind::Class),
                None,
            ),
        ];
        while nodes.len() < 300 {
            let n = 1 + rng.next(4);
            let mut q = String::new();
            for i in 0..n {
                if i > 0 {
                    q.push_str(seps[rng.next(4)]);
                }
                q.push_str(segs[rng.next(segs.len())]);
            }
            let kind = kinds[rng.next(kinds.len())];
            let last = q
                .rsplit(['.', '#', '/', ':'])
                .next()
                .unwrap_or("")
                .to_string();
            let short = if rng.next(3) == 0 {
                segs[rng.next(segs.len())].to_string()
            } else {
                last
            };
            let node = Node::new(kind, &q, &short, Some("f"), None, None);
            if nodes.iter().all(|m| m.id != node.id) {
                nodes.push(node);
            }
        }
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[FileRecord {
            path: "f".into(),
            hash: "h".into(),
            size: 1,
            mtime: 1,
            recorded_at: 0,
            state: FileState::Indexed,
        }])
        .unwrap();
        st.replace_structure("f", &nodes, &[]).unwrap();
        assert_eq!(st.count_nodes().unwrap(), 300);

        let mut names: Vec<String> = [
            "",
            "OrderService",
            "a.OrderService",
            "XOrderService",
            "inconnu",
            "日本語",
            "é",
            "Écran",
            "service",
            "rderService",
            ".",
            "a.",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        while names.len() < 200 {
            let n = &nodes[rng.next(nodes.len())];
            let name = match rng.next(5) {
                0 => n.id.clone(),
                1 => n.qualified.clone(),
                2 => n.short.clone(),
                3 => {
                    // suffixe à une frontière de segment
                    let idx: Vec<usize> = n
                        .qualified
                        .char_indices()
                        .filter(|(_, c)| matches!(c, '.' | '#' | '/' | ':'))
                        .map(|(i, c)| i + c.len_utf8())
                        .collect();
                    if idx.is_empty() {
                        n.qualified.clone()
                    } else {
                        n.qualified[idx[rng.next(idx.len())]..].to_string()
                    }
                }
                _ => {
                    // suffixe sans frontière
                    let chars: Vec<char> = n.qualified.chars().collect();
                    chars[rng.next(chars.len())..].iter().collect()
                }
            };
            names.push(name);
        }
        let mut non_trivial = 0;
        for name in &names {
            let expected = oracle(&st, name);
            assert_eq!(resolve(&st, name).unwrap(), expected, "nom {name:?}");
            if !matches!(expected, Lookup::NotFound) {
                non_trivial += 1;
            }
            // Candidats de `nodes_matching` : mêmes nœuds que la requête de la 0.18.0
            // (après affinage), triés par identifiant.
            let new: Vec<Node> = st
                .nodes_matching(name)
                .unwrap()
                .into_iter()
                .filter(|n| {
                    n.id == *name
                        || n.qualified == *name
                        || n.short == *name
                        || is_suffix(&n.qualified, name)
                })
                .collect();
            let old: Vec<Node> = st
                .run_resolve(LEGACY_SQL, &[name])
                .unwrap()
                .into_iter()
                .filter(|n| {
                    n.id == *name
                        || n.qualified == *name
                        || n.short == *name
                        || is_suffix(&n.qualified, name)
                })
                .collect();
            assert_eq!(new, old, "nom {name:?}");
        }
        assert!(non_trivial > 100);
    }
}
