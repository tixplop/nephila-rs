//! Structure préparée d'un fichier (DEC-086) : conversion, contrôles et calcul des clés faits par
//! les fils d'extraction, pour que l'écrivain n'exécute plus que des instructions SQL.

use std::cell::Cell;
use std::collections::HashSet;

use crate::error::{Error, Result};
use crate::model::{EdgeKind, Node, NodeKind, Ref};
use crate::parse::FileStructure;

/// Référence prête à être insérée : source déclarée par le fichier, clé et tête du nom visé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRef {
    pub source: String,
    pub kind: EdgeKind,
    pub target_name: String,
    /// Clé du nom visé (`resolve::name_keys`).
    pub key: String,
    /// Tête du nom visé (`resolve::name_keys`).
    pub head: Option<String>,
}

/// Enregistrements d'un fichier prêts à écrire : nœuds sans doublon d'identifiant (premier
/// exemplaire, ordre de déclaration), références dont la source est déclarée, contexte d'imports
/// unique des références.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStructure {
    pub path: String,
    pub nodes: Vec<Node>,
    pub refs: Vec<PreparedRef>,
    /// Contexte d'imports du fichier ; `None` quand le fichier n'a aucune référence.
    pub context: Option<String>,
    /// Références écartées parce que leur source n'est pas déclarée.
    pub dropped: usize,
}

thread_local! {
    static PREPARATIONS: Cell<u64> = const { Cell::new(0) };
}

fn note_preparation() {
    PREPARATIONS.with(|c| c.set(c.get() + 1));
    crate::diag::add(crate::diag::Counter::Preparations, 1);
}

/// Préparations faites par le fil courant (diagnostic, AC-074-2).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn preparations_on_thread() -> u64 {
    PREPARATIONS.with(Cell::get)
}

/// Nom court d'un nœud, selon son genre.
fn short_name(kind: NodeKind, qualified: &str) -> &str {
    let after = |seps: &[char]| qualified.rsplit(seps).next().unwrap_or(qualified);
    match kind {
        NodeKind::Document => after(&['/']),
        NodeKind::Module => after(&[':', '/']),
        NodeKind::Section => after(&['#']),
        NodeKind::Dependency => after(&[':']),
        // Chemin de clé : tout ce qui suit le premier `#` (un nom de membre peut en contenir).
        NodeKind::Key => qualified.split_once('#').map_or(qualified, |(_, k)| k),
        NodeKind::Table => after(&['.']),
        _ => after(&['.']),
    }
}

/// Référence d'entrée des contrôles : source, genre, nom visé, contexte.
type RawRef<'a> = (&'a str, EdgeKind, &'a str, Option<&'a str>);

/// Que faire d'une référence dont la source n'est pas déclarée.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Undeclared {
    /// L'écarter et la compter (structure issue de l'extraction).
    Drop,
    /// Refuser l'écriture (API brute).
    Reject,
}

/// Prépare la structure d'un fichier : nœuds dédoublonnés par identifiant, références filtrées
/// (sources non déclarées écartées et comptées), clé et tête de chaque nom visé, contexte.
pub fn prepare_structure(path: &str, x: &FileStructure) -> Result<PreparedStructure> {
    note_preparation();
    let mut nodes: Vec<Node> = Vec::with_capacity(x.declares.len());
    let mut ids: HashSet<String> = HashSet::with_capacity(x.declares.len());
    for d in &x.declares {
        let id = d.id();
        if ids.contains(&id) {
            continue;
        }
        // Dépendances externes et concepts n'ont pas de fichier propre.
        let file = match d.kind {
            NodeKind::Dependency | NodeKind::Concept => None,
            _ => Some(path),
        };
        let node = Node::new(
            d.kind,
            &d.qualified,
            d.short
                .as_deref()
                .unwrap_or_else(|| short_name(d.kind, &d.qualified)),
            file,
            d.subkind,
            d.summary.as_deref(),
        );
        ids.insert(id);
        nodes.push(node);
    }
    let declared: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let ctx = x.context.as_deref();
    let raw: Vec<RawRef<'_>> = x
        .references
        .iter()
        .map(|r| (r.source.as_str(), r.kind, r.target.as_str(), ctx))
        .collect();
    let (refs, context, dropped) = checked_refs(path, &declared, &raw, Undeclared::Drop)?;
    Ok(PreparedStructure {
        path: path.to_string(),
        nodes,
        refs,
        context,
        dropped,
    })
}

