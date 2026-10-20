//! Extracteur Groovy (REQ-006) : package, imports, classes, interfaces, enums, méthodes,
//! extends / implements, script (instructions et méthodes hors classe portées par le type
//! `NomFichier`). Les appels suivent les heuristiques de DEC-021 (`calls.rs`).
//!
//! Limites connues de la grammaire (DEC-016) : `trait` n'est pas reconnu (le mot-clé est lu comme
//! un appel de commande, sans heuristique de contournement) ; l'extracteur ne regarde pas
//! l'intérieur des nœuds `ERROR`.
//!
//! Premier niveau d'un script (DEC-023) : seules les déclarations de variables et les
//! instructions-expressions sont des instructions de script. Un appel de commande sans
//! parenthèses (`juxt_function_call` : `helper 42`, `println "hello"`) et une fermeture isolée ne
//! le sont pas : ni type de script, ni référence. Dans le corps d'une méthode ou d'une fonction,
//! l'appel de commande reste un appel (DEC-021). Règle sur l'arbre tel que la grammaire le donne,
//! sans heuristique : `println x` est lu comme une déclaration de variable (limite connue).

use std::path::Path;

use tree_sitter::Node;

use crate::error::Result;
use crate::model::{EdgeKind, SubKind};

use super::calls::{Calls, Locals, Receiver};
use super::jvm::{clean_type_name, file_stem_ident, Builder, INIT};
use super::walk::Preorder;
use super::{FileStructure, Lang};

/// Méthode qui porte les instructions d'un script (`Script.run()` de Groovy).
const SCRIPT_RUN: &str = "run";

/// Extrait la structure d'un fichier Groovy, en un seul passage sur l'arbre.
pub fn extract(path: &Path, text: &str) -> Result<FileStructure> {
    let tree = Lang::Groovy.parse(text)?;
    let mut x = Ctx {
        b: Builder::new(),
        calls: Calls::new(),
        src: text.as_bytes(),
        script_type: file_stem_ident(path),
    };
    let root = tree.root_node();
    let mut script_locals = Locals::new();
    let mut script_stmts = Vec::new();
    let mut cur = root.walk();
    for child in root.named_children(&mut cur) {
        match child.kind() {
            "package_declaration" => {
                if let Some(name) = first_named(child, &["scoped_identifier", "identifier"]) {
                    let name = x.text(name);
                    x.b.set_package(&name);
                }
            }
            "import_declaration" => x.import(child),
            "function_definition" => x.script_function(child),
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                x.type_declaration(child)
            }
            "local_variable_declaration" => {
                x.collect_locals(child, &mut script_locals);
                script_stmts.push(child);
            }
            // Une fermeture isolée n'est pas une instruction de script (DEC-023).
            "expression_statement" if !is_lone_closure(child) => {
                x.collect_locals(child, &mut script_locals);
                script_stmts.push(child);
            }
            _ => {}
        }
    }
    x.script_statements(&script_stmts, &script_locals);
    x.calls.finish(&mut x.b);
    Ok(x.b.finish())
}

struct Ctx<'a> {
    b: Builder,
    calls: Calls,
    src: &'a [u8],
    script_type: Option<String>,
}

/// `expression_statement` dont l'unique enfant nommé est une fermeture.
fn is_lone_closure(n: Node) -> bool {
    n.named_child_count() == 1 && n.named_child(0).is_some_and(|c| c.kind() == "closure")
}

fn first_named<'a>(n: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cur = n.walk();
    let found = n
        .named_children(&mut cur)
        .find(|c| kinds.contains(&c.kind()));
    found
}

