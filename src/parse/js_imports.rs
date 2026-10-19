//! Imports JavaScript et TypeScript (REQ-034, DEC-040) : résolution des spécifieurs, table des
//! noms importés, contexte de résolution du fichier.

use std::cell::RefCell;

use tree_sitter::Node;

/// Extensions de code JS/TS retirées d'un spécifieur (`.d.ts` et variantes d'abord).
const CODE_EXT: [&str; 8] = ["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"];
const DECL_EXT: [&str; 3] = [".d.ts", ".d.mts", ".d.cts"];
/// Extensions de ressource : le spécifieur est ignoré (liste de planification de REQ-034).
const RESOURCE_EXT: [&str; 26] = [
    "css", "scss", "sass", "less", "json", "svg", "png", "jpg", "jpeg", "gif", "webp", "ico",
    "woff", "woff2", "ttf", "html", "md", "txt", "yaml", "yml", "wasm", "vue", "svelte", "astro",
    "graphql", "gql",
];

/// Cible d'un spécifieur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cible {
    /// Spécifieur sans effet sur le graphe (hors du dépôt, ressource).
    Ignore,
    /// Modules du dépôt possibles (noms qualifiés, chemin sans extension).
    Modules(Vec<String>),
    /// `@/…`, `~/…` : alias de projet, résolu par le nom seul.
    Alias,
    /// Paquet npm, `node:fs`, URL…
    Externe,
}

/// Cible du spécifieur `spec` écrit dans le fichier `file` (chemin relatif au dépôt).
pub fn specifier(file: &str, spec: &str) -> Cible {
    let spec = spec.trim();
    if spec.starts_with("@/") || spec.starts_with("~/") {
        return Cible::Alias;
    }
    let relative = spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../");
    if !relative {
        return Cible::Externe;
    }
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    for seg in spec.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Cible::Ignore;
                }
            }
            s => parts.push(s),
        }
    }
    let mut path = parts.join("/");
    if let Some(last) = parts.last() {
        if let Some(stem) = DECL_EXT.iter().find_map(|e| last.strip_suffix(e)) {
            let keep = parts.len() - 1;
            path = parts[..keep]
                .iter()
                .copied()
                .chain(std::iter::once(stem))
                .collect::<Vec<_>>()
                .join("/");
            return Cible::Modules(vec![path]);
        }
        if let Some((stem, ext)) = last.rsplit_once('.') {
            let ext = ext.to_ascii_lowercase();
            if !stem.is_empty() && CODE_EXT.contains(&ext.as_str()) {
                let keep = parts.len() - 1;
                path = parts[..keep]
                    .iter()
                    .copied()
                    .chain(std::iter::once(stem))
                    .collect::<Vec<_>>()
                    .join("/");
                return Cible::Modules(vec![path]);
            }
            if RESOURCE_EXT.contains(&ext.as_str()) {
                return Cible::Ignore;
            }
        }
    }
    if path.is_empty() {
        return Cible::Modules(vec!["index".to_string()]);
    }
    let index = format!("{path}/index");
    Cible::Modules(vec![path, index])
}

/// Un nom importé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub local: String,
    pub exported: String,
    pub cible: Cible,
    pub namespace: bool,
}

/// Table des noms importés d'un fichier (`local → (nom exporté, cible, espace de noms ?)`).
#[derive(Debug, Default)]
pub struct Imports {
    pub entries: Vec<Import>,
    /// Utilisations `ns.X` rencontrées : (cibles de l'espace de noms, `X`).
    used: RefCell<Vec<(Vec<String>, String)>>,
}

impl Imports {
    /// Lit les imports de premier niveau de l'arbre.
    pub fn collect(root: Node, src: &[u8], file: &str) -> Imports {
        let mut t = Imports::default();
        let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
        let mut cur = root.walk();
        for st in root.named_children(&mut cur) {
            match st.kind() {
                "import_statement" => t.import_statement(st, file, &text),
                "lexical_declaration" | "variable_declaration" => {
                    t.require_declaration(st, file, &text)
                }
                _ => {}
            }
        }
        t
    }

