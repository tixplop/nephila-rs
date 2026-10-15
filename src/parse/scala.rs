//! Extracteur Scala 2 et 3 (REQ-006) : package, imports, types, méthodes, extends / with. Les
//! appels suivent les heuristiques de DEC-021 (`calls.rs`). L'extracteur ne regarde pas l'intérieur
//! des nœuds `ERROR`.

use std::path::Path;

use tree_sitter::Node;

use crate::error::Result;
use crate::model::{EdgeKind, SubKind};

use super::calls::{Calls, Locals, Receiver};
use super::jvm::{clean_type_name, file_stem_ident, Builder, INIT};
use super::{FileStructure, Lang};

/// Extrait la structure d'un fichier Scala, en un seul passage sur l'arbre.
pub fn extract(path: &Path, text: &str) -> Result<FileStructure> {
    let tree = Lang::Scala.parse(text)?;
    let mut x = Ctx {
        b: Builder::new(),
        calls: Calls::new(),
        src: text.as_bytes(),
        file_type: file_stem_ident(path).map(|s| format!("{s}$package")),
    };
    x.top_level(tree.root_node());
    x.calls.finish(&mut x.b);
    Ok(x.b.finish())
}

struct Ctx<'a> {
    b: Builder,
    calls: Calls,
    src: &'a [u8],
    file_type: Option<String>,
}

