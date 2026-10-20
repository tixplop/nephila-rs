//! Parcours préfixe itératif d'un arbre syntaxique (DEC-054) : la pile d'exécution ne dépend
//! pas de la profondeur de l'arbre. Ordre du document, tous les enfants (nommés ou non).

use tree_sitter::{Node, TreeCursor};

/// Itérateur préfixe sur le sous-arbre de la racine donnée, avec un seul `TreeCursor`.
pub(crate) struct Preorder<'t> {
    cursor: TreeCursor<'t>,
    started: bool,
    done: bool,
    skip: bool,
}

impl<'t> Preorder<'t> {
    pub(crate) fn new(root: Node<'t>) -> Self {
        Preorder {
            cursor: root.walk(),
            started: false,
            done: false,
            skip: false,
        }
    }

    /// Nœud suivant : la racine d'abord, puis chaque nœud avant ses descendants.
    #[allow(clippy::should_implement_trait)]
    pub(crate) fn next(&mut self) -> Option<Node<'t>> {
        if self.done {
            return None;
        }
        if !self.started {
            self.started = true;
            return Some(self.cursor.node());
        }
        if !self.skip && self.cursor.goto_first_child() {
            return Some(self.cursor.node());
        }
        self.skip = false;
        loop {
            if self.cursor.goto_next_sibling() {
                return Some(self.cursor.node());
            }
            // Le curseur créé sur la racine ne sort jamais de son sous-arbre.
            if !self.cursor.goto_parent() {
                self.done = true;
                return None;
            }
        }
    }

    /// Le prochain `next` ne descend pas dans le dernier nœud rendu.
    pub(crate) fn skip_children(&mut self) {
        self.skip = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Lang;

    const SRC: &str = "class A { int x; void m(int a) { foo(a); } } class B { }";

    fn reference<'t>(n: Node<'t>, out: &mut Vec<&'t str>) {
        out.push(n.kind());
        let mut cur = n.walk();
        for c in n.children(&mut cur) {
            reference(c, out);
        }
    }

    fn find<'t>(root: Node<'t>, kind: &str) -> Node<'t> {
        let mut it = Preorder::new(root);
        while let Some(n) = it.next() {
            if n.kind() == kind {
                return n;
            }
        }
        panic!("{kind} absent");
    }

    #[test]
    fn req044_ac3_ordre_identique_au_parcours_recursif() {
        let tree = Lang::Java.parse(SRC).unwrap();
        let mut expected = Vec::new();
        reference(tree.root_node(), &mut expected);
        let mut got = Vec::new();
        let mut it = Preorder::new(tree.root_node());
        while let Some(n) = it.next() {
            got.push(n.kind());
        }
        assert_eq!(got, expected);
        assert!(got.len() > 20);
        assert!(it.next().is_none());
    }

    #[test]
    fn req044_ac3_skip_children_saute_les_descendants() {
        let tree = Lang::Java.parse(SRC).unwrap();
        let root = tree.root_node();
        let body = find(root, "class_body");
        let mut it = Preorder::new(root);
        let mut seen = Vec::new();
        while let Some(n) = it.next() {
            seen.push((n.kind(), n.start_byte()));
            if n.kind() == "class_body" && n.id() == body.id() {
                it.skip_children();
            }
        }
        assert!(!seen.iter().any(|(k, _)| *k == "field_declaration"));
        assert!(!seen.iter().any(|(k, _)| *k == "method_invocation"));
        // Le frère suivant (`class B`) est visité.
        assert_eq!(
            seen.iter()
                .filter(|(k, _)| *k == "class_declaration")
                .count(),
            2
        );
        assert_eq!(seen.iter().filter(|(k, _)| *k == "class_body").count(), 2);
    }

    #[test]
    fn req044_ac3_depart_interieur_ne_sort_pas_du_sous_arbre() {
        let tree = Lang::Java.parse(SRC).unwrap();
        let method = find(tree.root_node(), "method_declaration");
        let mut expected = Vec::new();
        reference(method, &mut expected);
        let mut got = Vec::new();
        let mut it = Preorder::new(method);
        while let Some(n) = it.next() {
            assert!(n.start_byte() >= method.start_byte() && n.end_byte() <= method.end_byte());
            got.push(n.kind());
        }
        assert_eq!(got, expected);
        assert_eq!(got[0], "method_declaration");
    }

    #[test]
    fn req044_ac3_feuille_seule() {
        let tree = Lang::Java.parse(SRC).unwrap();
        let id = find(tree.root_node(), "identifier");
        assert_eq!(id.child_count(), 0);
        let mut it = Preorder::new(id);
        assert_eq!(it.next().map(|n| n.id()), Some(id.id()));
        assert!(it.next().is_none());
        assert!(it.next().is_none());
    }
}
