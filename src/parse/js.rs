//! Extracteur JavaScript et TypeScript (REQ-034, DEC-039, DEC-040) : nœuds package, type et
//! méthode, héritage, imports et contexte de résolution. Les appels et le JSX viennent ensuite.

use std::collections::HashSet;
use std::path::Path;

use tree_sitter::Node;

use crate::error::Result;
use crate::model::{node_id, EdgeKind, NodeKind, SubKind};

use super::js_imports::Imports;
use super::jvm::INIT;
use super::{Declared, FileStructure, Lang, Reference};

/// Nom du module ES d'un fichier : (qualifié, court). Le qualifié est le chemin relatif sans
/// extension (`.d.ts`, `.d.mts`, `.d.cts` retirés en entier, sinon la dernière extension) ; le
/// court est le nom du fichier sans extension.
pub fn module_name(path: &Path) -> (String, String) {
    let rel = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let (dir, file) = match rel.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, rel.as_str()),
    };
    let stem = [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .find_map(|s| file.strip_suffix(s))
        .or_else(|| file.rsplit_once('.').map(|(s, _)| s))
        .filter(|s| !s.is_empty())
        .unwrap_or(file);
    let qualified = match dir {
        Some(d) => format!("{d}/{stem}"),
        None => stem.to_string(),
    };
    (qualified, stem.to_string())
}

/// Extrait la structure d'un fichier JavaScript ou TypeScript : déclarations de premier niveau,
/// membres de classe et d'interface, héritage. Analyse tolérante aux erreurs.
pub fn extract(lang: Lang, path: &Path, text: &str) -> Result<FileStructure> {
    let (module, short) = module_name(path);
    let rel = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let tree = lang.parse(text)?;
    let mut x = Extractor {
        src: text.as_bytes(),
        declares: vec![Declared {
            kind: NodeKind::Package,
            qualified: module.clone(),
            subkind: None,
            summary: None,
            short: Some(short),
        }],
        references: Vec::new(),
        seen: HashSet::new(),
        imports: Imports::collect(tree.root_node(), text.as_bytes(), &rel),
    };
    let package = node_id(NodeKind::Package, &module);
    x.seen.insert(package.clone());
    for name in x.imports.import_targets() {
        x.add_ref(&package, EdgeKind::Imports, name);
    }
    x.statements(tree.root_node(), &module);
    Ok(FileStructure {
        context: Some(x.imports.context(&module)),
        declares: x.declares,
        references: x.references,
        warnings: Vec::new(),
    })
}

struct Extractor<'a> {
    src: &'a [u8],
    declares: Vec<Declared>,
    references: Vec<Reference>,
    seen: HashSet<String>,
    imports: Imports,
}