impl Ctx<'_> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.src[n.byte_range()]).into_owned()
    }

    fn import(&mut self, n: Node) {
        let mut is_static = false;
        let mut wildcard = false;
        let mut path = None;
        let mut cur = n.walk();
        for c in n.children(&mut cur) {
            match c.kind() {
                "static" => is_static = true,
                "asterisk" => wildcard = true,
                "scoped_identifier" | "identifier" => path = Some(self.text(c)),
                _ => {}
            }
        }
        if let Some(p) = path {
            self.b.add_import(&p, wildcard, is_static);
        }
    }

    /// Méthode `def f() { … }` hors classe : méthode du type de script.
    fn script_function(&mut self, f: Node) {
        let (Some(st), Some(name)) = (self.script_type.clone(), f.child_by_field_name("name"))
        else {
            return;
        };
        let name = self.text(name);
        self.b.enter_type(&st, SubKind::Class);
        if let Some(q) = self.b.declare_method(&name) {
            self.calls.declare_method(&st_q(&self.b), &name);
            self.method_calls(&q, f);
        }
        self.b.exit_type();
    }

    fn script_statements(&mut self, stmts: &[Node], locals: &Locals) {
        let (Some(st), false) = (self.script_type.clone(), stmts.is_empty()) else {
            return;
        };
        self.b.enter_type(&st, SubKind::Class);
        if let Some(q) = self.b.declare_method(SCRIPT_RUN) {
            let types = self.b.type_stack().to_vec();
            for s in stmts {
                self.walk_calls(*s, &q, &types, locals);
            }
        }
        self.b.exit_type();
    }

    fn type_declaration(&mut self, n: Node) {
        let sub = match n.kind() {
            "class_declaration" => SubKind::Class,
            "interface_declaration" => SubKind::Interface,
            "enum_declaration" => SubKind::Enum,
            _ => return,
        };
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        let q = self.b.enter_type(&self.text(name), sub);

        let mut cur = n.walk();
        for c in n.children(&mut cur) {
            let kind = match c.kind() {
                "superclass" | "extends_interfaces" => EdgeKind::Extends,
                "super_interfaces" => EdgeKind::Implements,
                _ => continue,
            };
            for t in self.type_names(c) {
                self.b.add_type_ref(&q, kind, &t);
            }
        }
        if let Some(body) = n.child_by_field_name("body") {
            let mut cur = body.walk();
            for m in body.named_children(&mut cur) {
                self.member(m);
            }
        }
        self.b.exit_type();
    }

    fn type_names(&self, n: Node) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = n.walk();
        for c in n.named_children(&mut cur) {
            match c.kind() {
                "type_list" => out.extend(self.type_names(c)),
                "line_comment" | "block_comment" | "annotation" | "marker_annotation" => {}
                _ => out.push(clean_type_name(&self.text(c))),
            }
        }
        out
    }

    fn member(&mut self, m: Node) {
        match m.kind() {
            "method_declaration" => {
                if let Some(name) = m.child_by_field_name("name") {
                    let name = self.text(name);
                    if let Some(q) = self.b.declare_method(&name) {
                        if let Some(t) = self.b.current_type() {
                            self.calls.declare_method(t, &name);
                        }
                        self.method_calls(&q, m);
                    }
                }
            }
            "constructor_declaration" | "static_initializer" | "block" => {
                if let Some(q) = self.b.declare_method(INIT) {
                    self.method_calls(&q, m);
                }
            }
            "field_declaration" => {
                let Some(owner) = self.b.current_type().map(str::to_string) else {
                    return;
                };
                let declared = m.child_by_field_name("type").map(|t| self.text(t));
                let mut cur = m.walk();
                for d in m.children_by_field_name("declarator", &mut cur) {
                    if let Some(name) = d.child_by_field_name("name") {
                        let ty = self.variable_type(declared.as_deref(), d);
                        self.calls.declare_field(&owner, &self.text(name), &ty);
                    }
                }
            }
            "enum_body_declarations" => {
                let mut cur = m.walk();
                for c in m.named_children(&mut cur) {
                    self.member(c);
                }
            }
            _ => self.type_declaration(m),
        }
    }

    /// Type d'une variable : celui écrit, sinon (`def`/`var`) celui du `new T(…)` initialisant.
    fn variable_type(&self, declared: Option<&str>, declarator: Node) -> String {
        match declared.map(str::trim) {
            Some(t) if !t.is_empty() && t != "def" && t != "var" => t.to_string(),
            _ => match declarator.child_by_field_name("value") {
                Some(v) if v.kind() == "object_creation_expression" => v
                    .child_by_field_name("type")
                    .map(|t| self.text(t))
                    .unwrap_or_default(),
                _ => String::new(),
            },
        }
    }

    /// Types locaux (paramètres et variables), sans distinguer les portées.
    fn collect_locals(&self, root: Node, locals: &mut Locals) {
        let mut it = Preorder::new(root);
        while let Some(n) = it.next() {
            if n.kind() == "ERROR" {
                it.skip_children();
                continue;
            }
            self.collect_local(n, locals);
        }
    }

    fn collect_local(&self, n: Node, locals: &mut Locals) {
        match n.kind() {
            "formal_parameter" | "enhanced_for_statement" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let ty = n
                        .child_by_field_name("type")
                        .map(|t| self.text(t))
                        .unwrap_or_default();
                    locals.declare(&self.text(name), &ty);
                }
            }
            "local_variable_declaration" => {
                let declared = n.child_by_field_name("type").map(|t| self.text(t));
                let mut cur = n.walk();
                for d in n.children_by_field_name("declarator", &mut cur) {
                    if let Some(name) = d.child_by_field_name("name") {
                        let ty = self.variable_type(declared.as_deref(), d);
                        locals.declare(&self.text(name), &ty);
                    }
                }
            }
            _ => {}
        }
    }

    fn method_calls(&mut self, method: &str, decl: Node) {
        let mut locals = Locals::new();
        self.collect_locals(decl, &mut locals);
        let types = self.b.type_stack().to_vec();
        self.walk_calls(decl, method, &types, &locals);
    }

    fn walk_calls(&mut self, root: Node, method: &str, types: &[String], locals: &Locals) {
        let mut it = Preorder::new(root);
        while let Some(n) = it.next() {
            if n.kind() == "ERROR" {
                it.skip_children();
                continue;
            }
            self.visit_call(n, method, types, locals);
        }
    }

    fn visit_call(&mut self, n: Node, method: &str, types: &[String], locals: &Locals) {
        match n.kind() {
            "method_invocation" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let receiver = match n.child_by_field_name("object") {
                        None => Receiver::Implicit,
                        Some(o) => self.receiver_of(o),
                    };
                    let name = self.text(name);
                    self.calls.record(method, types, locals, receiver, &name);
                }
            }
            // `helper 42` : appel de commande sans parenthèses (dans une méthode, une fonction ou une
            // fermeture ; au premier niveau du script il n'arrive jamais ici, DEC-023).
            "juxt_function_call" => {
                if let Some(name) = n.child_by_field_name("name") {
                    if name.kind() == "identifier" {
                        let name = self.text(name);
                        self.calls
                            .record(method, types, locals, Receiver::Implicit, &name);
                    }
                }
            }
            _ => {}
        }
    }

    fn receiver_of(&self, o: Node) -> Receiver {
        match o.kind() {
            "this" => Receiver::This,
            "identifier" => Receiver::Name(self.text(o)),
            "field_access" => {
                match (
                    o.child_by_field_name("object"),
                    o.child_by_field_name("field"),
                ) {
                    (Some(ob), Some(f)) if ob.kind() == "this" => Receiver::ThisField(self.text(f)),
                    _ => Receiver::Unknown,
                }
            }
            _ => Receiver::Unknown,
        }
    }
}

