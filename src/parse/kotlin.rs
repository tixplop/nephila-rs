//! Extracteur Kotlin (REQ-006) : package, imports, types, méthodes, supertypes. Les appels
//! suivent les heuristiques de DEC-021 (`calls.rs`). L'extracteur ne regarde pas l'intérieur
//! des nœuds `ERROR` (limite connue de la grammaire sur les corps tenant sur une ligne).

use std::collections::HashMap;
use std::path::Path;

use tree_sitter::Node;

use crate::error::Result;
use crate::model::{EdgeKind, SubKind};

use super::calls::{Calls, Locals, Receiver};
use super::jvm::{Builder, INIT};
use super::walk::Preorder;
use super::{FileStructure, Lang};

/// Extrait la structure d'un fichier Kotlin (`.kt` ou script `.kts`), en un passage.
pub fn extract(path: &Path, text: &str) -> Result<FileStructure> {
    let tree = Lang::Kotlin.parse(text)?;
    let mut b = Builder::new();
    let mut calls = Calls::new();
    let mut al = Aliases::default();
    let src = text.as_bytes();
    let file_type = file_type_name(path);
    let root = tree.root_node();
    let mut cur = root.walk();
    for child in root.children(&mut cur) {
        match child.kind() {
            "package_header" => {
                if let Some(q) = first_named(child, &["qualified_identifier", "identifier"]) {
                    b.set_package(&text_of(q, src));
                }
            }
            "import" => import(&mut b, &mut al, child, src),
            "function_declaration" => {
                if let Some(ft) = &file_type {
                    // Premier niveau : la pile est vide, `enter_type` ne refuse jamais.
                    let _ = b.enter_type(ft, SubKind::Class);
                    function(&mut b, &mut calls, &al, child, src);
                    b.exit_type();
                }
            }
            _ => declaration(&mut b, &mut calls, &al, child, src),
        }
    }
    calls.finish(&mut b);
    Ok(b.finish())
}

/// Alias d'import (`import a.b.Base as B`) : alias -> nom importé (dernier segment).
#[derive(Default)]
struct Aliases(HashMap<String, String>);

impl Aliases {
    /// Remplace l'alias en tête d'un nom de type (`B`, `B?`, `B<T>`) par le nom importé.
    fn resolve(&self, ty: &str) -> String {
        let t = ty.trim();
        let end = t
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(t.len());
        match self.0.get(&t[..end]) {
            Some(real) if !t[end..].starts_with('.') => format!("{real}{}", &t[end..]),
            _ => t.to_string(),
        }
    }
}

/// Type porteur des fonctions de premier niveau : `NomFichierKt` pour `.kt`, `NomFichier`
/// pour un script `.kts`.
fn file_type_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let stem: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        return None;
    }
    if path.extension().and_then(|e| e.to_str()) == Some("kts") {
        return Some(stem);
    }
    let mut chars = stem.chars();
    let first = chars.next()?.to_uppercase().collect::<String>();
    Some(format!("{first}{}Kt", chars.as_str()))
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

/// Premier enfant qui est un type (`user_type`, `nullable_type`...).
fn type_child(n: Node) -> Option<Node> {
    let mut cur = n.walk();
    let found = n
        .named_children(&mut cur)
        .find(|c| c.kind().ends_with("_type"));
    found
}

fn import(b: &mut Builder, al: &mut Aliases, n: Node, src: &[u8]) {
    let mut wildcard = false;
    let mut path = None;
    let mut seen_as = false;
    let mut alias = None;
    let mut cur = n.walk();
    for c in n.children(&mut cur) {
        match c.kind() {
            "*" => wildcard = true,
            "as" => seen_as = true,
            "identifier" if seen_as => alias = Some(text_of(c, src)),
            "qualified_identifier" | "identifier" if !seen_as && path.is_none() => {
                path = Some(text_of(c, src))
            }
            _ => {}
        }
    }
    if let Some(p) = path {
        if let (Some(a), false) = (alias, wildcard) {
            if let Some(last) = p.rsplit('.').next() {
                al.0.insert(a, last.to_string());
            }
        }
        b.add_import(&p, wildcard, false);
    }
}

