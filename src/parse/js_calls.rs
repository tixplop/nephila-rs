//! Appels et JSX JavaScript et TypeScript (REQ-034, DEC-021 adapté, DEC-040) : références
//! `calls` de l'hypothèse « références ». Un receveur de type inconnu ne produit rien.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::model::{NodeKind, SubKind};

use super::js::{is_function_value, member_name};
use super::js_imports::{Cible, Imports};
use super::{Declared, OrderedSet, MAX_NESTING};

/// Table locale : nom lié -> type annoté ou construit (`None` = type inconnu).
type Locals = HashMap<String, Option<String>>;

struct Class {
    q: String,
    fields: Locals,
}

struct Collector<'a> {
    src: &'a [u8],
    imports: &'a Imports,
    types: HashMap<String, SubKind>,
    methods: HashSet<String>,
    out: OrderedSet<(String, String)>,
    /// `namespace` et nœuds `ERROR` englobants ; au-delà de `MAX_NESTING`, on ne descend plus
    /// (même borne que l'extracteur de structure, DEC-105).
    level: usize,
}

/// Appels d'un fichier : couples (nom qualifié de la méthode appelante, nom visé).
pub fn collect(
    root: Node,
    src: &[u8],
    module: &str,
    imports: &Imports,
    declared: &[Declared],
) -> Vec<(String, String)> {
    let mut types = HashMap::new();
    let mut methods = HashSet::new();
    for d in declared {
        match d.kind {
            NodeKind::Type => {
                if let Some(s) = d.subkind {
                    types.insert(d.qualified.clone(), s);
                }
            }
            NodeKind::Method => {
                methods.insert(d.qualified.clone());
            }
            _ => {}
        }
    }
    let mut c = Collector {
        src,
        imports,
        types,
        methods,
        out: OrderedSet::new(),
        level: 0,
    };
    let mut owners = vec![module.to_string()];
    c.statements(root, &mut owners);
    c.out.into_vec()
}

fn is_class_kind(k: &str) -> bool {
    matches!(
        k,
        "class_declaration" | "abstract_class_declaration" | "class"
    )
}

/// Fonction qui rebinde `this` (ou classe) : `this` n'y désigne plus la classe englobante.
fn rebinds_this(k: &str) -> bool {
    is_class_kind(k)
        || matches!(
            k,
            "function_expression"
                | "function"
                | "generator_function"
                | "function_declaration"
                | "generator_function_declaration"
                | "method_definition"
        )
}

