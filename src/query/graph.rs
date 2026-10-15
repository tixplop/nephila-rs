//! Accès paresseux au graphe pour les requêtes de parcours : nœuds et liens sont lus à la
//! demande (index SQLite) et mémorisés, au lieu de charger tout le graphe en mémoire.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::error::Result;
use crate::model::{EdgeKind, Node};
use crate::store::Store;

type Adjacent = Vec<(String, EdgeKind)>;

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

    pub fn node(&self, id: &str) -> Result<Option<Node>> {
        if let Some(n) = self.nodes.borrow().get(id) {
            return Ok(n.clone());
        }
        let n = self.store.node_by_id(id)?;
        self.nodes.borrow_mut().insert(id.to_string(), n.clone());
        Ok(n)
    }

    /// Liens sortants (`outgoing`) ou entrants de `id` : (autre extrémité, genre).
    pub fn edges_at(&self, id: &str, outgoing: bool) -> Result<Vec<(String, EdgeKind)>> {
        let key = (id.to_string(), outgoing);
        if let Some(e) = self.edges.borrow().get(&key) {
            return Ok(e.clone());
        }
        let e = self.store.edges_at(id, outgoing)?;
        self.edges.borrow_mut().insert(key, e.clone());
        Ok(e)
    }
}