impl Ctx<'_> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.src[n.byte_range()]).into_owned()
    }

    /// Éléments de premier niveau d'un fichier ou d'un `package p { … }`.
    fn top_level(&mut self, container: Node) {
        let mut cur = container.walk();
        for child in container.named_children(&mut cur) {
            match child.kind() {
                "package_clause" => self.package(child),
                "import_declaration" => self.import(child),
                "function_definition" | "function_declaration" => self.in_file_type(child),
                "val_definition" | "var_definition" | "val_declaration" | "var_declaration" => {
                    self.in_file_type(child)
                }
                _ => self.type_declaration(child),
            }
        }
    }

    fn package(&mut self, n: Node) {
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        let outer = self.b.package().map(str::to_string);
        let name = self.text(name);
        // Les clauses successives s'emboîtent : `package a.b` puis `package c` = `a.b.c`.
        let full = match &outer {
            Some(o) => format!("{o}.{name}"),
            None => name,
        };
        self.b.set_package(&full);
        if let Some(body) = n.child_by_field_name("body") {
            self.top_level(body);
            self.b.restore_package(outer);
        }
    }

    /// Définition de premier niveau (Scala 3) : portée par `NomFichier$package`.
    fn in_file_type(&mut self, n: Node) {
        let Some(ft) = self.file_type.clone() else {
            return;
        };
        self.b.enter_type(&ft, SubKind::Object);
        self.member(n);
        self.b.exit_type();
    }

    fn import(&mut self, n: Node) {
        let mut cur = n.walk();
        let prefix: Vec<String> = n
            .children_by_field_name("path", &mut cur)
            .filter(Node::is_named)
            .map(|p| self.text(p))
            .collect();
        let prefix = prefix.join(".");
        if prefix.is_empty() {
            return;
        }
        let mut cur = n.walk();
        let mut has_selectors = false;
        for c in n.named_children(&mut cur) {
            match c.kind() {
                "namespace_wildcard" => {
                    has_selectors = true;
                    self.b.add_import(&prefix, true, false);
                }
                // Scala 3 : `import x.y.Z as W` vise `x.y.Z`.
                "as_renamed_identifier" | "arrow_renamed_identifier" => {
                    has_selectors = true;
                    if let Some(name) = c.child_by_field_name("name") {
                        let t = format!("{prefix}.{}", self.text(name));
                        self.b.add_import(&t, false, false);
                    }
                }
                "namespace_selectors" => {
                    has_selectors = true;
                    let mut c2 = c.walk();
                    for s in c.named_children(&mut c2) {
                        match s.kind() {
                            "namespace_wildcard" => self.b.add_import(&prefix, true, false),
                            "identifier" => {
                                let t = format!("{prefix}.{}", self.text(s));
                                self.b.add_import(&t, false, false);
                            }
                            "arrow_renamed_identifier" | "as_renamed_identifier" => {
                                if let Some(name) = s.child_by_field_name("name") {
                                    let t = format!("{prefix}.{}", self.text(name));
                                    self.b.add_import(&t, false, false);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if !has_selectors {
            self.b.add_import(&prefix, false, false);
        }
    }

    /// Traite `n` s'il déclare un type ; sinon l'ignore.
    fn type_declaration(&mut self, n: Node) {
        let sub = match n.kind() {
            "class_definition" => SubKind::Class,
            "trait_definition" => SubKind::Trait,
            "object_definition" => SubKind::Object,
            "enum_definition" => SubKind::Enum,
            _ => return,
        };
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name);
        if name.is_empty() {
            return;
        }
        let q = self.b.enter_type(&name, sub);

        if let Some(ext) = n.child_by_field_name("extend") {
            let mut cur = ext.walk();
            // Les jetons anonymes (`with`, `,`) portent aussi le champ `type` : on les écarte.
            let types = ext
                .children_by_field_name("type", &mut cur)
                .filter(Node::is_named);
            for (i, t) in types.enumerate() {
                let kind = if i == 0 {
                    EdgeKind::Extends
                } else {
                    EdgeKind::Implements
                };
                let target = clean_type_name(&self.text(t));
                self.b.add_type_ref(&q, kind, &target);
            }
        }
        if let Some(params) = n.child_by_field_name("class_parameters") {
            let mut cur = params.walk();
            for p in params.named_children(&mut cur) {
                if let (Some(name), Some(t)) =
                    (p.child_by_field_name("name"), p.child_by_field_name("type"))
                {
                    self.calls
                        .declare_field(&q, &self.text(name), &self.text(t));
                }
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

    fn member(&mut self, m: Node) {
        match m.kind() {
            "function_definition" | "function_declaration" => {
                let Some(name) = m.child_by_field_name("name") else {
                    return;
                };
                let name = self.text(name);
                // `def this(…)` : constructeur auxiliaire.
                let name = if name == "this" {
                    INIT.to_string()
                } else {
                    name
                };
                if let Some(q) = self.b.declare_method(&name) {
                    if let Some(t) = self.b.current_type() {
                        self.calls.declare_method(t, &name);
                    }
                    self.method_calls(&q, m);
                }
            }
            "val_definition" | "var_definition" | "val_declaration" | "var_declaration" => {
                let Some(owner) = self.b.current_type().map(str::to_string) else {
                    return;
                };
                if let Some((name, ty)) = self.variable(m) {
                    self.calls.declare_field(&owner, &name, &ty);
                }
            }
            "enum_case_definitions" => {}
            "template_body" | "enum_body" => {
                let mut cur = m.walk();
                for c in m.named_children(&mut cur) {
                    self.member(c);
                }
            }
            _ => self.type_declaration(m),
        }
    }

    /// Nom et type d'un `val`/`var` : type écrit, sinon celui du `new T(…)` initialisant.
    fn variable(&self, n: Node) -> Option<(String, String)> {
        let pat = n.child_by_field_name("pattern")?;
        if pat.kind() != "identifier" {
            return None;
        }
        let ty = match n.child_by_field_name("type") {
            Some(t) => self.text(t),
            None => n
                .child_by_field_name("value")
                .and_then(|v| self.constructed_type(v))
                .unwrap_or_default(),
        };
        Some((self.text(pat), ty))
    }

    fn constructed_type(&self, v: Node) -> Option<String> {
        if v.kind() != "instance_expression" {
            return None;
        }
        let mut cur = v.walk();
        let t = v
            .named_children(&mut cur)
            .find(|c| c.kind().ends_with("type_identifier") || c.kind() == "stable_type_identifier")
            .or_else(|| v.named_child(0));
        t.filter(|t| t.kind() != "arguments").map(|t| self.text(t))
    }

    fn collect_locals(&self, n: Node, locals: &mut Locals) {
        if n.kind() == "ERROR" {
            return;
        }
        match n.kind() {
            "parameter" | "class_parameter" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let ty = n
                        .child_by_field_name("type")
                        .map(|t| self.text(t))
                        .unwrap_or_default();
                    locals.declare(&self.text(name), &ty);
                }
            }
            "val_definition" | "var_definition" => {
                if let Some((name, ty)) = self.variable(n) {
                    locals.declare(&name, &ty);
                }
            }
            _ => {}
        }
        let mut cur = n.walk();
        for c in n.children(&mut cur) {
            self.collect_locals(c, locals);
        }
    }

    fn method_calls(&mut self, method: &str, decl: Node) {
        let mut locals = Locals::new();
        self.collect_locals(decl, &mut locals);
        let types = self.b.type_stack().to_vec();
        self.walk_calls(decl, method, &types, &locals);
    }

    fn walk_calls(&mut self, n: Node, method: &str, types: &[String], locals: &Locals) {
        if n.kind() == "ERROR" {
            return;
        }
        if n.kind() == "call_expression" {
            if let Some(mut f) = n.child_by_field_name("function") {
                if f.kind() == "generic_function" {
                    f = f.child_by_field_name("function").unwrap_or(f);
                }
                match f.kind() {
                    // `T(…)` : apply / construction, pas un appel de méthode connu.
                    "identifier" => {
                        let name = self.text(f);
                        if !name.chars().next().is_some_and(char::is_uppercase) {
                            self.calls
                                .record(method, types, locals, Receiver::Implicit, &name);
                        }
                    }
                    "field_expression" => {
                        if let (Some(v), Some(name)) = (
                            f.child_by_field_name("value"),
                            f.child_by_field_name("field"),
                        ) {
                            let r = self.receiver_of(v);
                            let name = self.text(name);
                            self.calls.record(method, types, locals, r, &name);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut cur = n.walk();
        for c in n.children(&mut cur) {
            self.walk_calls(c, method, types, locals);
        }
    }

    fn receiver_of(&self, o: Node) -> Receiver {
        match o.kind() {
            "identifier" if self.text(o) == "this" => Receiver::This,
            "this" => Receiver::This,
            "identifier" => Receiver::Name(self.text(o)),
            "field_expression" => {
                match (
                    o.child_by_field_name("value"),
                    o.child_by_field_name("field"),
                ) {
                    (Some(v), Some(f)) if self.text(v) == "this" => {
                        Receiver::ThisField(self.text(f))
                    }
                    _ => Receiver::Unknown,
                }
            }
            _ => Receiver::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::extract::Declared;
    use crate::model::{node_id, NodeKind};

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

    fn tid(q: &str) -> String {
        node_id(NodeKind::Type, q)
    }

    #[test]
    fn req006_ac11_scala_2_sous_genres_et_supertypes() {
        let s = run("Scala2.scala", &fx("Scala2.scala"));
        let p = "com.acme.shop";
        assert!(decl(&s, NodeKind::Package, p).is_some());
        assert_eq!(sub(&s, &format!("{p}.Named")), Some(SubKind::Trait));
        assert_eq!(sub(&s, &format!("{p}.Base")), Some(SubKind::Class));
        assert_eq!(sub(&s, &format!("{p}.Item")), Some(SubKind::Class));
        assert_eq!(sub(&s, &format!("{p}.Registry")), Some(SubKind::Object));
        assert!(decl(&s, NodeKind::Method, &format!("{p}.Registry.add")).is_some());
        assert!(decl(&s, NodeKind::Method, &format!("{p}.Named.name")).is_some());
        let base = tid(&format!("{p}.Base"));
        assert!(has_ref(&s, &base, EdgeKind::Extends, "Named"));
        assert!(has_ref(&s, &base, EdgeKind::Implements, "Serializable"));
        let item = tid(&format!("{p}.Item"));
        assert!(has_ref(&s, &item, EdgeKind::Extends, "Base"));
    }

    #[test]
    fn req006_ac11_scala_imports_selecteurs_et_joker() {
        let s = run("Scala2.scala", &fx("Scala2.scala"));
        let svc = tid("com.acme.shop.Service");
        // Chaque type de premier niveau importe chaque import explicite, renommé ou non.
        let named = tid("com.acme.shop.Named");
        assert!(has_ref(
            &s,
            &named,
            EdgeKind::Imports,
            "scala.collection.mutable.Map"
        ));
        assert!(has_ref(
            &s,
            &named,
            EdgeKind::Imports,
            "scala.collection.mutable.ListBuffer"
        ));
        assert!(!s
            .references
            .iter()
            .any(|r| r.source == svc && r.kind == EdgeKind::Extends));

        let w = run(
            "W.scala",
            "import a.b._\nimport c.*\nimport d.{_, E}\nclass K\n",
        );
        let k = tid("K");
        assert!(has_ref(&w, &k, EdgeKind::Imports, "d.E"));
        assert_eq!(
            w.references
                .iter()
                .filter(|r| r.kind == EdgeKind::Imports)
                .count(),
            1
        );
        assert_eq!(
            w.context.as_deref(),
            Some("import a.b.*\nimport c.*\nimport d.*\nimport d.E")
        );
    }

    #[test]
    fn req006_ac11_scala_appel_via_parametre_de_classe() {
        let s = run("Scala2.scala", &fx("Scala2.scala"));
        let m = node_id(NodeKind::Method, "com.acme.shop.Service.run");
        assert!(has_ref(&s, &m, EdgeKind::Calls, "Repo.save"));
    }

    #[test]
    fn req006_ac11_scala_3_enum_et_definitions_de_premier_niveau() {
        let s = run("Scala3.scala", &fx("Scala3.scala"));
        let p = "com.acme.shop";
        assert_eq!(sub(&s, &format!("{p}.Color")), Some(SubKind::Enum));
        assert_eq!(sub(&s, &format!("{p}.Greeter")), Some(SubKind::Trait));
        assert!(decl(&s, NodeKind::Type, &format!("{p}.Scala3$package")).is_some());
        assert!(decl(&s, NodeKind::Method, &format!("{p}.Scala3$package.top")).is_some());
        assert!(has_ref(
            &s,
            &tid(&format!("{p}.Hello")),
            EdgeKind::Extends,
            "Greeter"
        ));
    }

    #[test]
    fn packages_en_clauses_multiples_et_a_accolades() {
        let s = run("A.scala", "package a.b\npackage c\nclass X\n");
        assert!(decl(&s, NodeKind::Package, "a.b").is_some());
        assert!(decl(&s, NodeKind::Package, "a.b.c").is_some());
        assert!(decl(&s, NodeKind::Type, "a.b.c.X").is_some());

        let s = run("A.scala", "package foo {\n  class A\n}\nclass B\n");
        assert!(decl(&s, NodeKind::Type, "foo.A").is_some());
        assert!(decl(&s, NodeKind::Type, "B").is_some());
    }

    #[test]
    fn appels_this_variable_locale_et_constructeur() {
        let s = run(
            "S.scala",
            "class S(val repo: Repo) {\n  def a(o: Order): Unit = { repo.save(o); o.total(); this.b(); c(); val z = new Thing(); z.go(); this.repo.x(); Util.f() }\n  def b(): Unit = ()\n}\n",
        );
        let m = node_id(NodeKind::Method, "S.a");
        for t in [
            "Repo.save",
            "Order.total",
            "S.b",
            "S.c",
            "Thing.go",
            "Repo.x",
            "Util.f",
        ] {
            let t = if t == "S.c" { "c" } else { t };
            assert!(has_ref(&s, &m, EdgeKind::Calls, t), "{t}");
        }
    }

    #[test]
    fn extends_with_ensemble_exact_sans_jetons_anonymes() {
        let s = run("A.scala", "class A extends B with C with D\n");
        let a = tid("A");
        let got: Vec<(EdgeKind, String)> = s
            .references
            .iter()
            .filter(|r| r.source == a)
            .map(|r| (r.kind, r.target.clone()))
            .collect();
        let mut want = vec![
            (EdgeKind::Extends, "B".to_string()),
            (EdgeKind::Implements, "C".to_string()),
            (EdgeKind::Implements, "D".to_string()),
        ];
        let mut got = got;
        got.sort_by_key(|x| x.1.clone());
        want.sort_by_key(|x| x.1.clone());
        assert_eq!(got, want);
    }

    #[test]
    fn import_scala_3_renomme_as() {
        let s = run(
            "R.scala",
            "import x.y.Z as W\nimport x.y.{Q as R, S}\nclass K\n",
        );
        let k = tid("K");
        for t in ["x.y.Z", "x.y.Q", "x.y.S"] {
            assert!(has_ref(&s, &k, EdgeKind::Imports, t), "{t}");
        }
        assert!(!has_ref(&s, &k, EdgeKind::Imports, "x.y"));
        assert!(!s
            .references
            .iter()
            .any(|r| r.kind == EdgeKind::Imports
                && (r.target.ends_with('W') || r.target.ends_with('R'))));
    }

    #[test]
    fn noeuds_error_ignores_pour_appels_et_locales() {
        // Le corps contient un nœud ERROR qui enveloppe un appel `helper(1)` : ignoré.
        let text = "class K {\n  def helper(): Unit = ()\n  def m(x: Int): Unit = {\n    if (a foo.helper(1)\n  }\n}\n";
        let tree = Lang::Scala.parse(text).unwrap();
        assert!(
            tree.root_node()
                .to_sexp()
                .contains("(ERROR (infix_expression")
                && tree
                    .root_node()
                    .to_sexp()
                    .contains("(call_expression function: (identifier)")
        );
        let s = run("K.scala", text);
        assert!(decl(&s, NodeKind::Method, "K.m").is_some());
        assert!(!has_ref(
            &s,
            &node_id(NodeKind::Method, "K.m"),
            EdgeKind::Calls,
            "K.helper"
        ));
    }

    #[test]
    fn entree_vide_ou_invalide() {
        assert_eq!(run("E.scala", ""), FileStructure::default());
        let s = run("B.scala", "class Ok { def f() = 1 }\nclass {{{ ((\n");
        assert!(decl(&s, NodeKind::Method, "Ok.f").is_some());
    }
}