/// Adaptateur de l'API brute (`replace_structure(s)`) : mêmes contrôles, mais une référence dont
/// la source n'est pas déclarée est refusée au lieu d'être écartée.
pub fn prepare_raw(path: &str, nodes: &[Node], refs: &[Ref]) -> Result<PreparedStructure> {
    note_preparation();
    for n in nodes {
        if n.file.as_deref().is_some_and(|f| f != path) {
            return Err(Error::Message(format!(
                "le nœud {} n'appartient pas au fichier {path}",
                n.id
            )));
        }
    }
    let mut seen: HashSet<&str> = HashSet::with_capacity(nodes.len());
    let kept: Vec<Node> = nodes
        .iter()
        .filter(|n| seen.insert(n.id.as_str()))
        .cloned()
        .collect();
    let raw: Vec<RawRef<'_>> = refs
        .iter()
        .map(|r| {
            (
                r.source.as_str(),
                r.kind,
                r.target_name.as_str(),
                r.context.as_deref(),
            )
        })
        .collect();
    let (prepared, context, dropped) = checked_refs(path, &seen, &raw, Undeclared::Reject)?;
    Ok(PreparedStructure {
        path: path.to_string(),
        nodes: kept,
        refs: prepared,
        context,
        dropped,
    })
}

/// Contrôles (dans l'ordre : genre `contains`, contexte unique, source déclarée) puis calcul de
/// la clé et de la tête de chaque référence conservée.
fn checked_refs(
    path: &str,
    declared: &HashSet<&str>,
    refs: &[RawRef<'_>],
    policy: Undeclared,
) -> Result<(Vec<PreparedRef>, Option<String>, usize)> {
    let mut dropped = 0;
    let selected: Vec<&RawRef<'_>> = refs
        .iter()
        .filter(|r| {
            if policy == Undeclared::Drop && !declared.contains(r.0) {
                dropped += 1;
                false
            } else {
                true
            }
        })
        .collect();
    // Aucun lien `contains` n'est produit par des références : ils sont dérivés (DEC-077).
    if selected.iter().any(|r| r.1 == EdgeKind::Contains) {
        return Err(Error::Message(format!(
            "référence de genre contains refusée dans {path}"
        )));
    }
    // Contexte unique des références d'analyse du fichier (DEC-073).
    let context = selected.first().and_then(|r| r.3);
    if selected.iter().any(|r| r.3 != context) {
        return Err(Error::Message(format!(
            "références aux contextes d'imports différents dans {path}"
        )));
    }
    if policy == Undeclared::Reject {
        if let Some(r) = selected.iter().find(|r| !declared.contains(r.0)) {
            return Err(Error::Message(format!(
                "référence dont la source {} n'est pas déclarée dans {path}",
                r.0
            )));
        }
    }
    let prepared = selected
        .iter()
        .map(|r| {
            let (key, head) = crate::resolve::name_keys(r.2);
            PreparedRef {
                source: r.0.to_string(),
                kind: r.1,
                target_name: r.2.to_string(),
                key,
                head,
            }
        })
        .collect();
    Ok((prepared, context.map(str::to_string), dropped))
}

/// Oracle : conversion de la 0.22.0 (`to_records` et `short_name` de HEAD avant DEC-086).
#[cfg(test)]
pub(crate) mod oracle {
    use crate::model::{Node, NodeKind, Ref};
    use crate::parse::FileStructure;

    pub(crate) fn short_name(kind: NodeKind, qualified: &str) -> &str {
        let after = |seps: &[char]| qualified.rsplit(seps).next().unwrap_or(qualified);
        match kind {
            NodeKind::Document => after(&['/']),
            NodeKind::Module => after(&[':', '/']),
            NodeKind::Section => after(&['#']),
            NodeKind::Dependency => after(&[':']),
            NodeKind::Key => qualified.split_once('#').map_or(qualified, |(_, k)| k),
            NodeKind::Table => after(&['.']),
            _ => after(&['.']),
        }
    }

    pub(crate) fn to_records(path: &str, x: &FileStructure) -> (Vec<Node>, Vec<Ref>, usize) {
        let mut nodes: Vec<Node> = Vec::new();
        for d in &x.declares {
            let id = d.id();
            if nodes.iter().any(|n| n.id == id) {
                continue;
            }
            let file = match d.kind {
                NodeKind::Dependency | NodeKind::Concept => None,
                _ => Some(path),
            };
            nodes.push(Node::new(
                d.kind,
                &d.qualified,
                d.short
                    .as_deref()
                    .unwrap_or_else(|| short_name(d.kind, &d.qualified)),
                file,
                d.subkind,
                d.summary.as_deref(),
            ));
        }
        let mut refs = Vec::new();
        let mut dropped = 0;
        for r in &x.references {
            if !nodes.iter().any(|n| n.id == r.source) {
                dropped += 1;
                continue;
            }
            refs.push(Ref {
                source: r.source.clone(),
                kind: r.kind,
                target_name: r.target.clone(),
                context: x.context.clone(),
            });
        }
        (nodes, refs, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::oracle;
    use super::*;
    use crate::model::{FileRecord, FileState, SubKind};
    use crate::parse::{Declared, Reference};
    use crate::store::Store;

    fn decl(kind: NodeKind, q: &str, short: Option<&str>, summary: Option<&str>) -> Declared {
        Declared {
            kind,
            qualified: q.into(),
            subkind: None,
            summary: summary.map(str::to_string),
            short: short.map(str::to_string),
        }
    }

    fn reference(source: &str, kind: EdgeKind, target: &str) -> Reference {
        Reference {
            source: source.into(),
            kind,
            target: target.into(),
        }
    }

    /// Prépare et compare à l'oracle : nœuds et références dans le même ordre, doublons compris,
    /// même nombre d'écartées, clé et tête = `name_keys`, contexte reporté.
    fn assert_matches_oracle(path: &str, x: &FileStructure) {
        let (nodes, refs, dropped) = oracle::to_records(path, x);
        let p = prepare_structure(path, x).unwrap();
        assert_eq!(p.path, path);
        assert_eq!(p.nodes, nodes);
        assert_eq!(p.dropped, dropped);
        assert_eq!(p.refs.len(), refs.len());
        for (a, b) in p.refs.iter().zip(&refs) {
            assert_eq!(
                (&a.source, a.kind, &a.target_name),
                (&b.source, b.kind, &b.target_name)
            );
            let (key, head) = crate::resolve::name_keys(&b.target_name);
            assert_eq!((&a.key, &a.head), (&key, &head));
        }
        let expected_ctx = refs.first().and_then(|r| r.context.clone());
        assert_eq!(p.context, expected_ctx);
    }

    const AC069_NAMES: [&str; 8] = [
        "a.b.C", "C", " C ", "T.m", "x/y#z", "g:a:1.0", "a.b/c.d", "",
    ];
    const AC069_EXPECTED: [(&str, &str, Option<&str>); 8] = [
        ("a.b.C", "C", Some("a")),
        ("C", "C", None),
        (" C ", "C", None),
        ("T.m", "m", Some("T")),
        ("x/y#z", "z", None),
        ("g:a:1.0", "0", Some("g:a:1")),
        ("a.b/c.d", "d", Some("a")),
        ("", "", None),
    ];

    #[test]
    fn req074_ac1_cas_ecrits_a_la_main() {
        let mut x = FileStructure {
            context: Some("package p;\nimport a.B".into()),
            ..Default::default()
        };
        // Identifiant déclaré trois fois aux attributs différents : le premier exemplaire seul.
        let mut first = decl(NodeKind::Type, "p.A", Some("A1"), Some("un"));
        first.subkind = Some(SubKind::Class);
        let mut second = decl(NodeKind::Type, "p.A", Some("A2"), None);
        second.subkind = Some(SubKind::Interface);
        x.declares = vec![
            first,
            decl(NodeKind::Method, "p.A.m", None, None),
            second,
            decl(NodeKind::Type, "p.A", None, Some("trois")),
            decl(NodeKind::Dependency, "org:lib", None, None),
            decl(NodeKind::Concept, "Billing", None, None),
            // Noms d'AC-069-2 : espaces de bord, tête, séparateurs variés.
            decl(NodeKind::Key, "c.json#a#b.c", None, None),
            decl(NodeKind::Table, "shop.products", None, None),
        ];
        x.references = vec![
            reference("type:p.A", EdgeKind::Extends, "  a.B "),
            reference("type:p.A", EdgeKind::Extends, "  a.B "),
            reference("type:ghost.G", EdgeKind::Calls, "x"),
            reference("method:p.A.m", EdgeKind::Calls, "Foo.bar"),
            reference("method:p.A.m", EdgeKind::Mentions, "a/b:c#d"),
            reference("concept:Billing", EdgeKind::References, "\\\\srv\\x"),
            reference("type:p.A", EdgeKind::Imports, ""),
            reference("method:p.Z.zz", EdgeKind::Calls, "y"),
        ];
        for t in AC069_NAMES {
            x.references
                .push(reference("type:p.A", EdgeKind::References, t));
        }
        assert_matches_oracle("src/A.java", &x);
        // Valeurs attendues écrites en dur (AC-069-2), indépendantes de `name_keys`.
        let p = prepare_structure("src/A.java", &x).unwrap();
        for (name, key, head) in AC069_EXPECTED {
            let r = p
                .refs
                .iter()
                .rev()
                .find(|r| r.target_name == *name)
                .unwrap_or_else(|| panic!("référence {name:?} absente"));
            assert_eq!(r.key, key, "clé de {name:?}");
            assert_eq!(r.head.as_deref(), head, "tête de {name:?}");
        }
        let p = prepare_structure("src/A.java", &x).unwrap();
        assert_eq!(p.dropped, 2);
        assert_eq!(p.nodes.len(), 6);
        assert_eq!(p.nodes[0].short, "A1");
        assert_eq!(p.nodes[0].summary.as_deref(), Some("un"));
        // Contexte absent.
        x.context = None;
        assert_matches_oracle("src/A.java", &x);
        assert_eq!(prepare_structure("src/A.java", &x).unwrap().context, None);
        // Aucune référence conservée : pas de contexte.
        x.references.clear();
        x.context = Some("ctx".into());
        assert_matches_oracle("src/A.java", &x);
        assert_eq!(prepare_structure("src/A.java", &x).unwrap().context, None);
        // Fichier vide.
        assert_matches_oracle("E.java", &FileStructure::default());
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    #[test]
    fn req074_ac1_trois_cents_structures_aleatoires() {
        let mut g = Lcg(0x05ee_d074);
        let quals = [
            "p.A",
            "p.A.m",
            "p.B",
            "org:lib",
            "docs/x.md",
            "docs/x.md#Intro",
            "c.json#a#b.c",
            "shop.products",
            "Billing",
            "a/b",
        ];
        let targets = [
            "A",
            "p.A",
            " p.B ",
            "Foo.bar.baz",
            "a/b:c#d",
            "x\\y",
            "",
            "Mod:lib",
            "a.b.C",
            "C",
            " C ",
            "T.m",
            "x/y#z",
            "g:a:1.0",
            "a.b/c.d",
        ];
        let edge_kinds = [
            EdgeKind::DependsOn,
            EdgeKind::Imports,
            EdgeKind::Extends,
            EdgeKind::Implements,
            EdgeKind::LinksTo,
            EdgeKind::Mentions,
            EdgeKind::Calls,
            EdgeKind::References,
        ];
        for _ in 0..300 {
            let mut x = FileStructure::default();
            if g.next(2) == 0 {
                x.context = Some(format!("package p{};", g.next(5)));
            }
            for _ in 0..g.next(9) {
                let kind = NodeKind::ALL[g.next(NodeKind::ALL.len())];
                let q = quals[g.next(quals.len())];
                let short = (g.next(3) == 0).then(|| ["s1", "s2"][g.next(2)]);
                let summary = (g.next(3) == 0).then(|| ["r1", "r2"][g.next(2)]);
                x.declares.push(decl(kind, q, short, summary));
            }
            for _ in 0..g.next(12) {
                // La source est le plus souvent un nœud déclaré, parfois non.
                let source = if !x.declares.is_empty() && g.next(4) != 0 {
                    x.declares[g.next(x.declares.len())].id()
                } else {
                    format!("type:{}", quals[g.next(quals.len())])
                };
                x.references.push(reference(
                    &source,
                    edge_kinds[g.next(edge_kinds.len())],
                    targets[g.next(targets.len())],
                ));
            }
            assert_matches_oracle("f/File.x", &x);
        }
    }

    #[test]
    fn short_name_key_et_table() {
        assert_eq!(
            short_name(NodeKind::Key, "app/application.yml#spring.datasource.url"),
            "spring.datasource.url"
        );
        assert_eq!(short_name(NodeKind::Key, "c.json#a#b.c"), "a#b.c");
        assert_eq!(short_name(NodeKind::Table, "shop.products"), "products");
        assert_eq!(short_name(NodeKind::Table, "orders"), "orders");
        assert_eq!(
            short_name(NodeKind::Document, "db/V1__init.sql"),
            "V1__init.sql"
        );
    }

    #[test]
    fn req074_ac1_nom_court_comme_l_oracle() {
        for k in NodeKind::ALL {
            for q in [
                "a.b.c",
                "a/b/c",
                "a:b:c",
                "f#a#b.c",
                "plain",
                "x/y#z.w:v",
                "",
            ] {
                assert_eq!(short_name(*k, q), oracle::short_name(*k, q), "{k:?} {q}");
            }
        }
    }

    #[test]
    fn req074_ac1_contains_et_contextes_refuses() {
        let mut x = FileStructure::default();
        x.declares.push(decl(NodeKind::Type, "p.A", None, None));
        x.references
            .push(reference("type:p.A", EdgeKind::Contains, "B"));
        let e = prepare_structure("A.java", &x).unwrap_err();
        assert_eq!(
            e.to_string(),
            "référence de genre contains refusée dans A.java"
        );
    }

    fn record(path: &str) -> FileRecord {
        FileRecord {
            path: path.into(),
            hash: "h".into(),
            size: 1,
            mtime: 0,
            recorded_at: 0,
            state: FileState::Pending,
        }
    }

    fn rf(source: &str, kind: EdgeKind, target: &str, ctx: Option<&str>) -> Ref {
        Ref {
            source: source.into(),
            kind,
            target_name: target.into(),
            context: ctx.map(str::to_string),
        }
    }

    /// AC-074-3 : l'API brute rend les erreurs de la 0.22.0 et laisse le graphe inchangé.
    #[test]
    fn req074_ac3_api_brute_memes_erreurs_graphe_inchange() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[record("A.java")]).unwrap();
        let a = Node::new(
            NodeKind::Type,
            "p.A",
            "A",
            Some("A.java"),
            Some(SubKind::Class),
            None,
        );
        st.replace_structure("A.java", std::slice::from_ref(&a), &[])
            .unwrap();
        let before = st.nodes().unwrap();
        let b = Node::new(NodeKind::Type, "p.B", "B", Some("A.java"), None, None);
        let nodes = [b.clone()];

        let err = st
            .replace_structure(
                "A.java",
                &nodes,
                &[rf("type:p.B", EdgeKind::Contains, "x", None)],
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "référence de genre contains refusée dans A.java"
        );
        let err = st
            .replace_structure(
                "A.java",
                &nodes,
                &[
                    rf("type:p.B", EdgeKind::Calls, "x", Some("c1")),
                    rf("type:p.B", EdgeKind::Calls, "y", Some("c2")),
                ],
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "références aux contextes d'imports différents dans A.java"
        );
        let err = st
            .replace_structure(
                "A.java",
                &nodes,
                &[rf("type:p.Z", EdgeKind::Calls, "x", None)],
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "référence dont la source type:p.Z n'est pas déclarée dans A.java"
        );
        let other = Node::new(NodeKind::Type, "p.C", "C", Some("Other.java"), None, None);
        let err = st.replace_structure("A.java", &[other], &[]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "le nœud type:p.C n'appartient pas au fichier A.java"
        );
        let err = st
            .replace_structure("Inconnu.java", &nodes, &[])
            .unwrap_err();
        assert_eq!(err.to_string(), "fichier inconnu du graphe : Inconnu.java");
        assert_eq!(st.nodes().unwrap(), before);
    }
}