/// Type courant de `b` (sommet de pile).
fn st_q(b: &Builder) -> String {
    b.current_type().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{node_id, NodeKind};
    use crate::parse::Declared;

    fn fx(name: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/langs")
            .join(name);
        std::fs::read_to_string(p).unwrap()
    }

    fn run(name: &str, text: &str) -> FileStructure {
        extract(Path::new(name), text).unwrap()
    }

    fn decl<'a>(s: &'a FileStructure, kind: NodeKind, q: &str) -> Option<&'a Declared> {
        s.declares
            .iter()
            .find(|d| d.kind == kind && d.qualified == q)
    }

    fn sub(s: &FileStructure, q: &str) -> Option<SubKind> {
        decl(s, NodeKind::Type, q).and_then(|d| d.subkind)
    }

    fn has_ref(s: &FileStructure, src: &str, kind: EdgeKind, target: &str) -> bool {
        s.references
            .iter()
            .any(|r| r.source == src && r.kind == kind && r.target == target)
    }

    #[test]
    fn req006_ac11_groovy_types_methodes_et_supertypes() {
        let s = run("Sample.groovy", &fx("Sample.groovy"));
        let p = "com.acme.shop";
        assert!(decl(&s, NodeKind::Package, p).is_some());
        assert_eq!(sub(&s, &format!("{p}.Named")), Some(SubKind::Interface));
        assert_eq!(sub(&s, &format!("{p}.Level")), Some(SubKind::Enum));
        assert_eq!(sub(&s, &format!("{p}.Base")), Some(SubKind::Class));
        assert_eq!(sub(&s, &format!("{p}.Service")), Some(SubKind::Class));
        assert!(decl(&s, NodeKind::Method, &format!("{p}.Service.run")).is_some());
        assert!(decl(&s, NodeKind::Method, &format!("{p}.Named.name")).is_some());
        let svc = node_id(NodeKind::Type, &format!("{p}.Service"));
        assert!(has_ref(&s, &svc, EdgeKind::Extends, "Base"));
        assert!(has_ref(&s, &svc, EdgeKind::Implements, "Named"));
        assert!(has_ref(
            &s,
            &svc,
            EdgeKind::Imports,
            "groovy.transform.CompileStatic"
        ));
        let run_m = node_id(NodeKind::Method, &format!("{p}.Service.run"));
        assert!(has_ref(&s, &run_m, EdgeKind::Calls, "Repo.save"));
    }

    #[test]
    fn req006_ac11_groovy_methode_hors_classe_dans_le_type_du_script() {
        let s = run("Sample.groovy", &fx("Sample.groovy"));
        let q = "com.acme.shop.Sample";
        assert!(decl(&s, NodeKind::Type, q).is_some());
        assert!(decl(&s, NodeKind::Method, &format!("{q}.helper")).is_some());
    }

    #[test]
    fn script_instructions_typees_et_new() {
        let s = run(
            "deploy.groovy",
            "import a.B\ndef x = new Foo()\nx.go()\nB b = null\nb.stop()\nclass K { def m(Order o) { def y = new Q(); y.z(); o.t(); this.m(o); n() } def n() {} }\n",
        );
        assert!(decl(&s, NodeKind::Type, "deploy").is_some());
        let r = node_id(NodeKind::Method, "deploy.run");
        assert!(has_ref(&s, &r, EdgeKind::Calls, "Foo.go"));
        assert!(has_ref(&s, &r, EdgeKind::Calls, "B.stop"));
        let m = node_id(NodeKind::Method, "K.m");
        for t in ["Q.z", "Order.t", "K.m", "K.n"] {
            assert!(has_ref(&s, &m, EdgeKind::Calls, t), "{t}");
        }
    }

    fn calls_from(s: &FileStructure) -> Vec<&crate::parse::Reference> {
        s.references
            .iter()
            .filter(|r| r.kind == EdgeKind::Calls)
            .collect()
    }

    #[test]
    fn req006_ac14_appel_avec_parentheses_vers_methode_de_script() {
        let s = run("Tool.groovy", "def helper() { }\nhelper(42)\n");
        let r = node_id(NodeKind::Method, "Tool.run");
        assert!(has_ref(&s, &r, EdgeKind::Calls, "Tool.helper"));
    }

    #[test]
    fn req006_ac14_appel_de_commande_de_premier_niveau_sans_reference() {
        let s = run("Tool.groovy", "def helper() { }\nhelper 42\n");
        assert!(calls_from(&s).is_empty());
        assert!(decl(&s, NodeKind::Type, "Tool").is_some()); // porte `helper`
        assert!(decl(&s, NodeKind::Method, "Tool.run").is_none());
    }

    #[test]
    fn req006_ac14_println_litteral_aucun_noeud() {
        let s = run("Hello.groovy", "println \"hello\"\n");
        assert_eq!(s, FileStructure::default());
    }

    #[test]
    fn req006_ac14_apply_plugin_gradle_aucun_noeud() {
        let s = run("common.gradle", "apply plugin: 'java'\n");
        assert_eq!(s, FileStructure::default());
    }

    #[test]
    fn req006_ac14_fermeture_isolee_aucun_noeud() {
        let s = run("Closure.groovy", "{ -> 1 }\n");
        assert_eq!(s, FileStructure::default());
    }

    #[test]
    fn req006_ac14_appel_de_commande_dans_une_fonction() {
        let s = run(
            "Tool.groovy",
            "def helper() { }\ndef f() {\n  helper 42\n}\n",
        );
        let f = node_id(NodeKind::Method, "Tool.f");
        assert!(has_ref(&s, &f, EdgeKind::Calls, "Tool.helper"));
    }

    #[test]
    fn req006_ac14_appel_de_commande_dans_une_fermeture_de_methode() {
        let s = run(
            "K.groovy",
            "class K {\n  void m() {\n    [1].each { helper 42 }\n  }\n}\n",
        );
        let m = node_id(NodeKind::Method, "K.m");
        assert!(has_ref(&s, &m, EdgeKind::Calls, "helper"));
    }

    // Limite connue de la grammaire, sans heuristique (DEC-023) : `println x` est lu comme une
    // déclaration de variable, donc comme une instruction de script, sans référence calls.
    #[test]
    fn limite_println_identifiant_lu_comme_declaration() {
        let s = run("Hello.groovy", "println x\n");
        assert!(decl(&s, NodeKind::Type, "Hello").is_some());
        assert!(decl(&s, NodeKind::Method, "Hello.run").is_some());
        assert!(calls_from(&s).is_empty());
    }

    // Limite connue de la grammaire, sans heuristique (DEC-023) : `helper 42` directement dans le
    // bloc d'une méthode de classe est un nœud `ERROR`, donc aucune référence.
    #[test]
    fn limite_appel_de_commande_dans_le_bloc_d_une_methode() {
        let text = "class K {\n  void m() {\n    helper 42\n  }\n}\n";
        let s = run("K.groovy", text);
        assert!(decl(&s, NodeKind::Type, "K").is_some());
        assert!(decl(&s, NodeKind::Method, "K.m").is_some());
        assert!(calls_from(&s).is_empty());
    }

    #[test]
    fn noeuds_error_ignores_pour_appels_et_locales() {
        // `helper(1)` est enveloppé par un nœud ERROR : ni appel, ni locale.
        let text =
            "class K {\n  void helper() { }\n  void m() {\n    for (x in helper(1) {\n  }\n}\n";
        let tree = Lang::Groovy.parse(text).unwrap();
        let sexp = tree.root_node().to_sexp();
        assert!(
            sexp.contains("(ERROR name: (identifier) (method_invocation"),
            "{sexp}"
        );
        let s = run("K.groovy", text);
        assert!(decl(&s, NodeKind::Method, "K.m").is_some());
        let m = node_id(NodeKind::Method, "K.m");
        assert!(!has_ref(&s, &m, EdgeKind::Calls, "K.helper"));
    }

    #[test]
    fn fichier_sans_instruction_n_a_pas_de_type_de_script() {
        let s = run("Only.groovy", "class A {}\n");
        assert!(decl(&s, NodeKind::Type, "Only").is_none());
        assert!(decl(&s, NodeKind::Type, "A").is_some());
    }

    #[test]
    fn req006_ac13_trait_groovy_limite_connue() {
        let text = "package p\n\ninterface Shape {\n    double area();\n}\n\nclass Circle implements Shape {\n    double area() { return 1.0; }\n}\n\ntrait Greeter {\n    String greet() { return \"hi\"; }\n}\n";
        let s = run("Shapes.groovy", text);
        assert_eq!(sub(&s, "p.Shape"), Some(SubKind::Interface));
        assert_eq!(sub(&s, "p.Circle"), Some(SubKind::Class));
        assert!(decl(&s, NodeKind::Method, "p.Circle.area").is_some());
        let c = node_id(NodeKind::Type, "p.Circle");
        assert!(has_ref(&s, &c, EdgeKind::Implements, "Shape"));
        assert!(!s.declares.iter().any(|d| d.qualified.contains("Greeter")));
        assert!(!s.declares.iter().any(|d| d.qualified.contains("greet")));
        // Ni type de script `Shapes`, ni `Shapes.run`, ni référence calls `trait` / `String`.
        assert!(decl(&s, NodeKind::Type, "p.Shapes").is_none());
        assert!(!s.declares.iter().any(|d| d.qualified.contains("Shapes")));
        assert!(calls_from(&s).is_empty());
    }

    #[test]
    fn fixture_trait_groovy_sans_echec() {
        let s = run("Trait.groovy", &fx("Trait.groovy"));
        assert!(s.declares.iter().all(|d| !d.qualified.contains("Greeter")));
    }

    #[test]
    fn entree_vide_ou_invalide() {
        assert_eq!(run("E.groovy", ""), FileStructure::default());
        let s = run("B.groovy", "class Ok { void f() { } }\nclass {{{ ((\n");
        assert!(decl(&s, NodeKind::Method, "Ok.f").is_some());
    }

    fn run_small_stack(name: &'static str, src: String) -> FileStructure {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || extract(Path::new(name), &src))
            .unwrap()
            .join()
            .unwrap()
            .unwrap()
    }

    #[test]
    fn req044_ac2_concatenation_de_10000_termes_en_pile_reduite() {
        let terms: String = (1..10_000).map(|i| format!(" + \"s{i}\"")).collect();
        let src = format!(
            "class G {{\n  String helper() {{ \"\" }}\n  String m() {{ return helper(){terms} }}\n}}\n"
        );
        let s = run_small_stack("G.groovy", src);
        assert!(has_ref(&s, "method:G.m", EdgeKind::Calls, "G.helper"));
    }

    #[test]
    fn req044_ac2_script_de_10000_termes_en_pile_reduite() {
        let terms: String = (1..10_000).map(|i| format!(" + \"s{i}\"")).collect();
        let src = format!("def helper() {{ \"\" }}\ndef s = helper(){terms}\n");
        let s = run_small_stack("Deep.groovy", src);
        assert!(has_ref(
            &s,
            "method:Deep.run",
            EdgeKind::Calls,
            "Deep.helper"
        ));
    }
}