    fn push(&mut self, local: String, exported: String, cible: &Cible, namespace: bool) {
        if !local.is_empty() && !exported.is_empty() {
            self.entries.push(Import {
                local,
                exported,
                cible: cible.clone(),
                namespace,
            });
        }
    }

    fn import_statement(&mut self, st: Node, file: &str, text: &dyn Fn(Node) -> String) {
        let Some(spec) = st
            .child_by_field_name("source")
            .and_then(|s| string_value(s, text))
        else {
            return;
        };
        let cible = specifier(file, &spec);
        let mut cur = st.walk();
        for clause in st.named_children(&mut cur) {
            if clause.kind() != "import_clause" {
                continue;
            }
            let mut c2 = clause.walk();
            for part in clause.named_children(&mut c2) {
                match part.kind() {
                    "identifier" => {
                        let n = text(part);
                        self.push(n.clone(), n, &cible, false);
                    }
                    "namespace_import" => {
                        let mut c3 = part.walk();
                        let id = part
                            .named_children(&mut c3)
                            .find(|c| c.kind() == "identifier");
                        if let Some(id) = id {
                            let n = text(id);
                            self.push(n.clone(), n, &cible, true);
                        }
                    }
                    "named_imports" => {
                        let mut c3 = part.walk();
                        for sp in part.named_children(&mut c3) {
                            if sp.kind() != "import_specifier" {
                                continue;
                            }
                            let name = sp.child_by_field_name("name");
                            let Some(name) = name.filter(|n| n.kind() == "identifier") else {
                                continue;
                            };
                            let exported = text(name);
                            let local = sp
                                .child_by_field_name("alias")
                                .map(text)
                                .unwrap_or_else(|| exported.clone());
                            self.push(local, exported, &cible, false);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn require_declaration(&mut self, st: Node, file: &str, text: &dyn Fn(Node) -> String) {
        let mut cur = st.walk();
        for d in st.named_children(&mut cur) {
            if d.kind() != "variable_declarator" {
                continue;
            }
            let (Some(name), Some(value)) = (
                d.child_by_field_name("name"),
                d.child_by_field_name("value"),
            ) else {
                continue;
            };
            let Some(spec) = require_spec(value, text) else {
                continue;
            };
            let cible = specifier(file, &spec);
            match name.kind() {
                "identifier" => {
                    let n = text(name);
                    self.push(n.clone(), n, &cible, true);
                }
                "object_pattern" => {
                    let mut c2 = name.walk();
                    for p in name.named_children(&mut c2) {
                        match p.kind() {
                            "shorthand_property_identifier_pattern" => {
                                let n = text(p);
                                self.push(n.clone(), n, &cible, false);
                            }
                            "pair_pattern" => {
                                let (Some(k), Some(v)) =
                                    (p.child_by_field_name("key"), p.child_by_field_name("value"))
                                else {
                                    continue;
                                };
                                if v.kind() == "identifier"
                                    && matches!(k.kind(), "property_identifier" | "identifier")
                                {
                                    self.push(text(v), text(k), &cible, false);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Nom à porter par une référence d'héritage ou d'appel écrite `name` : `None` si le nom
    /// vient d'un spécifieur externe ou ignoré ; `ns.X…` réécrit en `X…` (l'utilisation est
    /// notée pour le contexte) ; un alias `c` ramené au nom exporté.
    pub fn rewrite(&self, name: &str) -> Option<String> {
        let (first, rest) = match name.split_once('.') {
            Some((f, r)) => (f, Some(r)),
            None => (name, None),
        };
        let Some(entry) = self.entries.iter().find(|e| e.local == first) else {
            return Some(name.to_string());
        };
        match &entry.cible {
            Cible::Externe | Cible::Ignore => None,
            cible if entry.namespace => {
                let rest = rest.filter(|r| !r.is_empty())?;
                if let Cible::Modules(targets) = cible {
                    let x = rest.split('.').next().unwrap_or(rest).to_string();
                    let mut used = self.used.borrow_mut();
                    let u = (targets.clone(), x);
                    if !used.contains(&u) {
                        used.push(u);
                    }
                }
                Some(rest.to_string())
            }
            _ => Some(match rest {
                Some(r) => format!("{}.{r}", entry.exported),
                None => entry.exported.clone(),
            }),
        }
    }

    /// Cibles des imports du package : nom exporté de chaque nom importé d'un spécifieur
    /// relatif ou d'alias, hors espace de noms.
    pub fn import_targets(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.entries {
            if !e.namespace
                && matches!(e.cible, Cible::Modules(_) | Cible::Alias)
                && !out.contains(&e.exported)
            {
                out.push(e.exported.clone());
            }
        }
        out
    }

    /// Contexte de résolution du fichier (format de `resolve::parse_context`).
    pub fn context(&self, module: &str) -> String {
        let mut lines = vec![format!("package {module}")];
        let mut add = |l: String| {
            if !lines.contains(&l) {
                lines.push(l);
            }
        };
        for e in &self.entries {
            if let (Cible::Modules(ts), false) = (&e.cible, e.namespace) {
                for t in ts {
                    add(format!("import {t}.{}", e.exported));
                }
            }
        }
        for (ts, x) in self.used.borrow().iter() {
            for t in ts {
                add(format!("import {t}.{x}"));
            }
        }
        lines.join("\n")
    }
}

/// Valeur d'un littéral chaîne sans guillemets ni interpolation.
fn string_value(n: Node, text: &dyn Fn(Node) -> String) -> Option<String> {
    match n.kind() {
        "string" => {
            let t = text(n);
            let t = t.trim_matches(|c| c == '"' || c == '\'');
            (!t.is_empty()).then(|| t.to_string())
        }
        "template_string" => {
            let t = text(n);
            let t = t.trim_matches('`');
            (!t.is_empty() && !t.contains("${")).then(|| t.to_string())
        }
        _ => None,
    }
}

/// `require('x')` : le spécifieur.
fn require_spec(value: Node, text: &dyn Fn(Node) -> String) -> Option<String> {
    if value.kind() != "call_expression" {
        return None;
    }
    let f = value.child_by_field_name("function")?;
    if f.kind() != "identifier" || text(f) != "require" {
        return None;
    }
    let args = value.child_by_field_name("arguments")?;
    if args.named_child_count() != 1 {
        return None;
    }
    string_value(args.named_child(0)?, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Lang;

    fn mods(file: &str, spec: &str) -> Vec<String> {
        match specifier(file, spec) {
            Cible::Modules(m) => m,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn req034_specifieurs_relatifs() {
        assert_eq!(mods("src/a.ts", "./b.js"), vec!["src/b"]);
        assert_eq!(mods("src/a.ts", "../lib/c.tsx"), vec!["lib/c"]);
        assert_eq!(mods("src/a.ts", "./b"), vec!["src/b", "src/b/index"]);
        assert_eq!(
            mods("src/a.ts", "./order.service"),
            vec!["src/order.service", "src/order.service/index"]
        );
        assert_eq!(mods("src/a.ts", "./types.d.ts"), vec!["src/types"]);
        assert_eq!(mods("src/a.ts", "."), vec!["src", "src/index"]);
        assert_eq!(mods("a.ts", "./x/../y.mjs"), vec!["y"]);
        assert_eq!(specifier("a.ts", "../x"), Cible::Ignore);
        assert_eq!(specifier("src/a.ts", "../../x"), Cible::Ignore);
        assert_eq!(specifier("src/a.ts", "./styles.css"), Cible::Ignore);
        assert_eq!(specifier("src/a.ts", "./data.json"), Cible::Ignore);
        assert_eq!(specifier("src/a.ts", "./logo.svg"), Cible::Ignore);
        assert_eq!(specifier("src/a.ts", "./q.graphql"), Cible::Ignore);
        assert_eq!(specifier("src/a.ts", "./q.gql"), Cible::Ignore);
    }

    #[test]
    fn req034_specifieurs_alias_et_externes() {
        assert_eq!(specifier("src/a.ts", "@/lib/utils"), Cible::Alias);
        assert_eq!(specifier("src/a.ts", "~/lib/utils"), Cible::Alias);
        for s in [
            "react",
            "node:fs",
            "@scope/pkg",
            "https://x.org/a.js",
            "/abs",
        ] {
            assert_eq!(specifier("src/a.ts", s), Cible::Externe, "{s}");
        }
    }

    fn table(file: &str, src: &str) -> Imports {
        let lang = if file.ends_with("ts") {
            Lang::TypeScript
        } else {
            Lang::JavaScript
        };
        let tree = lang.parse(src).unwrap();
        Imports::collect(tree.root_node(), src.as_bytes(), file)
    }

    fn entry(t: &Imports, local: &str) -> (String, Cible, bool) {
        let e = t.entries.iter().find(|e| e.local == local).unwrap();
        (e.exported.clone(), e.cible.clone(), e.namespace)
    }

    fn one(p: &str) -> Cible {
        Cible::Modules(vec![p.to_string()])
    }

    #[test]
    fn req034_formes_d_import() {
        let src = "import { a, b as c } from './m.js';\nimport type { T } from './t.js';\n\
            import X from './x.js';\nimport Y, { z } from './y.js';\nimport * as ns from './n.js';\n\
            import './effet.js';\nimport { q } from 'react';\nimport { w } from '@/lib/w';\n";
        let t = table("src/f.ts", src);
        assert_eq!(entry(&t, "a"), ("a".into(), one("src/m"), false));
        assert_eq!(entry(&t, "c"), ("b".into(), one("src/m"), false));
        assert_eq!(entry(&t, "T"), ("T".into(), one("src/t"), false));
        assert_eq!(entry(&t, "X"), ("X".into(), one("src/x"), false));
        assert_eq!(entry(&t, "Y"), ("Y".into(), one("src/y"), false));
        assert_eq!(entry(&t, "z"), ("z".into(), one("src/y"), false));
        assert_eq!(entry(&t, "ns"), ("ns".into(), one("src/n"), true));
        assert_eq!(entry(&t, "q").1, Cible::Externe);
        assert_eq!(entry(&t, "w").1, Cible::Alias);
        assert_eq!(t.entries.len(), 9);
        assert_eq!(t.import_targets(), vec!["a", "b", "T", "X", "Y", "z", "w"]);
    }

    #[test]
    fn req034_formes_require() {
        let src = "const ns = require('./n');\nconst { a, b: c } = require('./m.js');\n\
            const fs = require('fs');\nconst v = require(name);\n";
        let t = table("src/f.js", src);
        assert_eq!(
            entry(&t, "ns"),
            (
                "ns".into(),
                Cible::Modules(vec!["src/n".into(), "src/n/index".into()]),
                true
            )
        );
        assert_eq!(entry(&t, "a"), ("a".into(), one("src/m"), false));
        assert_eq!(entry(&t, "c"), ("b".into(), one("src/m"), false));
        assert_eq!(entry(&t, "fs").1, Cible::Externe);
        assert!(t.entries.iter().all(|e| e.local != "v"));
    }

    #[test]
    fn req034_contexte_produit() {
        let src =
            "import { Order } from '../model/order';\nimport { Repo as R } from './repo.js';\n\
            import * as ns from './n.js';\nimport { e } from 'react';\n";
        let t = table("src/f.ts", src);
        assert_eq!(t.rewrite("ns.B"), Some("B".into()));
        let ctx = t.context("src/f");
        assert_eq!(
            ctx,
            "package src/f\nimport model/order.Order\nimport model/order/index.Order\n\
             import src/repo.Repo\nimport src/n.B"
        );
    }

    #[test]
    fn req034_reecriture_des_noms() {
        let src = "import * as ns from './n.js';\nimport { Repo as R } from './repo.js';\n\
            import React from 'react';\nimport * as ext from 'lib';\n";
        let t = table("src/f.ts", src);
        assert_eq!(t.rewrite("ns.B.Inner"), Some("B.Inner".into()));
        assert_eq!(t.rewrite("ns"), None);
        assert_eq!(t.rewrite("R"), Some("Repo".into()));
        assert_eq!(t.rewrite("Local"), Some("Local".into()));
        assert_eq!(t.rewrite("React.Component"), None);
        assert_eq!(t.rewrite("ext.X"), None);
    }
}
