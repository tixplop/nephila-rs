//! Accès paresseux au graphe pour les requêtes de parcours : nœuds et liens sont lus à la
//! demande (index SQLite) et mémorisés, au lieu de charger tout le graphe en mémoire.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::error::Result;
use crate::model::{EdgeKind, Node};
use crate::store::Store;

pub type Adjacent = Rc<[(String, EdgeKind)]>;

/// Lecture du graphe nécessaire aux parcours (`neighbors`, `path`) ; `Lazy` la fournit depuis
/// le stockage, les tests depuis un graphe en mémoire.
pub trait Graph {
    fn node(&self, id: &str) -> Result<Option<Node>>;
    /// Liens sortants (`outgoing`) ou entrants de `id` : (autre extrémité, genre), dans l'ordre
    /// du stockage (autre extrémité, puis genre).
    fn edges_at(&self, id: &str, outgoing: bool) -> Result<Adjacent>;
}

pub struct Lazy<'s> {
    store: &'s Store,
    nodes: RefCell<HashMap<String, Option<Node>>>,
    edges: RefCell<HashMap<(String, bool), Adjacent>>,
}

impl<'s> Lazy<'s> {
    pub fn new(store: &'s Store) -> Self {
        Lazy {
            store,
            nodes: RefCell::new(HashMap::new()),
            edges: RefCell::new(HashMap::new()),
        }
    }
}

impl Graph for Lazy<'_> {
    fn node(&self, id: &str) -> Result<Option<Node>> {
        if let Some(n) = self.nodes.borrow().get(id) {
            return Ok(n.clone());
        }
        let n = self.store.node_by_id(id)?;
        self.nodes.borrow_mut().insert(id.to_string(), n.clone());
        Ok(n)
    }

    fn edges_at(&self, id: &str, outgoing: bool) -> Result<Adjacent> {
        let key = (id.to_string(), outgoing);
        if let Some(e) = self.edges.borrow().get(&key) {
            return Ok(Rc::clone(e));
        }
        let e: Adjacent = self.store.edges_at(id, outgoing)?.into();
        self.edges.borrow_mut().insert(key, Rc::clone(&e));
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, FileRecord, FileState, NodeKind};

    #[test]
    fn req108_ac1_edges_at_rend_la_meme_liste_partagee_dans_l_ordre_du_stockage() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        let mut edges = Vec::new();
        for i in (0..1000).rev() {
            edges.push(Edge {
                source: "type:hub".into(),
                target: format!("type:t{i:04}"),
                kind: if i % 2 == 0 {
                    EdgeKind::Calls
                } else {
                    EdgeKind::References
                },
            });
            edges.push(Edge {
                source: format!("type:s{i:04}"),
                target: "type:hub".into(),
                kind: EdgeKind::Calls,
            });
        }
        let mut nodes = vec![Node::new(
            NodeKind::Type,
            "hub",
            "hub",
            Some("a.java"),
            None,
            None,
        )];
        for i in 0..1000 {
            for pre in ["t", "s"] {
                let q = format!("{pre}{i:04}");
                nodes.push(Node::new(
                    NodeKind::Type,
                    &q,
                    &q,
                    Some("a.java"),
                    None,
                    None,
                ));
            }
        }
        st.insert_pending_files(&[FileRecord {
            path: "a.java".into(),
            hash: "h".into(),
            size: 3,
            mtime: 7,
            recorded_at: 0,
            state: FileState::Pending,
        }])
        .unwrap();
        st.replace_file("a.java", &nodes, &[]).unwrap();
        st.replace_edges(&edges).unwrap();
        let lazy = Lazy::new(&st);
        for outgoing in [true, false] {
            let a = lazy.edges_at("type:hub", outgoing).unwrap();
            let b = lazy.edges_at("type:hub", outgoing).unwrap();
            assert!(Rc::ptr_eq(&a, &b));
            assert_eq!(a.len(), 1000);
            assert_eq!(&a[..], &st.edges_at("type:hub", outgoing).unwrap()[..]);
        }
        let out = lazy.edges_at("type:hub", true).unwrap();
        let inc = lazy.edges_at("type:hub", false).unwrap();
        assert!(!Rc::ptr_eq(&out, &inc));
        assert_eq!(out[0].0, "type:t0000");
    }
}
