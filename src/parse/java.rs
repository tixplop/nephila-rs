//! Extracteur Java (REQ-006) : package, imports, types, méthodes, extends / implements.
//! Les appels suivent les heuristiques de DEC-021 (`calls.rs`).

use tree_sitter::Node;

use crate::error::Result;
use crate::model::{EdgeKind, SubKind};

use super::calls::{Calls, Locals, Receiver};
use super::jvm::{clean_type_name, Builder, INIT};
use super::walk::Preorder;
use super::{FileStructure, Lang};

/// Extrait la structure d'un fichier Java, en un seul passage sur l'arbre (tolérant aux erreurs).
pub fn extract(text: &str) -> Result<FileStructure> {
    let tree = Lang::Java.parse(text)?;
    let mut b = Builder::new();
    let mut calls = Calls::new();
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
            _ => type_declaration(&mut b, &mut calls, child, src),
        }
    }
    calls.finish(&mut b);
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
fn type_declaration(b: &mut Builder, calls: &mut Calls, n: Node, src: &[u8]) {
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

    // Composantes d'un record : propriétés du type.
    if let Some(params) = n.child_by_field_name("parameters") {
        let mut cur = params.walk();
        for p in params.named_children(&mut cur) {
            if let (Some(t), Some(name)) =
                (p.child_by_field_name("type"), p.child_by_field_name("name"))
            {
                calls.declare_field(&q, &text_of(name, src), &text_of(t, src));
            }
        }
    }
    if let Some(body) = n.child_by_field_name("body") {
        members(b, calls, body, src);
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

fn members(b: &mut Builder, calls: &mut Calls, body: Node, src: &[u8]) {
    let mut cur = body.walk();
    for m in body.named_children(&mut cur) {
        member(b, calls, m, src);
    }
}

fn member(b: &mut Builder, calls: &mut Calls, m: Node, src: &[u8]) {
    match m.kind() {
        "method_declaration" | "annotation_type_element_declaration" => {
            if let Some(name) = m.child_by_field_name("name") {
                let name = text_of(name, src);
                if let Some(q) = b.declare_method(&name) {
                    if let Some(t) = b.current_type() {
                        calls.declare_method(t, &name);
                    }
                    method_calls(b, calls, &q, m, src);
                }
            }
        }
        "constructor_declaration" | "compact_constructor_declaration" => {
            if let Some(q) = b.declare_method(INIT) {
                method_calls(b, calls, &q, m, src);
            }
        }
        "static_initializer" | "block" => {
            if let Some(q) = b.declare_method(INIT) {
                method_calls(b, calls, &q, m, src);
            }
        }
        "field_declaration" => {
            let Some(ty) = m.child_by_field_name("type") else {
                return;
            };
            let Some(owner) = b.current_type().map(str::to_string) else {
                return;
            };
            let mut cur = m.walk();
            for d in m.children_by_field_name("declarator", &mut cur) {
                if let Some(name) = d.child_by_field_name("name") {
                    calls.declare_field(&owner, &text_of(name, src), &text_of(ty, src));
                }
            }
        }
        // Les constantes d'enum ne déclarent rien ; leurs déclarations sont dans ce conteneur.
        "enum_body_declarations" => members(b, calls, m, src),
        _ => type_declaration(b, calls, m, src),
    }
}

/// Collecte les types locaux de `n` (paramètres, variables, boucles, motifs), sans distinguer
/// les portées : un nom redéclaré garde sa dernière déclaration.
fn collect_locals(n: Node, src: &[u8], locals: &mut Locals) {
    let typed = |node: Node, locals: &mut Locals| {
        if let (Some(t), Some(name)) = (
            node.child_by_field_name("type"),
            node.child_by_field_name("name"),
        ) {
            locals.declare(&text_of(name, src), &text_of(t, src));
        }
    };
    let mut it = Preorder::new(n);
    while let Some(n) = it.next() {
        match n.kind() {
            "formal_parameter" | "enhanced_for_statement" => typed(n, locals),
            "resource" => {
                if let (Some(t), Some(name)) =
                    (n.child_by_field_name("type"), n.child_by_field_name("name"))
                {
                    let ty = text_of(t, src);
                    locals.declare(
                        &text_of(name, src),
                        &initialised_type(&ty, n.child_by_field_name("value"), src),
                    );
                }
            }
            "local_variable_declaration" => {
                if let Some(t) = n.child_by_field_name("type") {
                    let ty = text_of(t, src);
                    let mut cur = n.walk();
                    for d in n.children_by_field_name("declarator", &mut cur) {
                        if let Some(name) = d.child_by_field_name("name") {
                            let ty = initialised_type(&ty, d.child_by_field_name("value"), src);
                            locals.declare(&text_of(name, src), &ty);
                        }
                    }
                }
            }
            "type_pattern" => {
                let mut cur = n.walk();
                let kids: Vec<Node> = n.named_children(&mut cur).collect();
                if let [t, name] = kids.as_slice() {
                    if name.kind() == "identifier" {
                        locals.declare(&text_of(*name, src), &text_of(*t, src));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Type d'une variable : celui écrit, ou pour `var` celui du constructeur `new T(…)`.
fn initialised_type(declared: &str, value: Option<Node>, src: &[u8]) -> String {
    if declared.trim() != "var" {
        return declared.to_string();
    }
    match value {
        Some(v) if v.kind() == "object_creation_expression" => v
            .child_by_field_name("type")
            .map(|t| text_of(t, src))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Enregistre les appels du corps (lambdas et classes anonymes comprises) de `method` (nom
/// qualifié), paramètres compris dans `decl`.
fn method_calls(b: &Builder, calls: &mut Calls, method: &str, decl: Node, src: &[u8]) {
    let mut locals = Locals::new();
    collect_locals(decl, src, &mut locals);
    let types = b.type_stack().to_vec();
    walk_calls(decl, src, method, &types, &locals, calls);
}

fn walk_calls(
    n: Node,
    src: &[u8],
    method: &str,
    types: &[String],
    locals: &Locals,
    calls: &mut Calls,
) {
    let mut it = Preorder::new(n);
    while let Some(n) = it.next() {
        if n.kind() == "method_invocation" {
            if let Some(name) = n.child_by_field_name("name") {
                let receiver = match n.child_by_field_name("object") {
                    None => Receiver::Implicit,
                    Some(o) => receiver_of(o, src),
                };
                calls.record(method, types, locals, receiver, &text_of(name, src));
            }
        }
    }
}

fn receiver_of(o: Node, src: &[u8]) -> Receiver {
    match o.kind() {
        "this" => Receiver::This,
        "identifier" => Receiver::Name(text_of(o, src)),
        "field_access" => {
            let obj = o.child_by_field_name("object");
            let field = o.child_by_field_name("field");
            match (obj, field) {
                (Some(ob), Some(f)) if ob.kind() == "this" => Receiver::ThisField(text_of(f, src)),
                _ => Receiver::Unknown,
            }
        }
        _ => Receiver::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{node_id, NodeKind};
    use crate::parse::Declared;

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
        // `new Order()` ne donne pas de lien calls ; l'appel sur `repo` en donne un.
        let place = node_id(NodeKind::Method, "com.acme.shop.OrderService.place");
        assert!(has_ref(&s, &place, EdgeKind::Calls, "OrderRepository.save"));
        // Le joker ne donne pas de lien mais figure dans le contexte.
        assert_eq!(s.references.len(), 2);
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

    fn calls_of(s: &FileStructure) -> Vec<(String, String)> {
        s.references
            .iter()
            .filter(|r| r.kind == EdgeKind::Calls)
            .map(|r| (r.source.clone(), r.target.clone()))
            .collect()
    }

    #[test]
    fn req006_ac9_appels_dans_lambda_et_classe_anonyme_attribues_a_la_methode() {
        let src = r#"package p;
class S {
    private Repo repo;
    void run(List<Order> orders, Mailer mailer) {
        orders.forEach(o -> repo.save(o));
        Runnable r = new Runnable() { public void run() { mailer.send(); } };
        java.util.function.Supplier<Cache> f = () -> Util.make();
    }
}
"#;
        let c = calls_of(&extract(src).unwrap());
        let from = "method:p.S.run";
        for t in ["List.forEach", "Repo.save", "Mailer.send", "Util.make"] {
            assert!(c.contains(&(from.to_string(), t.to_string())), "{t}: {c:?}");
        }
    }

    #[test]
    fn req006_ac9_fixture_sample21() {
        let s = extract(&fx("Sample21.java")).unwrap();
        let c = calls_of(&s);
        let d = "method:com.acme.shop.Sample21.describe".to_string();
        assert!(c.contains(&(d.clone(), "A.qty".to_string())), "{c:?}");
        assert!(c.contains(&(d, "String.join".to_string())));
        // `System.out.println` est une chaîne : aucune référence.
        assert!(!c.iter().any(|(_, t)| t.contains("println")));
    }

    #[test]
    fn appels_this_implicite_champ_et_constructeur() {
        let src = r#"class S {
    Repo r = new Repo();
    S() { init(); this.helper(); this.r.open(); }
    void init() {}
    void helper() { other(); }
    void t() { var x = new Foo<String>(); x.go(); Foo y = make(); y.stop(); }
}"#;
        let c = calls_of(&extract(src).unwrap());
        let has = |m: &str, t: &str| c.contains(&(format!("method:S.{m}"), t.to_string()));
        assert!(has("<init>", "S.init"));
        assert!(has("<init>", "S.helper"));
        assert!(has("<init>", "Repo.open"));
        assert!(has("helper", "other"));
        assert!(has("t", "Foo.go"));
        assert!(has("t", "Foo.stop"));
        assert!(has("t", "make"));
        // Aucun lien calls pour `new`.
        assert!(!c
            .iter()
            .any(|(_, t)| t == "Foo.<init>" || t == "Repo.<init>"));
    }

    #[test]
    fn appels_negatifs_chaine_et_var_sans_initialiseur() {
        let src = r#"class S {
    void m(Repo repo) {
        var a = repo.find();
        a.use();
        repo.find().use();
        unknown.call();
        int[] xs = null;
        xs.clone();
        getRepo().save();
    }
}"#;
        let c = calls_of(&extract(src).unwrap());
        let targets: Vec<&str> = c.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(targets, ["Repo.find", "getRepo"], "{c:?}");
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

    fn in_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn req044_ac2_concatenation_de_10000_termes_en_pile_reduite() {
        let terms: String = (1..10_000).map(|i| format!(" + \"s{i}\"")).collect();
        let src = format!(
            "class Big {{\n  String helper() {{ return \"\"; }}\n  String m() {{\n    return helper(){terms};\n  }}\n}}\n"
        );
        let s = in_small_stack(move || extract(&src)).unwrap();
        assert!(has_ref(&s, "method:Big.m", EdgeKind::Calls, "Big.helper"));
    }

    #[test]
    fn req044_ac2_5000_parentheses_imbriquees_en_pile_reduite() {
        let src = format!(
            "class Big {{\n  String helper() {{ return \"\"; }}\n  String m() {{\n    return {}helper(){};\n  }}\n}}\n",
            "(".repeat(5_000),
            ")".repeat(5_000)
        );
        let s = in_small_stack(move || extract(&src)).unwrap();
        assert!(has_ref(&s, "method:Big.m", EdgeKind::Calls, "Big.helper"));
    }
}
