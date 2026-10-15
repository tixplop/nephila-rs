//! Extracteur Java (REQ-006) : package, imports, types, méthodes, extends / implements.
//! Les appels sont ajoutés par T-024.

use tree_sitter::Node;

use crate::error::Result;
use crate::model::{EdgeKind, SubKind};

use super::jvm::{clean_type_name, Builder, INIT};
use super::{FileStructure, Lang};

/// Extrait la structure d'un fichier Java, en un seul passage sur l'arbre (tolérant aux erreurs).
pub fn extract(text: &str) -> Result<FileStructure> {
    let tree = Lang::Java.parse(text)?;
    let mut b = Builder::new();
    let src = text.as_bytes();
    let root = tree.root_node();
    let mut cur = root.walk();
    for child in root.children(&mut cur) {
        match child.kind() {
            "package_declaration" => {
                if let Some(name) = first_named(child, &["scoped_identifier", "identifier"]) {
                    b.set_package(&text_of(name, src));
                }
            }
            "import_declaration" => import(&mut b, child, src),
            _ => type_declaration(&mut b, child, src),
        }
    }
    Ok(b.finish())
}

fn text_of(n: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[n.byte_range()]).into_owned()
}

fn first_named<'a>(n: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cur = n.walk();
    let found = n
        .named_children(&mut cur)
        .find(|c| kinds.contains(&c.kind()));
    found
}

fn import(b: &mut Builder, n: Node, src: &[u8]) {
    let mut is_static = false;
    let mut wildcard = false;
    let mut path = None;
    let mut cur = n.walk();
    for c in n.children(&mut cur) {
        match c.kind() {
            "static" => is_static = true,
            "asterisk" => wildcard = true,
            "scoped_identifier" | "identifier" => path = Some(text_of(c, src)),
            _ => {}
        }
    }
    if let Some(p) = path {
        b.add_import(&p, wildcard, is_static);
    }
}

fn subkind_of(kind: &str) -> Option<SubKind> {
    match kind {
        "class_declaration" => Some(SubKind::Class),
        "interface_declaration" => Some(SubKind::Interface),
        "enum_declaration" => Some(SubKind::Enum),
        "record_declaration" => Some(SubKind::Record),
        "annotation_type_declaration" => Some(SubKind::Annotation),
        _ => None,
    }
}

/// Traite `n` s'il déclare un type ; sinon l'ignore.
fn type_declaration(b: &mut Builder, n: Node, src: &[u8]) {
    let Some(sub) = subkind_of(n.kind()) else {
        return;
    };
    let Some(name) = n.child_by_field_name("name") else {
        return;
    };
    let q = b.enter_type(&text_of(name, src), sub);

    // extends / implements.
    let mut cur = n.walk();
    for c in n.children(&mut cur) {
        match c.kind() {
            // class : `extends T` ; seul champ `superclass`.
            "superclass" => {
                for t in type_names(c, src) {
                    b.add_type_ref(&q, EdgeKind::Extends, &t);
                }
            }
            // class, enum, record : `implements A, B`.
            "super_interfaces" => {
                for t in type_names(c, src) {
                    b.add_type_ref(&q, EdgeKind::Implements, &t);
                }
            }
            // interface : `extends A, B` -> extends.
            "extends_interfaces" => {
                for t in type_names(c, src) {
                    b.add_type_ref(&q, EdgeKind::Extends, &t);
                }
            }
            // `permits` est ignoré.
            _ => {}
        }
    }

    if let Some(body) = n.child_by_field_name("body") {
        members(b, body, src);
    }
    b.exit_type();
}

/// Noms de types listés sous `n` (un `type_list`, ou un type direct), génériques retirés.
fn type_names(n: Node, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = n.walk();
    for c in n.named_children(&mut cur) {
        match c.kind() {
            "type_list" => out.extend(type_names(c, src)),
            "line_comment" | "block_comment" | "annotation" | "marker_annotation" => {}
            _ => out.push(clean_type_name(&text_of(c, src))),
        }
    }
    out
}

fn members(b: &mut Builder, body: Node, src: &[u8]) {
    let mut cur = body.walk();
    for m in body.named_children(&mut cur) {
        member(b, m, src);
    }
}