impl Collector<'_> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.src[n.byte_range()]).into_owned()
    }

    fn name_of(&self, n: Node) -> Option<String> {
        let name = n.child_by_field_name("name")?;
        matches!(
            name.kind(),
            "identifier" | "type_identifier" | "property_identifier"
        )
        .then(|| self.text(name))
    }

    // ---- parcours des déclarations (même forme que l'extracteur de structure) ----

    fn statements(&mut self, parent: Node, owners: &mut Vec<String>) {
        let mut cur = parent.walk();
        for n in parent.named_children(&mut cur) {
            self.statement(n, owners);
        }
    }

    fn statement(&mut self, n: Node, owners: &mut Vec<String>) {
        match n.kind() {
            "export_statement" => {
                let mut cur = n.walk();
                for c in n.named_children(&mut cur) {
                    if !matches!(c.kind(), "decorator" | "comment") {
                        self.statement(c, owners);
                    }
                }
            }
            "ambient_declaration" => self.statements(n, owners),
            "ERROR" => {
                if self.level < MAX_NESTING {
                    self.level += 1;
                    self.statements(n, owners);
                    self.level -= 1;
                }
            }
            "expression_statement" => {
                let mut cur = n.walk();
                for c in n.named_children(&mut cur) {
                    if matches!(c.kind(), "internal_module" | "module") {
                        self.namespace(c, owners);
                    }
                }
            }
            "internal_module" | "module" => self.namespace(n, owners),
            k if is_class_kind(k) => {
                let name = self.name_of(n).unwrap_or_else(|| "default".to_string());
                self.class(n, &name, owners);
            }
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = self.name_of(n) {
                    let caller = format!("{}.{name}", owners.last().unwrap());
                    self.scan(n, &caller, owners, None);
                }
            }
            "function_expression" | "function" | "generator_function" => {
                let name = self.name_of(n).unwrap_or_else(|| "default".to_string());
                let caller = format!("{}.{name}", owners.last().unwrap());
                self.scan(n, &caller, owners, None);
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
                        let caller = format!("{}.{}", owners.last().unwrap(), self.text(name));
                        self.scan(value, &caller, owners, None);
                    }
                }
            }
            _ => {}
        }
    }

    fn namespace(&mut self, n: Node, owners: &mut Vec<String>) {
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        if name.kind() != "identifier" {
            return;
        }
        let q = format!("{}.{}", owners.last().unwrap(), self.text(name));
        if self.level >= MAX_NESTING {
            return;
        }
        if let Some(body) = n.child_by_field_name("body") {
            owners.push(q);
            self.level += 1;
            self.statements(body, owners);
            self.level -= 1;
            owners.pop();
        }
    }

    fn class(&mut self, n: Node, name: &str, owners: &[String]) {
        let q = format!("{}.{name}", owners.last().unwrap());
        let Some(body) = n.child_by_field_name("body") else {
            return;
        };
        let class = Class {
            fields: self.class_fields(body),
            q: q.clone(),
        };
        let mut cur = body.walk();
        for m in body.named_children(&mut cur) {
            let (target, name) = match m.kind() {
                "method_definition" => (Some(m), m.child_by_field_name("name")),
                "public_field_definition" | "field_definition" => {
                    let v = m
                        .child_by_field_name("value")
                        .filter(|v| is_function_value(*v));
                    (
                        v,
                        m.child_by_field_name("name")
                            .or_else(|| m.child_by_field_name("property")),
                    )
                }
                _ => continue,
            };
            let (Some(target), Some(name)) = (target, name) else {
                continue;
            };
            let Some(name) = member_name(self.src, name) else {
                continue;
            };
            let caller = format!("{q}.{name}");
            self.scan(target, &caller, owners, Some(&class));
        }
    }

    /// Champs de la classe : champs typés ou initialisés par `new T()`, et paramètres-propriétés
    /// du constructeur.
    fn class_fields(&self, body: Node) -> Locals {
        let mut fields = Locals::new();
        let mut cur = body.walk();
        for m in body.named_children(&mut cur) {
            match m.kind() {
                "public_field_definition" | "field_definition" => {
                    let Some(name) = m
                        .child_by_field_name("name")
                        .or_else(|| m.child_by_field_name("property"))
                        .filter(|n| {
                            matches!(
                                n.kind(),
                                "property_identifier" | "private_property_identifier"
                            )
                        })
                    else {
                        continue;
                    };
                    let ty = self.declared_type(m.child_by_field_name("type"), m);
                    bind(&mut fields, self.text(name).trim_start_matches('#'), ty);
                }
                "method_definition" => {
                    let is_ctor = m
                        .child_by_field_name("name")
                        .is_some_and(|n| self.text(n) == "constructor");
                    let Some(params) = m.child_by_field_name("parameters").filter(|_| is_ctor)
                    else {
                        continue;
                    };
                    let mut c2 = params.walk();
                    for p in params.named_children(&mut c2) {
                        if !matches!(p.kind(), "required_parameter" | "optional_parameter") {
                            continue;
                        }
                        let mut c3 = p.walk();
                        let prop = p.children(&mut c3).any(|c| {
                            matches!(
                                c.kind(),
                                "accessibility_modifier" | "readonly" | "override_modifier"
                            )
                        });
                        let pat = p.child_by_field_name("pattern");
                        if let (true, Some(pat)) = (prop, pat.filter(|p| p.kind() == "identifier"))
                        {
                            let ty = self.declared_type(p.child_by_field_name("type"), p);
                            bind(&mut fields, &self.text(pat), ty);
                        }
                    }
                }
                _ => {}
            }
        }
        fields
    }

    // ---- types et tables locales ----

    /// Type annoté, sinon type construit par `new T(…)` dans la valeur de `holder`.
    fn declared_type(&self, annotation: Option<Node>, holder: Node) -> Option<String> {
        if let Some(a) = annotation {
            return self.type_text(a);
        }
        let v = holder.child_by_field_name("value")?;
        if v.kind() != "new_expression" {
            return None;
        }
        let ctor = v.child_by_field_name("constructor")?;
        chain(self, ctor).map(|s| s.join("."))
    }

    fn type_text(&self, n: Node) -> Option<String> {
        match n.kind() {
            "type_annotation" => self.type_text(n.named_child(0)?),
            "type_identifier" => Some(self.text(n)),
            "generic_type" => self.type_text(n.child_by_field_name("name")?),
            "nested_type_identifier" => {
                let t: String = self.text(n).split_whitespace().collect();
                Some(t)
            }
            _ => None,
        }
    }

    /// Noms liés dans une fonction (paramètres, variables, fonctions et classes imbriquées),
    /// à plat : un nom lié n'importe où dans la fonction masque l'extérieur.
    fn bindings(&self, f: Node) -> Locals {
        let mut locals = Locals::new();
        let mut stack: Vec<Node> = ["parameters", "parameter", "body"]
            .iter()
            .filter_map(|k| f.child_by_field_name(k))
            .collect();
        while let Some(n) = stack.pop() {
            match n.kind() {
                "variable_declarator" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        let ty = self.declared_type(n.child_by_field_name("type"), n);
                        self.bind_pattern(&mut locals, name, ty);
                    }
                }
                "required_parameter" | "optional_parameter" => {
                    if let Some(p) = n.child_by_field_name("pattern") {
                        let ty = self.declared_type(n.child_by_field_name("type"), n);
                        self.bind_pattern(&mut locals, p, ty);
                    }
                }
                "formal_parameters" => {
                    let mut cur = n.walk();
                    for p in n.named_children(&mut cur) {
                        if !matches!(
                            p.kind(),
                            "required_parameter" | "optional_parameter" | "comment"
                        ) {
                            self.bind_pattern(&mut locals, p, None);
                        }
                    }
                }
                "arrow_function" => {
                    if let Some(p) = n.child_by_field_name("parameter") {
                        self.bind_pattern(&mut locals, p, None);
                    }
                }
                "catch_clause" => {
                    if let Some(p) = n.child_by_field_name("parameter") {
                        self.bind_pattern(&mut locals, p, None);
                    }
                }
                "for_in_statement" => {
                    if let Some(l) = n.child_by_field_name("left") {
                        self.bind_pattern(&mut locals, l, None);
                    }
                }
                "function_declaration"
                | "generator_function_declaration"
                | "class_declaration"
                | "abstract_class_declaration" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        self.bind_pattern(&mut locals, name, None);
                    }
                }
                _ => {}
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        locals
    }

    fn bind_pattern(&self, locals: &mut Locals, p: Node, ty: Option<String>) {
        let mut stack = vec![(p, ty)];
        while let Some((n, ty)) = stack.pop() {
            match n.kind() {
                "identifier" | "type_identifier" | "shorthand_property_identifier_pattern" => {
                    bind(locals, &self.text(n), ty)
                }
                "assignment_pattern" => {
                    if let Some(l) = n.child_by_field_name("left") {
                        stack.push((l, ty));
                    }
                }
                "pair_pattern" => {
                    if let Some(v) = n.child_by_field_name("value") {
                        stack.push((v, None));
                    }
                }
                "object_pattern"
                | "array_pattern"
                | "rest_pattern"
                | "object_assignment_pattern" => {
                    let mut cur = n.walk();
                    for c in n.named_children(&mut cur) {
                        stack.push((c, None));
                    }
                }
                _ => {}
            }
        }
    }

    // ---- portée ----

    fn lookup_type(&self, owners: &[String], name: &str) -> Option<(String, SubKind)> {
        owners.iter().rev().find_map(|o| {
            let q = format!("{o}.{name}");
            self.types.get(&q).map(|s| (q, *s))
        })
    }

    fn lookup_method(&self, owners: &[String], name: &str) -> Option<String> {
        owners.iter().rev().find_map(|o| {
            let q = format!("{o}.{name}");
            self.methods.contains(&q).then_some(q)
        })
    }

    /// Nom visé par un type écrit `t` (`T` ou `ns.T`) : déclaré dans le fichier, ou importé.
    fn resolve_type(&self, owners: &[String], t: &str) -> Option<String> {
        if let Some((q, sub)) = self.lookup_type(owners, t) {
            return matches!(sub, SubKind::Class | SubKind::Interface | SubKind::Object)
                .then_some(q);
        }
        let first = t.split('.').next().unwrap_or(t);
        self.imports.entries.iter().find(|e| e.local == first)?;
        self.imports.rewrite(t)
    }

    /// Appel `f()` ou `<F/>` d'un nom simple non lié localement.
    fn resolve_plain(&self, owners: &[String], name: &str) -> Option<String> {
        if let Some(q) = self.lookup_method(owners, name) {
            return Some(q);
        }
        if self.lookup_type(owners, name).is_some() {
            return None;
        }
        let e = self.imports.entries.iter().find(|e| e.local == name)?;
        if e.namespace || !matches!(e.cible, Cible::Modules(_) | Cible::Alias) {
            return None;
        }
        self.imports.rewrite(name)
    }

    /// Appel `a.b.m()` dont le receveur est une chaîne de noms.
    fn resolve_chain(
        &self,
        owners: &[String],
        locals: &Locals,
        segs: &[String],
        typed: bool,
    ) -> Option<String> {
        let first = &segs[0];
        if locals.contains_key(first) {
            if !typed || segs.len() != 2 {
                return None;
            }
            let ty = locals[first].as_ref()?;
            return Some(format!("{}.{}", self.resolve_type(owners, ty)?, segs[1]));
        }
        let full = segs.join(".");
        if let Some(q) = self.lookup_method(owners, &full) {
            return Some(q);
        }
        let (last, recv) = segs.split_last()?;
        if let Some((q, SubKind::Class | SubKind::Interface | SubKind::Object)) =
            self.lookup_type(owners, &recv.join("."))
        {
            return Some(format!("{q}.{last}"));
        }
        let e = self.imports.entries.iter().find(|e| e.local == *first)?;
        let upper = first.chars().next().is_some_and(char::is_uppercase);
        if e.namespace
            || (segs.len() == 2 && upper && matches!(e.cible, Cible::Modules(_) | Cible::Alias))
        {
            return self.imports.rewrite(&full);
        }
        None
    }

    // ---- corps de fonction ----

    fn scan(&mut self, f: Node, caller: &str, owners: &[String], class: Option<&Class>) {
        if !self.methods.contains(caller) {
            return;
        }
        let locals = self.bindings(f);
        let mut stack: Vec<(Node, bool)> = ["parameters", "parameter", "body"]
            .iter()
            .filter_map(|k| f.child_by_field_name(k))
            .map(|n| (n, class.is_some()))
            .collect();
        while let Some((n, this_ok)) = stack.pop() {
            let target = match n.kind() {
                "call_expression" => self.call(n, owners, &locals, class.filter(|_| this_ok)),
                "jsx_self_closing_element" => n
                    .child_by_field_name("name")
                    .and_then(|x| self.jsx(x, owners, &locals)),
                "jsx_element" => n
                    .child_by_field_name("open_tag")
                    .and_then(|o| o.child_by_field_name("name"))
                    .and_then(|x| self.jsx(x, owners, &locals)),
                _ => None,
            };
            if let Some(t) = target {
                let r = (caller.to_string(), t);
                self.out.insert(r);
            }
            let ok = this_ok && !rebinds_this(n.kind());
            let mut cur = n.walk();
            let kids: Vec<Node> = n.named_children(&mut cur).collect();
            stack.extend(kids.into_iter().rev().map(|c| (c, ok)));
        }
    }

    fn jsx(&self, name: Node, owners: &[String], locals: &Locals) -> Option<String> {
        let t: String = self.text(name).split_whitespace().collect();
        let segs: Vec<String> = t.split('.').map(str::to_string).collect();
        if !segs.iter().all(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        }) {
            return None;
        }
        if segs.len() == 1 {
            let n = &segs[0];
            if !n.chars().next().is_some_and(char::is_uppercase) || locals.contains_key(n) {
                return None;
            }
            return self.resolve_plain(owners, n);
        }
        self.resolve_chain(owners, locals, &segs, false)
    }

    fn call(
        &self,
        n: Node,
        owners: &[String],
        locals: &Locals,
        class: Option<&Class>,
    ) -> Option<String> {
        let f = n.child_by_field_name("function")?;
        match f.kind() {
            "identifier" => {
                let name = self.text(f);
                if locals.contains_key(&name) {
                    return None;
                }
                self.resolve_plain(owners, &name)
            }
            "member_expression" => {
                let prop = f.child_by_field_name("property")?;
                if !matches!(
                    prop.kind(),
                    "property_identifier" | "private_property_identifier"
                ) {
                    return None;
                }
                let prop = self.text(prop).trim_start_matches('#').to_string();
                let obj = unwrap_non_null(f.child_by_field_name("object")?);
                match obj.kind() {
                    "this" => {
                        let c = class?;
                        let q = format!("{}.{prop}", c.q);
                        self.methods.contains(&q).then_some(q)
                    }
                    "member_expression"
                        if unwrap_non_null(obj.child_by_field_name("object")?).kind() == "this" =>
                    {
                        let c = class?;
                        let field = obj.child_by_field_name("property")?;
                        if !matches!(
                            field.kind(),
                            "property_identifier" | "private_property_identifier"
                        ) {
                            return None;
                        }
                        let field = self.text(field).trim_start_matches('#').to_string();
                        let ty = c.fields.get(&field)?.as_ref()?;
                        Some(format!("{}.{prop}", self.resolve_type(owners, ty)?))
                    }
                    _ => {
                        let mut segs = chain(self, obj)?;
                        segs.push(prop);
                        self.resolve_chain(owners, locals, &segs, true)
                    }
                }
            }
            _ => None,
        }
    }
}