impl Extractor<'_> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.src[n.byte_range()]).into_owned()
    }

    fn declare(&mut self, kind: NodeKind, qualified: String, subkind: Option<SubKind>) {
        if self.seen.insert(node_id(kind, &qualified)) {
            self.declares.push(Declared {
                kind,
                qualified,
                subkind,
                summary: None,
                short: None,
            });
        }
    }

    fn add_ref(&mut self, source: &str, kind: EdgeKind, target: String) {
        if target.is_empty() {
            return;
        }
        let r = Reference {
            source: source.to_string(),
            kind,
            target,
        };
        if !self.references.contains(&r) {
            self.references.push(r);
        }
    }

    /// Instructions d'un `program` ou du corps d'un namespace, déclarées sous `owner`.
    fn statements(&mut self, parent: Node, owner: &str) {
        let mut cur = parent.walk();
        for n in parent.named_children(&mut cur) {
            self.statement(n, owner);
        }
    }

    fn statement(&mut self, n: Node, owner: &str) {
        match n.kind() {
            "export_statement" => {
                let mut cur = n.walk();
                for c in n.named_children(&mut cur) {
                    if matches!(c.kind(), "decorator" | "comment") {
                        continue;
                    }
                    self.statement(c, owner);
                }
            }
            "ambient_declaration" => self.statements(n, owner),
            // `namespace N {}` au premier niveau est enveloppé dans une instruction-expression.
            "expression_statement" => {
                let mut cur = n.walk();
                for c in n.named_children(&mut cur) {
                    if matches!(c.kind(), "internal_module" | "module") {
                        self.namespace(c, owner);
                    }
                }
            }
            // Reprise sur erreur : les déclarations qui suivent un morceau illisible comptent.
            "ERROR" => self.statements(n, owner),
            "class_declaration" | "abstract_class_declaration" | "class" => {
                let name = self.name_of(n).unwrap_or_else(|| "default".to_string());
                self.type_decl(n, owner, &name, SubKind::Class);
            }
            "interface_declaration" => {
                if let Some(name) = self.name_of(n) {
                    self.type_decl(n, owner, &name, SubKind::Interface);
                }
            }
            "enum_declaration" => {
                if let Some(name) = self.name_of(n) {
                    self.type_decl(n, owner, &name, SubKind::Enum);
                }
            }
            "type_alias_declaration" => {
                if let Some(name) = self.name_of(n) {
                    self.type_decl(n, owner, &name, SubKind::Alias);
                }
            }
            "internal_module" | "module" => self.namespace(n, owner),
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                if let Some(name) = self.name_of(n) {
                    self.declare(NodeKind::Method, format!("{owner}.{name}"), None);
                }
            }
            // `export default function () {}` : fonction anonyme.
            "function_expression" | "function" | "generator_function" => {
                let name = self.name_of(n).unwrap_or_else(|| "default".to_string());
                self.declare(NodeKind::Method, format!("{owner}.{name}"), None);
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut cur = n.walk();
                for d in n.named_children(&mut cur) {
                    if d.kind() != "variable_declarator" {
                        continue;
                    }
                    let (Some(name), Some(value)) = (
                        d.child_by_field_name("name"),
                        d.child_by_field_name("value"),
                    ) else {
                        continue;
                    };
                    if name.kind() == "identifier" && is_function_value(value) {
                        let name = self.text(name);
                        self.declare(NodeKind::Method, format!("{owner}.{name}"), None);
                    }
                }
            }
            _ => {}
        }
    }

    /// Nom simple porté par le champ `name` (identifiant), sinon `None`.
    fn name_of(&self, n: Node) -> Option<String> {
        let name = n.child_by_field_name("name")?;
        matches!(
            name.kind(),
            "identifier" | "type_identifier" | "property_identifier"
        )
        .then(|| self.text(name))
    }

    fn namespace(&mut self, n: Node, owner: &str) {
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        // `namespace A.B`, `declare module 'x'` : ni nœud, ni contenu.
        if name.kind() != "identifier" {
            return;
        }
        let q = format!("{owner}.{}", self.text(name));
        self.declare(NodeKind::Type, q.clone(), Some(SubKind::Object));
        if let Some(body) = n.child_by_field_name("body") {
            self.statements(body, &q);
        }
    }

    fn type_decl(&mut self, n: Node, owner: &str, name: &str, sub: SubKind) {
        let q = format!("{owner}.{name}");
        self.declare(NodeKind::Type, q.clone(), Some(sub));
        let id = node_id(NodeKind::Type, &q);
        let mut cur = n.walk();
        for c in n.named_children(&mut cur) {
            match c.kind() {
                "class_heritage" => self.heritage(c, &id),
                "extends_type_clause" => self.heritage(c, &id),
                _ => {}
            }
        }
        if let Some(body) = n.child_by_field_name("body") {
            if matches!(sub, SubKind::Class | SubKind::Interface) {
                self.members(body, &q);
            }
        }
    }

    /// `extends` / `implements` sous un `class_heritage` ou un `extends_type_clause`.
    fn heritage(&mut self, n: Node, id: &str) {
        let mut cur = n.walk();
        for c in n.named_children(&mut cur) {
            match c.kind() {
                "extends_clause" => self.heritage_names(c, id, EdgeKind::Extends),
                "implements_clause" => self.heritage_names(c, id, EdgeKind::Implements),
                "comment" | "type_arguments" => {}
                // Grammaire JavaScript : `extends <expression>` directement.
                _ => self.heritage_name(c, id, EdgeKind::Extends),
            }
        }
    }

    fn heritage_names(&mut self, clause: Node, id: &str, kind: EdgeKind) {
        let mut cur = clause.walk();
        for c in clause.named_children(&mut cur) {
            if matches!(c.kind(), "comment" | "type_arguments") {
                continue;
            }
            self.heritage_name(c, id, kind);
        }
    }

    fn heritage_name(&mut self, n: Node, id: &str, kind: EdgeKind) {
        if let Some(name) = self.simple_name(n).and_then(|nm| self.imports.rewrite(&nm)) {
            self.add_ref(id, kind, name);
        }
    }

    /// Nom (`B` ou `ns.B`) d'une expression ou d'un type ; génériques retirés ; `None` pour toute
    /// autre expression (appel, etc.).
    fn simple_name(&self, n: Node) -> Option<String> {
        match n.kind() {
            "identifier" | "type_identifier" => Some(self.text(n)),
            "generic_type" => self.simple_name(n.child_by_field_name("name")?),
            "member_expression" | "nested_type_identifier" | "nested_identifier" => {
                let t: String = self.text(n).split_whitespace().collect();
                let ok = !t.is_empty()
                    && t.split('.').all(|p| {
                        !p.is_empty()
                            && p.chars()
                                .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
                    });
                ok.then_some(t)
            }
            _ => None,
        }
    }

    /// Membres d'un corps de classe ou d'interface, sous le type `owner`.
    fn members(&mut self, body: Node, owner: &str) {
        let mut cur = body.walk();
        for m in body.named_children(&mut cur) {
            let name = match m.kind() {
                "method_definition" | "method_signature" | "abstract_method_signature" => {
                    m.child_by_field_name("name")
                }
                "public_field_definition" | "field_definition" => {
                    let v = m.child_by_field_name("value");
                    if !v.is_some_and(is_function_value) {
                        continue;
                    }
                    m.child_by_field_name("name")
                        .or_else(|| m.child_by_field_name("property"))
                }
                _ => continue,
            };
            let Some(name) = name.and_then(|n| self.member_name(n)) else {
                continue;
            };
            self.declare(NodeKind::Method, format!("{owner}.{name}"), None);
        }
    }

    fn member_name(&self, n: Node) -> Option<String> {
        let t = self.text(n);
        match n.kind() {
            "property_identifier" if t == "constructor" => Some(INIT.to_string()),
            "property_identifier" | "number" => Some(t),
            "private_property_identifier" => Some(t.trim_start_matches('#').to_string()),
            "string" => {
                let s = t.trim_matches(|c| c == '"' || c == '\'' || c == '`');
                (!s.is_empty()).then(|| s.to_string())
            }
            _ => None,
        }
    }
}