fn member(b: &mut Builder, m: Node, src: &[u8]) {
    match m.kind() {
        "method_declaration" | "annotation_type_element_declaration" => {
            if let Some(name) = m.child_by_field_name("name") {
                b.declare_method(&text_of(name, src));
            }
        }
        "constructor_declaration"
        | "compact_constructor_declaration"
        | "static_initializer"
        | "block" => {
            b.declare_method(INIT);
        }
        // Les constantes d'enum ne déclarent rien ; leurs déclarations sont dans ce conteneur.
        "enum_body_declarations" => members(b, m, src),
        _ => type_declaration(b, m, src),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::extract::Declared;
    use crate::model::{node_id, NodeKind};

    fn fx(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/langs")
            .join(name);
        std::fs::read_to_string(p).unwrap()
    }

    fn decl<'a>(s: &'a FileStructure, kind: NodeKind, q: &str) -> Option<&'a Declared> {
        s.declares
            .iter()
            .find(|d| d.kind == kind && d.qualified == q)
    }

    fn has_ref(s: &FileStructure, src: &str, kind: EdgeKind, target: &str) -> bool {
        s.references
            .iter()
            .any(|r| r.source == src && r.kind == kind && r.target == target)
    }

    const ORDER_SERVICE: &str = r#"package com.acme.shop;

import com.acme.model.Order;
import java.util.*;

public class OrderService {
    private OrderRepository repo;