fn bind(locals: &mut Locals, name: &str, ty: Option<String>) {
    match locals.get(name) {
        Some(old) if *old != ty => {
            locals.insert(name.to_string(), None);
        }
        Some(_) => {}
        None => {
            locals.insert(name.to_string(), ty);
        }
    }
}

fn unwrap_non_null(n: Node) -> Node {
    let mut n = n;
    while n.kind() == "non_null_expression" {
        match n.named_child(0) {
            Some(c) => n = c,
            None => break,
        }
    }
    n
}

/// Segments d'une chaîne `a.b.c` d'identifiants ; `None` pour toute autre expression.
fn chain(c: &Collector, n: Node) -> Option<Vec<String>> {
    let mut segs = Vec::new();
    let mut cur = unwrap_non_null(n);
    loop {
        match cur.kind() {
            "identifier" => {
                segs.push(c.text(cur));
                break;
            }
            "member_expression" => {
                let p = cur.child_by_field_name("property")?;
                if p.kind() != "property_identifier" {
                    return None;
                }
                segs.push(c.text(p));
                cur = unwrap_non_null(cur.child_by_field_name("object")?);
            }
            _ => return None,
        }
    }
    segs.reverse();
    Some(segs)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::parse::{js, Lang};

    fn calls(lang: Lang, path: &str, src: &str) -> Vec<String> {
        js::extract(lang, Path::new(path), src)
            .unwrap()
            .references
            .iter()
            .filter(|r| r.kind.as_str() == "calls")
            .map(|r| format!("{} -> {}", r.source, r.target))
            .collect()
    }

    fn ts(path: &str, src: &str) -> Vec<String> {
        calls(Lang::TypeScript, path, src)
    }

    #[test]
    fn req034_ac2_this_dans_champ_fleche() {
        let r = ts(
            "m.ts",
            "abstract class S { handle = () => this.of2(); of2() {} }\n",
        );
        assert_eq!(r, vec!["method:m.S.handle -> m.S.of2"]);
    }

    #[test]
    fn req034_ac3_fonctions_de_premier_niveau() {
        let src = "function helper() {}\nconst slug = (s: string) => s;\n\
            export function formatDate(d: Date) { helper(); slug('x'); console.log(d); fetch('/x'); [1].map(() => helper()); }\nhelper();\n";
        let r = ts("src/util/format.ts", src);
        assert_eq!(
            r,
            vec![
                "method:src/util/format.formatDate -> src/util/format.helper",
                "method:src/util/format.formatDate -> src/util/format.slug",
            ]
        );
    }

    #[test]
    fn req034_ac4_commonjs() {
        let src = "const { formatDate: fd } = require('./util/format');\nconst util = require('./util/format');\n\
            function start() { fd(); util.slug(); }\nmodule.exports = { start };\n";
        let r = calls(Lang::JavaScript, "src/server.cjs", src);
        assert_eq!(
            r,
            vec![
                "method:src/server.start -> formatDate",
                "method:src/server.start -> slug"
            ]
        );
    }

    #[test]
    fn req034_ac5_jsx() {
        let r = calls(
            Lang::JavaScript,
            "src/App.jsx",
            "import Button from './Button';\nexport function App() { return <div><Button label=\"x\"/></div>; }\n",
        );
        assert_eq!(r, vec!["method:src/App.App -> Button"]);
        let src = "import { Item } from './Item';\nimport * as ui from './ui';\n\
            export const List = ({ items }: P) => <ul>{items.map(i => <Item key={i}/>)}<ui.Card/><>x</></ul>;\n";
        let r = calls(Lang::Tsx, "src/components/List.tsx", src);
        assert_eq!(
            r,
            vec![
                "method:src/components/List.List -> Item",
                "method:src/components/List.List -> Card"
            ]
        );
    }

    #[test]
    fn req034_jsx_composant_lie_localement_ou_minuscule() {
        let src =
            "import { Item } from './Item';\nexport function A({ Item }) { return <Item/>; }\n\
            export function B() { const foo = 1; return <foo.Bar/>; }\n";
        assert!(calls(Lang::Tsx, "a.tsx", src).is_empty());
    }

    #[test]
    fn req034_ac6_externes_et_alias() {
        let src = "import React, { useState } from 'react';\nimport { readFile } from 'node:fs';\nimport { cn } from '@/lib/utils';\n\
            export class Page extends React.Component { render() { useState(); readFile(); cn(); React.createElement(1); } }\n";
        let r = ts("src/page.tsx", src);
        assert_eq!(r, vec!["method:src/page.Page.render -> cn"]);
    }

    #[test]
    fn req034_ac7_alias_et_instance() {
        let r = ts(
            "src/a.ts",
            "import { a as c } from './m';\nimport { X } from './services';\n\
             export function f() { c(); const x = new X(); x.run(); }\n",
        );
        assert_eq!(r, vec!["method:src/a.f -> a", "method:src/a.f -> X.run"]);
    }

    #[test]
    fn req034_masquage_par_un_parametre() {
        let src = "function f() {}\nimport { g } from './g';\n\
            export function a(f: () => void) { f(); }\nexport function b() { const g = 1; g(); }\n\
            export function c() { try {} catch (f) { f(); } }\nexport function d() { f(); }\n";
        assert_eq!(ts("p.ts", src), vec!["method:p.d -> p.f"]);
    }

    #[test]
    fn req034_this_chaine_et_inconnus() {
        let src = "class C { a() {} run() { this.a(); this.b(); this.a().c(); foo.bar(); this.h[0](); super.run(); } }\n";
        assert_eq!(ts("c.ts", src), vec!["method:c.C.run -> c.C.a"]);
    }

    #[test]
    fn req034_receveur_type() {
        let src = "import { Repo } from './repo';\nimport * as m from './model';\n\
            class S { private repo: Repo; #q: m.Q = x; other = new Repo(); constructor(private readonly svc: Local, p: Repo) {}\n\
            go(o: Repo, l: Local, z: string[]) { this.repo.save(); this.#q.go(); this.other.x(); this.svc.y(); o.del(); l.z(); z.map(); this.nope.f(); }\n}\n\
            class Local { z() {} y() {} }\n";
        let r = ts("src/s.ts", src);
        assert_eq!(
            r,
            vec![
                "method:src/s.S.go -> Repo.save",
                "method:src/s.S.go -> Q.go",
                "method:src/s.S.go -> Repo.x",
                "method:src/s.S.go -> src/s.Local.y",
                "method:src/s.S.go -> Repo.del",
                "method:src/s.S.go -> src/s.Local.z",
            ]
        );
    }

    #[test]
    fn req034_statique_et_classe_importee() {
        let src = "import { Util } from './util';\nclass L { static s() {} }\n\
            export function f() { Util.make(); L.s(); Math.max(1); const n = new Util(); }\n";
        assert_eq!(
            ts("src/f.ts", src),
            vec!["method:src/f.f -> Util.make", "method:src/f.f -> src/f.L.s"]
        );
    }

    #[test]
    fn req034_namespace_import_et_local() {
        let src = "import * as ns from './n';\nnamespace N { export function g() {} export function h() { g(); } }\n\
            export function f() { ns.run(); ns.T.make(); N.g(); }\n";
        assert_eq!(
            ts("src/f.ts", src),
            vec![
                "method:src/f.N.h -> src/f.N.g",
                "method:src/f.f -> run",
                "method:src/f.f -> T.make",
                "method:src/f.f -> src/f.N.g"
            ]
        );
    }

    #[test]
    fn req034_deux_imports_de_meme_nom_exporte() {
        // Le format de contexte (REQ-008) ne distingue pas les deux modules : la référence
        // porte le nom exporté, la résolution l'écartera comme ambiguë ; ici, une seule
        // référence par nom, sans confusion avec l'alias local.
        let src = "import { X } from './a';\nimport { X as Y } from './b';\n\
            export function f() { X(); Y(); }\n";
        assert_eq!(ts("src/f.ts", src), vec!["method:src/f.f -> X"]);
        let s = js::extract(Lang::TypeScript, Path::new("src/f.ts"), src).unwrap();
        let ctx = s.context.unwrap();
        assert!(
            ctx.contains("import src/a.X") && ctx.contains("import src/b.X"),
            "{ctx}"
        );
    }

    #[test]
    fn req034_fonctions_imbriquees_et_rappels() {
        let src = "function h() {}\nclass K { m() { const f = () => h(); [1].forEach(function () { h(); }); class L { n() { h(); this.m(); } } } }\n";
        assert_eq!(ts("k.ts", src), vec!["method:k.K.m -> k.h"]);
    }

    #[test]
    fn req034_export_default_et_new() {
        let src = "import { X } from './x';\nexport default function () { new X(); X(); }\n";
        assert_eq!(ts("d.ts", src), vec!["method:d.default -> X"]);
    }
}