fn is_function_value(n: Node) -> bool {
    matches!(
        n.kind(),
        "arrow_function" | "function_expression" | "function" | "generator_function"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str) -> (String, String) {
        module_name(Path::new(p))
    }

    #[test]
    fn module_name_cas_usuels() {
        assert_eq!(m("src/a.ts"), ("src/a".into(), "a".into()));
        assert_eq!(m("src/a.d.ts"), ("src/a".into(), "a".into()));
        assert_eq!(m("src/a.d.mts"), ("src/a".into(), "a".into()));
        assert_eq!(m("src/a.d.cts"), ("src/a".into(), "a".into()));
        assert_eq!(
            m("web/src/order.service.ts"),
            ("web/src/order.service".into(), "order.service".into())
        );
        assert_eq!(m("main.ts"), ("main".into(), "main".into()));
        assert_eq!(m("src/b.jsx"), ("src/b".into(), "b".into()));
    }

    #[test]
    fn extract_rend_le_noeud_package() {
        let s = extract(Lang::TypeScript, Path::new("src/a.d.ts"), "").unwrap();
        assert_eq!(s.declares.len(), 1);
        assert_eq!(s.declares[0].id(), "package:src/a");
        assert_eq!(s.declares[0].short.as_deref(), Some("a"));
        assert_eq!(s.context.as_deref(), Some("package src/a"));
        assert!(s.references.is_empty());
    }

    fn ex(lang: Lang, path: &str, text: &str) -> FileStructure {
        extract(lang, Path::new(path), text).unwrap()
    }

    fn ids(s: &FileStructure) -> Vec<String> {
        s.declares.iter().map(|d| d.id()).collect()
    }

    fn refs(s: &FileStructure) -> Vec<String> {
        s.references
            .iter()
            .map(|r| format!("{} {} {}", r.source, r.kind.as_str(), r.target))
            .collect()
    }

    fn sub(s: &FileStructure, id: &str) -> Option<SubKind> {
        s.declares.iter().find(|d| d.id() == id).unwrap().subkind
    }

    #[test]
    fn req034_ac2_declarations_typescript() {
        let src = "export interface A extends B, C { m(): void }\n\
            enum E { X }\nconst enum F { Y }\nexport type Props = { id: string };\n\
            abstract class S implements A { abstract m(): void; static of() {} get size() { return 0 } #secret() {} handle = () => this.of2(); of2() {} }\n\
            function f(a: string): void;\nfunction f(a: number): void;\nfunction f(a: any) {}\n\
            namespace N { export function g() {} export class K {} }\n";
        let s = ex(Lang::TypeScript, "m.ts", src);
        let got = ids(&s);
        for want in [
            "type:m.A",
            "type:m.E",
            "type:m.F",
            "type:m.Props",
            "type:m.S",
            "type:m.N",
            "method:m.A.m",
            "method:m.S.m",
            "method:m.S.of",
            "method:m.S.size",
            "method:m.S.secret",
            "method:m.S.handle",
            "method:m.S.of2",
            "method:m.N.g",
            "type:m.N.K",
            "method:m.f",
        ] {
            assert!(
                got.contains(&want.to_string()),
                "manque {want} dans {got:?}"
            );
        }
        assert_eq!(got.iter().filter(|i| *i == "method:m.f").count(), 1);
        assert_eq!(sub(&s, "type:m.A"), Some(SubKind::Interface));
        assert_eq!(sub(&s, "type:m.E"), Some(SubKind::Enum));
        assert_eq!(sub(&s, "type:m.F"), Some(SubKind::Enum));
        assert_eq!(sub(&s, "type:m.Props"), Some(SubKind::Alias));
        assert_eq!(sub(&s, "type:m.S"), Some(SubKind::Class));
        assert_eq!(sub(&s, "type:m.N"), Some(SubKind::Object));
        let r = refs(&s);
        assert!(r.contains(&"type:m.A extends B".to_string()), "{r:?}");
        assert!(r.contains(&"type:m.A extends C".to_string()));
        assert!(r.contains(&"type:m.S implements A".to_string()));
    }

    #[test]
    fn req034_ac3_fonctions_de_premier_niveau() {
        let src = "function helper() {}\nconst slug = (s: string) => s;\n\
            export function formatDate(d: Date) { helper(); [1].map(() => helper()); function inner() {} }\nhelper();\n\
            const obj = { m() {} };\nlet n = 1;\nconst fe = function () {};\n";
        let s = ex(Lang::TypeScript, "src/util/format.ts", src);
        let got = ids(&s);
        assert_eq!(
            got,
            vec![
                "package:src/util/format",
                "method:src/util/format.helper",
                "method:src/util/format.slug",
                "method:src/util/format.formatDate",
                "method:src/util/format.fe",
            ]
        );
        assert!(s.references.is_empty());
    }

    #[test]
    fn req034_ac4_commonjs_et_defaut_anonyme() {
        let cjs = "const { formatDate: fd } = require('./util/format');\nfunction start() { fd(); }\nmodule.exports = { start };\nexports.x = function () {};\n";
        let s = ex(Lang::JavaScript, "src/server.cjs", cjs);
        assert_eq!(
            ids(&s),
            vec!["package:src/server", "method:src/server.start"]
        );
        let s = ex(
            Lang::JavaScript,
            "src/tool.mjs",
            "export default function () { }\n",
        );
        assert!(ids(&s).contains(&"method:src/tool.default".to_string()));
        let s = ex(Lang::JavaScript, "src/old.js", "function a() {}\nclass B { constructor() {} m() {} static s() {} #p() {} get g() {} }\n");
        let got = ids(&s);
        for want in [
            "method:src/old.a",
            "type:src/old.B",
            "method:src/old.B.<init>",
            "method:src/old.B.m",
            "method:src/old.B.s",
            "method:src/old.B.p",
            "method:src/old.B.g",
        ] {
            assert!(
                got.contains(&want.to_string()),
                "manque {want} dans {got:?}"
            );
        }
    }

    #[test]
    fn req034_export_default_class_anonyme() {
        let s = ex(Lang::TypeScript, "a.ts", "export default class {}\n");
        assert!(ids(&s).contains(&"type:a.default".to_string()));
        let s = ex(Lang::JavaScript, "a.js", "export default class {}\n");
        assert!(ids(&s).contains(&"type:a.default".to_string()));
        assert_eq!(sub(&s, "type:a.default"), Some(SubKind::Class));
    }

    #[test]
    fn req034_fichier_d_declarations() {
        let src = "declare class D { m(): void }\ndeclare function df(x: number): string;\ndeclare namespace NS { function g(): void; namespace In { class Z {} } }\n";
        let s = ex(Lang::TypeScript, "types/x.d.ts", src);
        let got = ids(&s);
        for want in [
            "package:types/x",
            "type:types/x.D",
            "method:types/x.D.m",
            "method:types/x.df",
            "type:types/x.NS",
            "method:types/x.NS.g",
            "type:types/x.NS.In",
            "type:types/x.NS.In.Z",
        ] {
            assert!(
                got.contains(&want.to_string()),
                "manque {want} dans {got:?}"
            );
        }
    }

    #[test]
    fn req034_heritage_formes() {
        let src = "class A extends ns.B implements I, J<T> {}\nclass C extends mixin(B) {}\nclass D extends Base<string> {}\ninterface K<T> extends L<T>, M {}\n";
        let s = ex(Lang::TypeScript, "h.ts", src);
        let r = refs(&s);
        assert_eq!(
            r,
            vec![
                "type:h.A extends ns.B",
                "type:h.A implements I",
                "type:h.A implements J",
                "type:h.D extends Base",
                "type:h.K extends L",
                "type:h.K extends M",
            ]
        );
        let s = ex(
            Lang::JavaScript,
            "h.js",
            "class A extends B {}\nclass C extends ns.D {}\nclass E extends f(B) {}\n",
        );
        assert_eq!(
            refs(&s),
            vec!["type:h.A extends B", "type:h.C extends ns.D"]
        );
    }

    #[test]
    fn req034_heritage_espace_de_noms_et_externe() {
        let src = "import * as ns from './base';\nimport React from 'react';\nimport { Base } from 'lib';\n\
                   class A extends ns.B {}\nclass P extends React.Component {}\nclass Q extends Base {}\n";
        let s = ex(Lang::TypeScript, "src/h.ts", src);
        let r = refs(&s);
        assert!(r.contains(&"type:src/h.A extends B".to_string()), "{r:?}");
        assert!(!r
            .iter()
            .any(|x| x.contains("React") || x.contains("Component")));
        assert!(!r.iter().any(|x| x.contains("Q extends")), "{r:?}");
        let ctx = s.context.unwrap();
        assert!(ctx.contains("import src/base.B"), "{ctx}");
        assert!(!ctx.contains("React"), "{ctx}");
    }

    #[test]
    fn req034_cas_negatifs() {
        let src = "function outer() { class Inner {} function nested() {} }\n\
            const o = { m() {}, n: () => 1 };\nmodule.exports = { a };\nclass Z { x = 1; y: string; }\n\
            declare module 'x' { class Q {} function qf(): void; }\nnamespace A.B { class R {} }\n";
        let s = ex(Lang::TypeScript, "n.ts", src);
        assert_eq!(ids(&s), vec!["package:n", "method:n.outer", "type:n.Z"]);
    }

    #[test]
    fn req034_namespaces_imbriques() {
        let s = ex(Lang::TypeScript, "n.ts", "namespace A { export namespace B { export function f() {} export namespace C { export class K { m() {} } } } }\n");
        let got = ids(&s);
        for want in [
            "type:n.A",
            "type:n.A.B",
            "method:n.A.B.f",
            "type:n.A.B.C",
            "type:n.A.B.C.K",
            "method:n.A.B.C.K.m",
        ] {
            assert!(
                got.contains(&want.to_string()),
                "manque {want} dans {got:?}"
            );
        }
    }

    #[test]
    fn req034_decorateurs_ignores() {
        let s = ex(Lang::TypeScript, "d.ts", "@Component({a: 1})\nexport class C { @Input() x: string; @HostListener('a') on() {} constructor(private repo: R) {} }\n");
        let got = ids(&s);
        assert!(got.contains(&"type:d.C".to_string()), "{got:?}");
        assert!(got.contains(&"method:d.C.on".to_string()));
        assert!(got.contains(&"method:d.C.<init>".to_string()));
        assert_eq!(got.len(), 4, "{got:?}");
    }

    #[test]
    fn req034_ac9_fichiers_mal_formes() {
        let s = ex(
            Lang::TypeScript,
            "Broken.ts",
            "export function ok() {}\nexport class Good {}\nclass Bad { m() { \n",
        );
        let got = ids(&s);
        assert!(got.contains(&"method:Broken.ok".to_string()), "{got:?}");
        assert!(got.contains(&"type:Broken.Good".to_string()));
        let s = ex(Lang::JavaScript, "broken.jsx", "export function ok() { return <div/>; }\nfunction two() {}\nfunction bad() { if (x) { \n");
        let got = ids(&s);
        assert!(got.contains(&"method:broken.ok".to_string()), "{got:?}");
        assert!(got.contains(&"method:broken.two".to_string()));
    }

    #[test]
    fn req034_fonctions_generateur_et_var() {
        let s = ex(
            Lang::JavaScript,
            "g.js",
            "function* gen() {}\nvar v = function () {};\nclass B { h = function () {}; k = () => 1; n = 2 }\n",
        );
        let got = ids(&s);
        for want in ["method:g.gen", "method:g.v", "method:g.B.h", "method:g.B.k"] {
            assert!(
                got.contains(&want.to_string()),
                "manque {want} dans {got:?}"
            );
        }
        assert!(!got.contains(&"method:g.B.n".to_string()), "{got:?}");
    }

    #[test]
    fn req034_tsx_classe_et_fonction_jsx() {
        let src = "export class Page extends Base { render() { return <div/>; } }\n\
            export function Card() { return <span>x</span>; }\nconst Btn = () => <b/>;\n";
        let s = ex(Lang::Tsx, "ui/p.tsx", src);
        let got = ids(&s);
        for want in [
            "type:ui/p.Page",
            "method:ui/p.Page.render",
            "method:ui/p.Card",
            "method:ui/p.Btn",
        ] {
            assert!(
                got.contains(&want.to_string()),
                "manque {want} dans {got:?}"
            );
        }
        assert_eq!(refs(&s), vec!["type:ui/p.Page extends Base"]);
    }

    #[test]
    fn req034_ac4_require_sans_declaration() {
        let s = ex(
            Lang::JavaScript,
            "src/u.js",
            "const util = require('./util/format');\n",
        );
        assert_eq!(ids(&s), vec!["package:src/u"]);
        assert!(s.references.is_empty());
    }
}