    public void place() {
        repo.save(new Order());
    }
}
"#;

    #[test]
    fn req006_ac3_declarations_et_imports() {
        let s = extract(ORDER_SERVICE).unwrap();
        let t = decl(&s, NodeKind::Type, "com.acme.shop.OrderService").unwrap();
        assert_eq!(t.subkind, Some(SubKind::Class));
        assert!(decl(&s, NodeKind::Method, "com.acme.shop.OrderService.place").is_some());
        assert!(decl(&s, NodeKind::Package, "com.acme.shop").is_some());
        let src = node_id(NodeKind::Type, "com.acme.shop.OrderService");
        assert!(has_ref(&s, &src, EdgeKind::Imports, "com.acme.model.Order"));
        // Le joker ne donne pas de lien mais figure dans le contexte.
        assert_eq!(s.references.len(), 1);
        assert_eq!(
            s.context.as_deref(),
            Some("package com.acme.shop\nimport com.acme.model.Order\nimport java.util.*")
        );
    }

    #[test]
    fn req006_ac2_declarations_du_fichier_produites_par_un_seul_extract_sans_resume() {
        // Vérifie seulement que toutes les déclarations du fichier sortent d'un unique
        // `extract`, sans résumé. Le lien nœud -> fichier d'origine est prouvé de bout en
        // bout en T-033.
        let s = extract(ORDER_SERVICE).unwrap();
        assert!(s.declares.iter().all(|d| matches!(
            d.kind,
            NodeKind::Package | NodeKind::Type | NodeKind::Method
        )));
        assert!(s.declares.iter().all(|d| d.summary.is_none()));
    }

    #[test]
    fn req006_ac9_java_8_a_21() {
        let s = extract(&fx("Sample21.java")).unwrap();
        let p = "com.acme.shop.Sample21";
        let sub = |q: &str| decl(&s, NodeKind::Type, q).unwrap().subkind;
        assert_eq!(sub(p), Some(SubKind::Interface));
        assert_eq!(sub(&format!("{p}.A")), Some(SubKind::Record));
        assert_eq!(sub(&format!("{p}.B")), Some(SubKind::Record));
        assert_eq!(sub(&format!("{p}.Kind")), Some(SubKind::Enum));
        assert_eq!(sub(&format!("{p}.Marker")), Some(SubKind::Annotation));
        assert!(decl(&s, NodeKind::Method, &format!("{p}.describe")).is_some());
        assert!(decl(&s, NodeKind::Method, &format!("{p}.task")).is_some());
        // Constructeur compact du record.
        assert!(decl(&s, NodeKind::Method, &format!("{p}.A.<init>")).is_some());
        // Les records implémentent l'interface ; `permits` ne donne rien.
        let a = node_id(NodeKind::Type, &format!("{p}.A"));
        assert!(has_ref(&s, &a, EdgeKind::Implements, "Sample21"));
        let top = node_id(NodeKind::Type, p);
        assert!(!s
            .references
            .iter()
            .any(|r| r.source == top && r.kind != EdgeKind::Imports));
        // Import explicite, import statique sans lien.
        assert!(has_ref(&s, &top, EdgeKind::Imports, "java.util.List"));
        assert_eq!(
            s.references
                .iter()
                .filter(|r| r.kind == EdgeKind::Imports)
                .count(),
            1
        );
        assert!(s
            .context
            .as_deref()
            .unwrap()
            .contains("import static java.util.Objects.requireNonNull"));
    }

    #[test]
    fn classe_sealed_nested_enum_et_heritage() {
        let src = r#"package p;
public sealed abstract class Shape extends Base<String> implements Comparable<Shape>, java.io.Serializable permits Circle {
    public static class Inner { Inner() {} void run() {} void run(int x) {} }
    static { }
    { }
    Shape() {}
    Shape(int a) {}
    enum Color implements Named { RED, GREEN { void f() {} }; void paint() {} }
}
interface I extends J, K<X> { void m(); }
"#;
        let s = extract(src).unwrap();
        let t = |q: &str| decl(&s, NodeKind::Type, q).map(|d| d.subkind);
        assert_eq!(t("p.Shape"), Some(Some(SubKind::Class)));
        assert_eq!(t("p.Shape.Inner"), Some(Some(SubKind::Class)));
        assert_eq!(t("p.Shape.Color"), Some(Some(SubKind::Enum)));
        assert_eq!(t("p.I"), Some(Some(SubKind::Interface)));
        // Surcharges, constructeurs et initialiseurs fusionnés.
        let count = |q: &str| {
            s.declares
                .iter()
                .filter(|d| d.kind == NodeKind::Method && d.qualified == q)
                .count()
        };
        assert_eq!(count("p.Shape.Inner.run"), 1);
        assert_eq!(count("p.Shape.<init>"), 1);
        assert_eq!(count("p.Shape.Inner.<init>"), 1);
        assert_eq!(count("p.Shape.Color.paint"), 1);
        assert_eq!(count("p.I.m"), 1);
        let ty = |q: &str| node_id(NodeKind::Type, q);
        assert!(has_ref(&s, &ty("p.Shape"), EdgeKind::Extends, "Base"));
        assert!(has_ref(
            &s,
            &ty("p.Shape"),
            EdgeKind::Implements,
            "Comparable"
        ));
        assert!(has_ref(
            &s,
            &ty("p.Shape"),
            EdgeKind::Implements,
            "java.io.Serializable"
        ));
        assert!(has_ref(
            &s,
            &ty("p.Shape.Color"),
            EdgeKind::Implements,
            "Named"
        ));
        assert!(has_ref(&s, &ty("p.I"), EdgeKind::Extends, "J"));
        assert!(has_ref(&s, &ty("p.I"), EdgeKind::Extends, "K"));
        assert!(!s.references.iter().any(|r| r.target == "Circle"));
    }

    #[test]
    fn imports_de_chaque_type_de_premier_niveau_seulement() {
        let src = "package p; import a.X; class A { class N {} } class B {}";
        let s = extract(src).unwrap();
        let imports: Vec<&str> = s
            .references
            .iter()
            .filter(|r| r.kind == EdgeKind::Imports)
            .map(|r| r.source.as_str())
            .collect();
        assert_eq!(imports, ["type:p.A", "type:p.B"]);
    }

    #[test]
    fn sans_package_et_fichier_vide() {
        let s = extract("class A { void m() {} }").unwrap();
        assert!(decl(&s, NodeKind::Type, "A").is_some());
        assert!(decl(&s, NodeKind::Method, "A.m").is_some());
        assert!(s.context.is_none());
        assert_eq!(extract("").unwrap(), FileStructure::default());
    }

    #[test]
    fn req006_ac6_broken_java_ce_qui_precede_l_erreur() {
        let s = extract(&fx("Broken.java")).unwrap();
        assert!(decl(&s, NodeKind::Package, "com.acme.shop").is_some());
        assert!(decl(&s, NodeKind::Type, "com.acme.shop.Broken").is_some());
        assert!(decl(&s, NodeKind::Method, "com.acme.shop.Broken.ok").is_some());
    }

    #[test]
    fn req006_ac12_fichier_de_20000_lignes() {
        let mut src = String::from("package gen;\npublic class Big {\n");
        let mut n = 0;
        while 2 + 3 * n < 20_000 {
            src.push_str(&format!(
                "    public int m{n}(int a) {{\n        return a;\n    }}\n"
            ));
            n += 1;
        }
        src.push_str("}\n");
        let s = extract(&src).unwrap();
        let types = s
            .declares
            .iter()
            .filter(|d| d.kind == NodeKind::Type)
            .count();
        let methods = s
            .declares
            .iter()
            .filter(|d| d.kind == NodeKind::Method)
            .count();
        assert_eq!(types, 1);
        assert_eq!(methods, n);
        assert!(s
            .declares
            .iter()
            .filter(|d| d.kind == NodeKind::Method)
            .all(|d| d.qualified.starts_with("gen.Big.")));
    }
}