fn modifiers_text(n: Node, src: &[u8]) -> String {
    first_named(n, &["modifiers"])
        .map(|m| text_of(m, src))
        .unwrap_or_default()
}

fn has_keyword(n: Node, kw: &str) -> bool {
    let mut cur = n.walk();
    let found = n.children(&mut cur).any(|c| c.kind() == kw);
    found
}

/// Traite `n` s'il déclare un type ; sinon l'ignore.
fn declaration(b: &mut Builder, calls: &mut Calls, al: &Aliases, n: Node, src: &[u8]) {
    let (sub, name) = match n.kind() {
        "class_declaration" => {
            let mods = modifiers_text(n, src);
            let words: Vec<&str> = mods.split_whitespace().collect();
            let sub = if has_keyword(n, "interface") {
                SubKind::Interface
            } else if words.contains(&"enum") {
                SubKind::Enum
            } else if words.contains(&"annotation") {
                SubKind::Annotation
            } else {
                SubKind::Class
            };
            (sub, n.child_by_field_name("name").map(|x| text_of(x, src)))
        }
        "object_declaration" => (
            SubKind::Object,
            n.child_by_field_name("name").map(|x| text_of(x, src)),
        ),
        "companion_object" => (
            SubKind::Object,
            Some(
                n.child_by_field_name("name")
                    .map(|x| text_of(x, src))
                    .unwrap_or_else(|| "Companion".to_string()),
            ),
        ),
        _ => return,
    };
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(q) = b.enter_type(&name, sub) else {
        return;
    };

    if let Some(specs) = first_named(n, &["delegation_specifiers"]) {
        let mut cur = specs.walk();
        for s in specs.named_children(&mut cur) {
            if s.kind() != "delegation_specifier" {
                continue;
            }
            let mut c2 = s.walk();
            for part in s.named_children(&mut c2) {
                match part.kind() {
                    "constructor_invocation" => {
                        if let Some(t) = type_child(part) {
                            b.add_type_ref(&q, EdgeKind::Extends, &al.resolve(&text_of(t, src)));
                        }
                    }
                    "explicit_delegation" => {
                        if let Some(t) = type_child(part) {
                            b.add_type_ref(&q, EdgeKind::Implements, &al.resolve(&text_of(t, src)));
                        }
                    }
                    k if k.ends_with("_type") => {
                        b.add_type_ref(&q, EdgeKind::Implements, &al.resolve(&text_of(part, src)));
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(pc) = first_named(n, &["primary_constructor"]) {
        if let Some(params) = first_named(pc, &["class_parameters"]) {
            let mut cur = params.walk();
            for p in params.named_children(&mut cur) {
                if p.kind() == "class_parameter" && (has_keyword(p, "val") || has_keyword(p, "var"))
                {
                    if let (Some(id), Some(t)) = (first_named(p, &["identifier"]), type_child(p)) {
                        calls.declare_field(&q, &text_of(id, src), &al.resolve(&text_of(t, src)));
                    }
                }
            }
        }
        if let Some(m) = b.declare_method(INIT) {
            method_calls(b, calls, al, &m, pc, src);
        }
    }

    let mut cur = n.walk();
    for c in n.named_children(&mut cur) {
        match c.kind() {
            "class_body" => members(b, calls, al, c, src),
            "enum_class_body" => {
                let mut c2 = c.walk();
                for e in c.named_children(&mut c2) {
                    if e.kind() == "enum_entry" {
                        if let Some(body) = first_named(e, &["class_body"]) {
                            members(b, calls, al, body, src);
                        }
                    } else {
                        member(b, calls, al, e, src);
                    }
                }
            }
            _ => {}
        }
    }
    b.exit_type();
}

fn members(b: &mut Builder, calls: &mut Calls, al: &Aliases, body: Node, src: &[u8]) {
    let mut cur = body.walk();
    for m in body.named_children(&mut cur) {
        member(b, calls, al, m, src);
    }
}

fn member(b: &mut Builder, calls: &mut Calls, al: &Aliases, m: Node, src: &[u8]) {
    match m.kind() {
        "function_declaration" => function(b, calls, al, m, src),
        "secondary_constructor" | "anonymous_initializer" => {
            if let Some(q) = b.declare_method(INIT) {
                method_calls(b, calls, al, &q, m, src);
            }
        }
        "property_declaration" => {
            let Some(owner) = b.current_type().map(str::to_string) else {
                return;
            };
            if let Some((name, ty)) = declared_variable(m, src, al) {
                calls.declare_field(&owner, &name, &ty);
            }
        }
        _ => declaration(b, calls, al, m, src),
    }
}

fn function(b: &mut Builder, calls: &mut Calls, al: &Aliases, f: Node, src: &[u8]) {
    let Some(name) = f.child_by_field_name("name") else {
        return;
    };
    let name = text_of(name, src);
    if let Some(q) = b.declare_method(&name) {
        if let Some(t) = b.current_type() {
            calls.declare_method(t, &name);
        }
        method_calls(b, calls, al, &q, f, src);
    }
}

/// Nom et type d'une propriété ou variable locale : type écrit, sinon celui du constructeur
/// appelé par l'initialiseur (`val x = T()`), sinon vide.
fn declared_variable(n: Node, src: &[u8], al: &Aliases) -> Option<(String, String)> {
    let var = first_named(n, &["variable_declaration"])?;
    let id = first_named(var, &["identifier"])?;
    let ty = match type_child(var) {
        Some(t) => text_of(t, src),
        None => constructor_type(n, src).unwrap_or_default(),
    };
    let ty = al.resolve(&ty);
    Some((text_of(id, src), ty))
}

/// Nom du type construit par l'initialiseur `T(…)` d'une déclaration, si c'est un appel dont le
/// nom commence par une majuscule.
fn constructor_type(decl: Node, src: &[u8]) -> Option<String> {
    let mut cur = decl.walk();
    let init = decl
        .named_children(&mut cur)
        .find(|c| c.kind() == "call_expression")?;
    let callee = init.named_child(0)?;
    (callee.kind() == "identifier")
        .then(|| text_of(callee, src))
        .filter(|s| s.chars().next().is_some_and(char::is_uppercase))
}

/// Types locaux : paramètres et variables locales, sans distinguer les portées.
fn collect_locals(n: Node, src: &[u8], al: &Aliases, locals: &mut Locals) {
    let mut it = Preorder::new(n);
    while let Some(n) = it.next() {
        match n.kind() {
            "parameter" | "class_parameter" => {
                if let Some(id) = first_named(n, &["identifier"]) {
                    let ty = type_child(n).map(|t| text_of(t, src)).unwrap_or_default();
                    locals.declare(&text_of(id, src), &al.resolve(&ty));
                }
            }
            "property_declaration" => {
                if let Some((name, ty)) = declared_variable(n, src, al) {
                    locals.declare(&name, &ty);
                }
            }
            _ => {}
        }
    }
}

fn method_calls(
    b: &Builder,
    calls: &mut Calls,
    al: &Aliases,
    method: &str,
    decl: Node,
    src: &[u8],
) {
    let mut locals = Locals::new();
    collect_locals(decl, src, al, &mut locals);
    let types = b.type_stack().to_vec();
    walk_calls(decl, src, al, method, &types, &locals, calls);
}

fn walk_calls(
    n: Node,
    src: &[u8],
    al: &Aliases,
    method: &str,
    types: &[String],
    locals: &Locals,
    calls: &mut Calls,
) {
    let mut it = Preorder::new(n);
    while let Some(n) = it.next() {
        if n.kind() == "call_expression" {
            if let Some(callee) = n.named_child(0) {
                match callee.kind() {
                    // `T(…)` : construction, pas un appel de méthode.
                    "identifier" => {
                        let name = text_of(callee, src);
                        if !name.chars().next().is_some_and(char::is_uppercase) {
                            calls.record(method, types, locals, Receiver::Implicit, &name);
                        }
                    }
                    "navigation_expression" => {
                        let mut cur = callee.walk();
                        let kids: Vec<Node> = callee.named_children(&mut cur).collect();
                        if let (Some(recv), Some(name)) = (kids.first(), kids.last()) {
                            if kids.len() == 2 && name.kind() == "identifier" {
                                calls.record(
                                    method,
                                    types,
                                    locals,
                                    receiver_of(*recv, src, al),
                                    &text_of(*name, src),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn receiver_of(o: Node, src: &[u8], al: &Aliases) -> Receiver {
    match o.kind() {
        "this_expression" => Receiver::This,
        "identifier" => Receiver::Name(al.resolve(&text_of(o, src))),
        "navigation_expression" => {
            let mut cur = o.walk();
            let kids: Vec<Node> = o.named_children(&mut cur).collect();
            match kids.as_slice() {
                [t, f] if t.kind() == "this_expression" && f.kind() == "identifier" => {
                    Receiver::ThisField(text_of(*f, src))
                }
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

    fn run(name: &str, text: &str) -> FileStructure {
        extract(Path::new(name), text).unwrap()
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

    fn sub(s: &FileStructure, q: &str) -> Option<SubKind> {
        decl(s, NodeKind::Type, q).and_then(|d| d.subkind)
    }

    fn sample() -> FileStructure {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/langs/Sample.kt"),
        )
        .unwrap();
        run("Util.kt", &text)
    }

    #[test]
    fn req006_ac10_sous_genres() {
        let s = sample();
        let p = "com.acme.shop";
        assert!(decl(&s, NodeKind::Package, p).is_some());
        assert_eq!(sub(&s, &format!("{p}.Shape")), Some(SubKind::Class));
        assert_eq!(sub(&s, &format!("{p}.Circle")), Some(SubKind::Class));
        assert_eq!(sub(&s, &format!("{p}.Named")), Some(SubKind::Interface));
        assert_eq!(sub(&s, &format!("{p}.Color")), Some(SubKind::Enum));
        assert_eq!(sub(&s, &format!("{p}.Tag")), Some(SubKind::Annotation));
        assert_eq!(sub(&s, &format!("{p}.Registry")), Some(SubKind::Object));
        assert_eq!(
            sub(&s, &format!("{p}.Service.Companion")),
            Some(SubKind::Object)
        );
        assert!(decl(&s, NodeKind::Method, &format!("{p}.Service.<init>")).is_some());
        assert!(decl(&s, NodeKind::Method, &format!("{p}.Registry.all")).is_some());
        assert_eq!(sub(&s, &format!("{p}.UtilKt")), Some(SubKind::Class));
        assert!(decl(&s, NodeKind::Method, &format!("{p}.UtilKt.top")).is_some());
    }

    #[test]
    fn req006_ac10_supertypes_et_imports() {
        let s = sample();
        let svc = node_id(NodeKind::Type, "com.acme.shop.Service");
        assert!(has_ref(&s, &svc, EdgeKind::Extends, "Base"));
        assert!(!has_ref(&s, &svc, EdgeKind::Extends, "B"));
        assert!(has_ref(&s, &svc, EdgeKind::Implements, "Named"));
        let circle = node_id(NodeKind::Type, "com.acme.shop.Circle");
        assert!(has_ref(&s, &circle, EdgeKind::Extends, "Shape"));
        // Alias : la cible est le nom importé ; le joker ne donne pas de lien.
        assert!(has_ref(&s, &svc, EdgeKind::Imports, "com.acme.base.Base"));
        assert_eq!(
            s.references
                .iter()
                .filter(|r| r.kind == EdgeKind::Imports && r.source == svc)
                .count(),
            1
        );
        assert!(s.context.as_deref().unwrap().contains("import java.util.*"));
    }

    #[test]
    fn req006_ac10_appel_via_propriete_du_constructeur() {
        let s = sample();
        let run_m = node_id(NodeKind::Method, "com.acme.shop.Service.run");
        assert!(has_ref(&s, &run_m, EdgeKind::Calls, "Repo.save"));
        // `Circle(1.0)` est une construction, pas un appel.
        assert!(!s
            .references
            .iter()
            .any(|r| r.kind == EdgeKind::Calls && r.target.ends_with("Circle")));
    }

    #[test]
    fn req006_ac10_alias_dans_champs_locales_et_receveurs() {
        let s = run(
            "A.kt",
            "package p\nimport a.Repo as R\nimport a.Util as U\nclass S(private val repo: R) {\n    fun f(x: R?) {\n        repo.save()\n        x?.load()\n        val y = R()\n        y.drop()\n        U.help()\n    }\n}\n",
        );
        let m = node_id(NodeKind::Method, "p.S.f");
        assert!(has_ref(&s, &m, EdgeKind::Calls, "Repo.save"));
        assert!(has_ref(&s, &m, EdgeKind::Calls, "Repo.drop"));
        assert!(has_ref(&s, &m, EdgeKind::Calls, "Util.help"));
        assert!(!s.references.iter().any(|r| r.kind == EdgeKind::Calls
            && (r.target.starts_with("R.") || r.target.starts_with("U."))));
    }

    #[test]
    fn req006_ac10_service_repo_order_repository() {
        let s = run(
            "S.kt",
            "package p\nclass S(private val repo: OrderRepository) : Base(), I {\n    fun place(o: Order) {\n        repo.save(o)\n    }\n}\n",
        );
        let m = node_id(NodeKind::Method, "p.S.place");
        assert!(has_ref(&s, &m, EdgeKind::Calls, "OrderRepository.save"));
        let t = node_id(NodeKind::Type, "p.S");
        assert!(has_ref(&s, &t, EdgeKind::Extends, "Base"));
        assert!(has_ref(&s, &t, EdgeKind::Implements, "I"));
    }

    #[test]
    fn variable_locale_initialisee_par_constructeur() {
        let s = run(
            "A.kt",
            "class A {\n    fun f() {\n        val x = Foo()\n        x.go()\n        helper()\n        this.helper()\n    }\n    fun helper() {}\n}\n",
        );
        let m = node_id(NodeKind::Method, "A.f");
        assert!(has_ref(&s, &m, EdgeKind::Calls, "Foo.go"));
        assert!(has_ref(&s, &m, EdgeKind::Calls, "A.helper"));
    }

    #[test]
    fn script_kts_fonctions_rattachees_au_nom_du_fichier() {
        let s = run("tools/run.kts", "fun helper() = 1\n");
        assert!(decl(&s, NodeKind::Type, "run").is_some());
        assert!(decl(&s, NodeKind::Method, "run.helper").is_some());
    }

    #[test]
    fn kotlin_sans_package_et_nom_en_minuscule() {
        let s = run("util.kt", "fun f() {\n}\n");
        assert!(decl(&s, NodeKind::Method, "UtilKt.f").is_some());
    }

    #[test]
    fn corps_sur_une_ligne_tolere() {
        // Limite connue de la grammaire : nœud ERROR, ni échec ni panique.
        let s = run("R.kt", "object R { fun f() = 1 }\nclass Ok\n");
        assert!(decl(&s, NodeKind::Type, "Ok").is_some());
    }

    #[test]
    fn entree_vide_ou_invalide() {
        assert!(run("E.kt", "").declares.is_empty());
        let _ = run("E.kt", "class {{{ fun ((");
    }

    #[test]
    fn req044_ac2_concatenation_de_10000_termes_en_pile_reduite() {
        let terms: String = (1..10_000).map(|i| format!(" + \"s{i}\"")).collect();
        let src =
            format!("class K {{\n  fun helper() = \"\"\n  fun m(): String = helper(){terms}\n}}\n");
        let s = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || extract(Path::new("K.kt"), &src))
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        assert!(has_ref(&s, "method:K.m", EdgeKind::Calls, "K.helper"));
    }

    #[test]
    fn req104_ac3_imbrication_bornee_a_1000_niveaux() {
        use crate::parse::nesting_support::check;
        check(
            |t| extract(Path::new("Deep.kt"), t).unwrap(),
            |i| format!("class C{i} {{\n fun m() {{}}\n"),
            "}\n",
            "class B {\n fun m() {}\n}",
        );
    }
}
