//! Résolution des liens sans LLM (REQ-008) et liens `contains` dérivés (REQ-005).
//!
//! La passe est une fonction pure de `nodes`, `refs` et `declarations` : elle recalcule l'ensemble
//! des liens en mémoire puis les remplace en une transaction. Le résultat est identique à un
//! recalcul ciblé (références non résolues ou visant des noms ajoutés/supprimés), qui ne serait
//! qu'une optimisation.
//!
//! Le contexte d'une référence est un texte libre dont les lignes (ou segments séparés par `;`)
//! `package a.b` et `import a.b.C` / `import a.b.*` sont reconnues.

use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::model::{node_id, parse_node_id, Edge, EdgeKind, Node, NodeKind, Ref, RefOrigin};
use crate::scan::module_of;
use crate::store::Store;

/// Bilan d'une passe de résolution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveStats {
    pub edges: usize,
    pub unresolved: usize,
}

/// Résultat du calcul : liens et références non résolues.
#[derive(Debug, Clone, Default)]
pub struct Resolution {
    pub edges: Vec<Edge>,
    pub unresolved: Vec<Ref>,
    /// Nœuds `dependency` dérivés des références depends_on sans module du dépôt (DEC-020).
    pub dependencies: Vec<Node>,
}

/// Recalcule tous les liens du graphe à partir des nœuds et des références.
pub fn resolve(store: &mut Store) -> Result<ResolveStats> {
    let r = compute_from(store)?;
    store.replace_derived_dependencies(&r.dependencies)?;
    store.replace_edges(&r.edges)?;
    // Les liens sont dédupliqués par la clé primaire : on compte ce qui est stocké.
    Ok(ResolveStats {
        edges: store.count_edges()?,
        unresolved: r.unresolved.len(),
    })
}

/// Références qui ne se résolvent vers aucun nœud.
pub fn unresolved(store: &Store) -> Result<Vec<Ref>> {
    Ok(compute_from(store)?.unresolved)
}

fn compute_from(store: &Store) -> Result<Resolution> {
    Ok(compute(
        &store.nodes()?,
        &store.refs_with_origin()?,
        &store.declarations()?,
    ))
}

#[derive(Default)]
struct FileContext {
    package: Option<String>,
    imports: Vec<String>,
}

fn parse_context(ctx: Option<&str>) -> FileContext {
    let mut out = FileContext::default();
    let Some(ctx) = ctx else { return out };
    for seg in ctx.split(['\n', ';']) {
        let seg = seg.trim();
        if let Some(p) = seg.strip_prefix("package ") {
            let p = p.trim();
            if !p.is_empty() {
                out.package = Some(p.to_string());
            }
        } else if let Some(i) = seg.strip_prefix("import ") {
            let i = i.trim();
            let i = i.strip_prefix("static ").map_or(i, str::trim);
            if !i.is_empty() {
                out.imports.push(i.to_string());
            }
        }
    }
    out
}

/// Index des nœuds pour la résolution.
struct Index<'a> {
    by_qualified: HashMap<&'a str, Vec<&'a Node>>,
    /// Nom simple (dernier segment) -> nœuds.
    by_short: HashMap<&'a str, Vec<&'a Node>>,
    /// Deux derniers segments (séparateur compris) -> nœuds : évite de parcourir tous les
    /// nœuds portant un nom courant (`get`, `run`…) pour une référence `Type.méthode`.
    by_last_two: HashMap<&'a str, Vec<&'a Node>>,
}

impl<'a> Index<'a> {
    fn new(nodes: &'a [Node]) -> Self {
        let mut by_qualified: HashMap<&str, Vec<&Node>> = HashMap::new();
        let mut by_short: HashMap<&str, Vec<&Node>> = HashMap::new();
        let mut by_last_two: HashMap<&str, Vec<&Node>> = HashMap::new();
        for n in nodes {
            by_last_two
                .entry(last_two_segments(&n.qualified))
                .or_default()
                .push(n);
            by_qualified.entry(&n.qualified).or_default().push(n);
            by_short
                .entry(last_segment(&n.qualified))
                .or_default()
                .push(n);
        }
        Index {
            by_qualified,
            by_short,
            by_last_two,
        }
    }

    /// Nœuds de nom qualifié exact permis par le lien.
    fn exact(&self, name: &str, ok: &dyn Fn(&Node) -> bool) -> Vec<&'a Node> {
        self.by_qualified
            .get(name)
            .map(|v| v.iter().copied().filter(|n| ok(n)).collect())
            .unwrap_or_default()
    }

    /// Nœuds dont le nom qualifié se termine par `name` sur une frontière de segment.
    fn suffix(&self, name: &str, ok: &dyn Fn(&Node) -> bool) -> Vec<&'a Node> {
        let table = if name.contains(SEPARATORS) {
            self.by_last_two.get(last_two_segments(name))
        } else {
            self.by_short.get(last_segment(name))
        };
        let Some(cands) = table else {
            return Vec::new();
        };
        cands
            .iter()
            .copied()
            .filter(|n| ok(n) && is_suffix(&n.qualified, name))
            .collect()
    }
}

const SEPARATORS: [char; 5] = ['.', '/', '#', ':', '\\'];

fn last_segment(q: &str) -> &str {
    q.rsplit(SEPARATORS).next().unwrap_or(q)
}

/// Les deux derniers segments, séparateur compris ; le nom entier s'il n'a qu'un segment.
fn last_two_segments(q: &str) -> &str {
    let mut it = q
        .char_indices()
        .rev()
        .filter(|(_, c)| SEPARATORS.contains(c));
    match (it.next(), it.next()) {
        (Some(_), Some((i, c))) => &q[i + c.len_utf8()..],
        _ => q,
    }
}

fn is_suffix(qualified: &str, name: &str) -> bool {
    if qualified == name {
        return true;
    }
    qualified
        .strip_suffix(name)
        .is_some_and(|rest| rest.ends_with(SEPARATORS))
}

fn parent_of(q: &str) -> Option<&str> {
    q.rfind('.').map(|i| &q[..i])
}

/// Ordre : nom exact, imports, même package, nom simple (ou suffixe qualifié) unique.
/// Une ambiguïté à une étape laisse la référence non résolue.
fn resolve_ref(r: &Ref, source_kind: NodeKind, source_q: &str, idx: &Index) -> Option<String> {
    let ok = |n: &Node| r.kind.allows(source_kind, n.kind);
    let single = |v: Vec<&Node>| -> Option<Option<String>> {
        match v.len() {
            0 => None,
            1 => Some(Some(v[0].id.clone())),
            _ => Some(None),
        }
    };
    let name = r.target_name.trim();
    let ctx = parse_context(r.context.as_deref());

    if let Some(res) = single(idx.exact(name, &ok)) {
        return res;
    }

    // Imports du fichier : le premier segment du nom désigne le type importé.
    let first = name.split('.').next().unwrap_or(name);
    let rest = &name[first.len()..];
    let mut imported: Vec<String> = Vec::new();
    for imp in &ctx.imports {
        if let Some(pkg) = imp.strip_suffix(".*") {
            imported.push(format!("{pkg}.{name}"));
        } else if last_segment(imp) == first {
            imported.push(format!("{imp}{rest}"));
        }
    }
    imported.sort();
    imported.dedup();
    // Un import explicite l'emporte sur un import à joker.
    let explicit: Vec<&String> = imported
        .iter()
        .filter(|c| {
            ctx.imports
                .iter()
                .any(|i| !i.ends_with(".*") && (**c == *i || c.starts_with(&format!("{i}."))))
        })
        .collect();
    for group in [explicit, imported.iter().collect()] {
        let mut found: Vec<&Node> = Vec::new();
        for cand in group {
            found.extend(idx.exact(cand, &ok));
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        found.dedup_by(|a, b| a.id == b.id);
        if let Some(res) = single(found) {
            return res;
        }
    }

    // Même package que la source.
    let package = ctx
        .package
        .clone()
        .or_else(|| derived_package(source_kind, source_q));
    if let Some(pkg) = package.filter(|p| !p.is_empty()) {
        if let Some(res) = single(idx.exact(&format!("{pkg}.{name}"), &ok)) {
            return res;
        }
    }

    single(idx.suffix(name, &ok)).flatten()
}

fn derived_package(kind: NodeKind, q: &str) -> Option<String> {
    match kind {
        NodeKind::Type => parent_of(q).map(str::to_string),
        NodeKind::Method => parent_of(q).and_then(parent_of).map(str::to_string),
        _ => None,
    }
}

/// Calcule les liens résolus, les liens `contains` dérivés et les références non résolues.
pub fn compute(
    nodes: &[Node],
    refs: &[(Ref, RefOrigin)],
    declarations: &[(String, String)],
) -> Resolution {
    let idx = Index::new(nodes);
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut out = Resolution::default();

    let mut dependencies: HashMap<String, Node> = HashMap::new();
    for (r, origin) in refs {
        let Ok((skind, sq)) = parse_node_id(&r.source) else {
            out.unresolved.push(r.clone());
            continue;
        };
        if !ids.contains(r.source.as_str()) {
            continue;
        }
        // depends_on vise des coordonnées : module du dépôt, sinon dépendance dérivée (DEC-020).
        if r.kind == EdgeKind::DependsOn && skind == NodeKind::Module {
            let name = r.target_name.trim();
            if name.is_empty() {
                continue;
            }
            let module = node_id(NodeKind::Module, name);
            let target = if ids.contains(module.as_str()) {
                module
            } else {
                let dep = Node::new(
                    NodeKind::Dependency,
                    name,
                    name.rsplit(':').next().unwrap_or(name),
                    None,
                    None,
                    None,
                );
                let id = dep.id.clone();
                dependencies.entry(id.clone()).or_insert(dep);
                id
            };
            out.edges.push(Edge {
                source: r.source.clone(),
                target,
                kind: r.kind,
            });
            continue;
        }
        match resolve_ref(r, skind, sq, &idx) {
            Some(target) => out.edges.push(Edge {
                source: r.source.clone(),
                target,
                kind: r.kind,
            }),
            // Une mention issue de l'analyse syntaxique non résolue est normale (texte libre).
            None if r.kind == EdgeKind::Mentions && *origin == RefOrigin::Parse => {}
            None => out.unresolved.push(r.clone()),
        }
    }
    out.dependencies = dependencies.into_values().collect();
    out.dependencies.sort_by(|a, b| a.id.cmp(&b.id));

    derive_contains(nodes, declarations, &idx, &mut out.edges);

    let mut seen = HashSet::new();
    out.edges
        .retain(|e| seen.insert((e.source.clone(), e.target.clone(), e.kind)));
    out
}

fn derive_contains(
    nodes: &[Node],
    declarations: &[(String, String)],
    idx: &Index,
    edges: &mut Vec<Edge>,
) {
    let is_kind = |q: &str, k: NodeKind| {
        idx.by_qualified
            .get(q)
            .is_some_and(|v| v.iter().any(|n| n.kind == k))
    };
    let mut push = |from: NodeKind, fq: &str, to: NodeKind, tq: &str| {
        edges.push(Edge {
            source: node_id(from, fq),
            target: node_id(to, tq),
            kind: EdgeKind::Contains,
        });
    };
    for n in nodes {
        match n.kind {
            NodeKind::Type => {
                if let Some(p) = parent_of(&n.qualified).filter(|p| is_kind(p, NodeKind::Package)) {
                    push(NodeKind::Package, p, NodeKind::Type, &n.qualified);
                }
            }
            NodeKind::Method => {
                // Un parent type l'emporte ; sinon un package (fonction de module ES, DEC-040).
                if let Some(parent) = parent_of(&n.qualified) {
                    if is_kind(parent, NodeKind::Type) {
                        push(NodeKind::Type, parent, NodeKind::Method, &n.qualified);
                    } else if is_kind(parent, NodeKind::Package) {
                        push(NodeKind::Package, parent, NodeKind::Method, &n.qualified);
                    }
                }
            }
            NodeKind::Section => {
                if let Some((d, _)) = n.qualified.split_once('#') {
                    if is_kind(d, NodeKind::Document) {
                        push(NodeKind::Document, d, NodeKind::Section, &n.qualified);
                    }
                }
            }
            _ => {}
        }
    }

    // Document de données -> table | clé : le fichier déclarant est le document (son nom qualifié
    // est son chemin) ; une table partagée est contenue par chacun de ses fichiers. Le nom
    // qualifié d'une clé n'est pas découpé (un nom de membre peut contenir `#`).
    for (file, node) in declarations {
        let to = match crate::model::parse_node_id(node) {
            Ok((k @ (NodeKind::Table | NodeKind::Key), q)) => (k, q),
            _ => continue,
        };
        if is_kind(file, NodeKind::Document) {
            push(NodeKind::Document, file, to.0, to.1);
        }
    }

    // Module -> package : module du fichier déclarant le package (AC-005-5).
    let kinds: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut build_dirs: HashMap<String, &Node> = HashMap::new();
    for n in nodes.iter().filter(|n| n.kind == NodeKind::Module) {
        if let Some(f) = &n.file {
            let dir = f.rsplit_once('/').map_or("", |(d, _)| d).to_string();
            build_dirs.entry(dir).or_insert(n);
        }
    }
    let dirs: HashSet<String> = build_dirs.keys().cloned().collect();
    for (file, node) in declarations {
        let Some(n) = kinds
            .get(node.as_str())
            .filter(|n| n.kind == NodeKind::Package)
        else {
            continue;
        };
        if let Some(m) = module_of(file, &dirs).and_then(|d| build_dirs.get(&d)) {
            push(
                NodeKind::Module,
                &m.qualified,
                NodeKind::Package,
                &n.qualified,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{node_id, EdgeKind, FileRecord, FileState, Node, NodeKind, Ref, SubKind};
    use tempfile::TempDir;

    struct Env {
        _dir: TempDir,
        store: Store,
    }

    fn env(files: &[&str]) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path()).unwrap();
        let recs: Vec<FileRecord> = files
            .iter()
            .map(|p| FileRecord {
                path: (*p).into(),
                hash: "h".into(),
                size: 1,
                mtime: 1,
                recorded_at: 0,
                state: FileState::Pending,
            })
            .collect();
        store.insert_files(&recs).unwrap();
        Env { _dir: dir, store }
    }

    fn ty(q: &str, f: &str) -> Node {
        let short = q.rsplit('.').next().unwrap();
        Node::new(
            NodeKind::Type,
            q,
            short,
            Some(f),
            Some(SubKind::Class),
            None,
        )
    }
    fn method(q: &str, f: &str) -> Node {
        let short = q.rsplit('.').next().unwrap();
        Node::new(NodeKind::Method, q, short, Some(f), None, None)
    }
    fn plain(kind: NodeKind, q: &str, f: Option<&str>) -> Node {
        let short = q.rsplit(['.', '/', '#']).next().unwrap();
        Node::new(kind, q, short, f, None, None)
    }
    fn rf(source: &str, kind: EdgeKind, target: &str, ctx: Option<&str>) -> Ref {
        Ref {
            source: source.into(),
            kind,
            target_name: target.into(),
            context: ctx.map(str::to_string),
        }
    }
    fn has_edge(e: &Env, s: &str, k: EdgeKind, t: &str) -> bool {
        e.store
            .edges()
            .unwrap()
            .iter()
            .any(|x| x.source == s && x.kind == k && x.target == t)
    }
    fn tid(q: &str) -> String {
        node_id(NodeKind::Type, q)
    }
    fn mid(q: &str) -> String {
        node_id(NodeKind::Method, q)
    }

    #[test]
    fn req008_ac1_unknown_name_stays_unresolved() {
        let mut e = env(&["A.java"]);
        e.store
            .replace_file(
                "A.java",
                &[ty("A", "A.java"), method("A.run", "A.java")],
                &[rf(&mid("A.run"), EdgeKind::Calls, "Ghost.foo", None)],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 1);
        let un = unresolved(&e.store).unwrap();
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].target_name, "Ghost.foo");
        assert!(!e
            .store
            .edges()
            .unwrap()
            .iter()
            .any(|x| x.kind == EdgeKind::Calls));
    }

    #[test]
    fn req008_ac4_ambiguous_simple_name_unresolved() {
        let mut e = env(&["a/Order.java", "b/Order.java", "S.java"]);
        e.store
            .replace_file("a/Order.java", &[ty("com.a.Order", "a/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file("b/Order.java", &[ty("com.b.Order", "b/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file(
                "S.java",
                &[ty("com.c.Svc", "S.java")],
                &[rf(
                    &tid("com.c.Svc"),
                    EdgeKind::Imports,
                    "Order",
                    Some("package com.c;"),
                )],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 1);
        assert!(!e
            .store
            .edges()
            .unwrap()
            .iter()
            .any(|x| x.kind == EdgeKind::Imports));
    }

    #[test]
    fn req008_ac5_explicit_import_wins_over_same_simple_name() {
        let mut e = env(&["a/Order.java", "b/Order.java", "S.java"]);
        e.store
            .replace_file("a/Order.java", &[ty("com.a.Order", "a/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file("b/Order.java", &[ty("com.b.Order", "b/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file(
                "S.java",
                &[ty("com.c.Svc", "S.java")],
                &[rf(
                    &tid("com.c.Svc"),
                    EdgeKind::Imports,
                    "Order",
                    Some("package com.c;\nimport com.b.Order;"),
                )],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &tid("com.c.Svc"),
            EdgeKind::Imports,
            &tid("com.b.Order")
        ));
        assert!(!has_edge(
            &e,
            &tid("com.c.Svc"),
            EdgeKind::Imports,
            &tid("com.a.Order")
        ));
    }

    /// Base : types `qs` (un fichier chacun) + une source `com.c.Svc` portant la référence.
    fn import_case(qs: &[&str], target: &str, ctx: &str) -> Env {
        let files: Vec<String> = (0..qs.len()).map(|i| format!("t{i}/T.java")).collect();
        let mut names: Vec<&str> = files.iter().map(String::as_str).collect();
        names.push("S.java");
        let mut e = env(&names);
        for (q, f) in qs.iter().zip(&files) {
            e.store.replace_file(f, &[ty(q, f)], &[]).unwrap();
        }
        e.store
            .replace_file(
                "S.java",
                &[ty("com.c.Svc", "S.java")],
                &[rf(&tid("com.c.Svc"), EdgeKind::Imports, target, Some(ctx))],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        e
    }
    fn imports_any(e: &Env) -> bool {
        e.store
            .edges()
            .unwrap()
            .iter()
            .any(|x| x.source == tid("com.c.Svc") && x.kind == EdgeKind::Imports)
    }

    #[test]
    fn req008_ac5_wildcard_ambiguity_unresolved() {
        let e = import_case(
            &["com.a.Order", "com.b.Order"],
            "Order",
            "import com.a.*;\nimport com.b.*;",
        );
        assert!(!imports_any(&e));
    }

    #[test]
    fn req008_ac5_explicit_beats_wildcard() {
        let e = import_case(
            &["com.a.Order", "com.b.Order"],
            "Order",
            "import com.b.Order;\nimport com.a.*;",
        );
        assert!(has_edge(
            &e,
            &tid("com.c.Svc"),
            EdgeKind::Imports,
            &tid("com.b.Order")
        ));
    }

    #[test]
    fn req008_ac5_explicit_import_respects_segment_boundary() {
        // `com.b.Order` explicite ne rend pas `com.b.OrderLine` (joker) prioritaire.
        let e = import_case(
            &["com.a.OrderLine", "com.b.OrderLine", "com.b.Order"],
            "OrderLine",
            "import com.b.Order;\nimport com.a.*;\nimport com.b.*;",
        );
        assert!(!imports_any(&e));
    }

    #[test]
    fn wildcard_import_resolves() {
        let mut e = env(&["b/Order.java", "S.java", "a/Order.java"]);
        e.store
            .replace_file("a/Order.java", &[ty("com.a.Order", "a/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file("b/Order.java", &[ty("com.b.Order", "b/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file(
                "S.java",
                &[ty("com.c.Svc", "S.java")],
                &[rf(
                    &tid("com.c.Svc"),
                    EdgeKind::Imports,
                    "Order",
                    Some("import com.b.*;"),
                )],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &tid("com.c.Svc"),
            EdgeKind::Imports,
            &tid("com.b.Order")
        ));
    }

    #[test]
    fn same_package_beats_other_packages() {
        let mut e = env(&["a/Order.java", "b/Order.java", "S.java"]);
        e.store
            .replace_file("a/Order.java", &[ty("com.a.Order", "a/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file("b/Order.java", &[ty("com.b.Order", "b/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file(
                "S.java",
                &[ty("com.a.Svc", "S.java")],
                &[rf(
                    &tid("com.a.Svc"),
                    EdgeKind::Imports,
                    "Order",
                    Some("package com.a;"),
                )],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &tid("com.a.Svc"),
            EdgeKind::Imports,
            &tid("com.a.Order")
        ));
    }

    #[test]
    fn exact_qualified_name_and_unique_simple_name() {
        let mut e = env(&["a/Order.java", "S.java"]);
        e.store
            .replace_file("a/Order.java", &[ty("com.a.Order", "a/Order.java")], &[])
            .unwrap();
        e.store
            .replace_file(
                "S.java",
                &[ty("com.c.Svc", "S.java")],
                &[
                    rf(&tid("com.c.Svc"), EdgeKind::Imports, "com.a.Order", None),
                    rf(&tid("com.c.Svc"), EdgeKind::Extends, "Order", None),
                ],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 0);
        assert!(has_edge(
            &e,
            &tid("com.c.Svc"),
            EdgeKind::Imports,
            &tid("com.a.Order")
        ));
        assert!(has_edge(
            &e,
            &tid("com.c.Svc"),
            EdgeKind::Extends,
            &tid("com.a.Order")
        ));
    }

    #[test]
    fn method_suffix_and_import_prefix() {
        let mut e = env(&["R.java", "O.java"]);
        e.store
            .replace_file(
                "R.java",
                &[
                    ty("com.acme.OrderRepository", "R.java"),
                    method("com.acme.OrderRepository.save", "R.java"),
                ],
                &[],
            )
            .unwrap();
        e.store
            .replace_file(
                "O.java",
                &[
                    ty("com.x.OrderService", "O.java"),
                    method("com.x.OrderService.place", "O.java"),
                ],
                &[rf(
                    &mid("com.x.OrderService.place"),
                    EdgeKind::Calls,
                    "OrderRepository.save",
                    Some("package com.x;\nimport com.acme.OrderRepository;"),
                )],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &mid("com.x.OrderService.place"),
            EdgeKind::Calls,
            &mid("com.acme.OrderRepository.save")
        ));
        // sans import : suffixe qualifié unique
        e.store
            .replace_file(
                "O.java",
                &[
                    ty("com.x.OrderService", "O.java"),
                    method("com.x.OrderService.place", "O.java"),
                ],
                &[rf(
                    &mid("com.x.OrderService.place"),
                    EdgeKind::Calls,
                    "OrderRepository.save",
                    None,
                )],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &mid("com.x.OrderService.place"),
            EdgeKind::Calls,
            &mid("com.acme.OrderRepository.save")
        ));
    }

    #[test]
    fn target_kind_must_fit_link_kind() {
        // un package de même nom qu'un type ne doit pas être la cible d'un imports
        let mut e = env(&["S.java"]);
        e.store
            .replace_file(
                "S.java",
                &[
                    ty("Svc", "S.java"),
                    plain(NodeKind::Package, "Svc2", Some("S.java")),
                ],
                &[rf(&tid("Svc"), EdgeKind::Imports, "Svc2", None)],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 1);
    }

    #[test]
    fn req008_repair_when_target_appears_and_unlink_when_renamed() {
        let mut e = env(&["A.java", "B.java"]);
        e.store
            .replace_file(
                "A.java",
                &[ty("A", "A.java"), method("A.go", "A.java")],
                &[rf(&mid("A.go"), EdgeKind::Calls, "B.foo", None)],
            )
            .unwrap();
        e.store
            .replace_file("B.java", &[ty("B", "B.java")], &[])
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert_eq!(unresolved(&e.store).unwrap().len(), 1);

        // 008-2 : B.foo apparaît, A inchangé
        e.store
            .replace_file(
                "B.java",
                &[ty("B", "B.java"), method("B.foo", "B.java")],
                &[],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert!(has_edge(&e, &mid("A.go"), EdgeKind::Calls, &mid("B.foo")));
        assert_eq!(st.unresolved, 0);

        // 008-3 : renommée en bar
        e.store
            .replace_file(
                "B.java",
                &[ty("B", "B.java"), method("B.bar", "B.java")],
                &[],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert!(!has_edge(&e, &mid("A.go"), EdgeKind::Calls, &mid("B.foo")));
        assert_eq!(st.unresolved, 1);
        assert_eq!(unresolved(&e.store).unwrap()[0].target_name, "B.foo");
    }

    #[test]
    fn removed_file_unlinks_references() {
        let mut e = env(&["A.java", "B.java"]);
        e.store
            .replace_file(
                "A.java",
                &[ty("A", "A.java")],
                &[rf(&tid("A"), EdgeKind::Extends, "B", None)],
            )
            .unwrap();
        e.store
            .replace_file("B.java", &[ty("B", "B.java")], &[])
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(&e, &tid("A"), EdgeKind::Extends, &tid("B")));
        e.store.remove_file("B.java").unwrap();
        resolve(&mut e.store).unwrap();
        assert!(!has_edge(&e, &tid("A"), EdgeKind::Extends, &tid("B")));
        assert_eq!(unresolved(&e.store).unwrap().len(), 1);
    }

    #[test]
    fn resolve_is_idempotent_and_returns_counts() {
        let mut e = env(&["A.java", "B.java"]);
        e.store
            .replace_file(
                "A.java",
                &[ty("A", "A.java")],
                &[rf(&tid("A"), EdgeKind::Extends, "B", None)],
            )
            .unwrap();
        e.store
            .replace_file("B.java", &[ty("B", "B.java")], &[])
            .unwrap();
        let s1 = resolve(&mut e.store).unwrap();
        let before = e.store.edges().unwrap().len();
        let s2 = resolve(&mut e.store).unwrap();
        assert_eq!(s1, s2);
        assert_eq!(before, e.store.edges().unwrap().len());
        assert_eq!(s1.edges, before);
    }

    /// REQ-005 AC-005-2 : document -> table | key, d'après les déclarations ; une table partagée
    /// est contenue par chacun de ses fichiers ; le nom d'une clé n'est pas découpé.
    #[test]
    fn contains_document_table_et_key() {
        let mut e = env(&["db/V1.sql", "db/V2.sql", "c.json"]);
        let table = |f: &str| plain(NodeKind::Table, "orders", Some(f));
        let doc = |f: &str| plain(NodeKind::Document, f, Some(f));
        e.store
            .replace_file("db/V1.sql", &[doc("db/V1.sql"), table("db/V1.sql")], &[])
            .unwrap();
        e.store
            .replace_file("db/V2.sql", &[doc("db/V2.sql"), table("db/V2.sql")], &[])
            .unwrap();
        let key = Node::new(
            NodeKind::Key,
            "c.json#a.b#c",
            "a.b#c",
            Some("c.json"),
            None,
            None,
        );
        e.store
            .replace_file("c.json", &[doc("c.json"), key], &[])
            .unwrap();
        resolve(&mut e.store).unwrap();
        let d = |f: &str| node_id(NodeKind::Document, f);
        let t = node_id(NodeKind::Table, "orders");
        assert!(has_edge(&e, &d("db/V1.sql"), EdgeKind::Contains, &t));
        assert!(has_edge(&e, &d("db/V2.sql"), EdgeKind::Contains, &t));
        assert!(has_edge(
            &e,
            &d("c.json"),
            EdgeKind::Contains,
            "key:c.json#a.b#c"
        ));
        assert!(!has_edge(&e, &d("c.json"), EdgeKind::Contains, &t));
    }

    #[test]
    fn contains_package_method_pour_un_module_es() {
        let mut e = env(&["src/a.ts"]);
        e.store
            .replace_file(
                "src/a.ts",
                &[
                    plain(NodeKind::Package, "src/a", Some("src/a.ts")),
                    ty("src/a.S", "src/a.ts"),
                    method("src/a.f", "src/a.ts"),
                    method("src/a.S.m", "src/a.ts"),
                ],
                &[],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        let pk = node_id(NodeKind::Package, "src/a");
        assert!(has_edge(&e, &pk, EdgeKind::Contains, &mid("src/a.f")));
        assert!(has_edge(&e, &pk, EdgeKind::Contains, &tid("src/a.S")));
        // Un parent type l'emporte : la méthode d'un type n'est pas contenue par le package.
        assert!(has_edge(
            &e,
            &tid("src/a.S"),
            EdgeKind::Contains,
            &mid("src/a.S.m")
        ));
        assert!(!has_edge(&e, &pk, EdgeKind::Contains, &mid("src/a.S.m")));
    }

    #[test]
    fn contains_edges_are_derived() {
        let mut e = env(&["S.java", "d.md"]);
        e.store
            .replace_file(
                "S.java",
                &[
                    plain(NodeKind::Package, "com.acme", Some("S.java")),
                    ty("com.acme.Svc", "S.java"),
                    method("com.acme.Svc.place", "S.java"),
                ],
                &[],
            )
            .unwrap();
        e.store
            .replace_file(
                "d.md",
                &[
                    plain(NodeKind::Document, "docs/d.md", Some("d.md")),
                    plain(NodeKind::Section, "docs/d.md#intro", Some("d.md")),
                ],
                &[],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        let pk = node_id(NodeKind::Package, "com.acme");
        assert!(has_edge(&e, &pk, EdgeKind::Contains, &tid("com.acme.Svc")));
        assert!(has_edge(
            &e,
            &tid("com.acme.Svc"),
            EdgeKind::Contains,
            &mid("com.acme.Svc.place")
        ));
        assert!(has_edge(
            &e,
            &node_id(NodeKind::Document, "docs/d.md"),
            EdgeKind::Contains,
            &node_id(NodeKind::Section, "docs/d.md#intro")
        ));
    }

    #[test]
    fn req005_ac5_module_is_nearest_build_ancestor() {
        let mut e = env(&[
            "pom.xml",
            "api/pom.xml",
            "api/src/S.java",
            "core/pom.xml",
            "core/src/C.java",
        ]);
        let m = |q, f| plain(NodeKind::Module, q, Some(f));
        e.store
            .replace_file("pom.xml", &[m("root", "pom.xml")], &[])
            .unwrap();
        e.store
            .replace_file("api/pom.xml", &[m("api", "api/pom.xml")], &[])
            .unwrap();
        e.store
            .replace_file("core/pom.xml", &[m("core", "core/pom.xml")], &[])
            .unwrap();
        e.store
            .replace_file(
                "api/src/S.java",
                &[plain(NodeKind::Package, "com.api", Some("api/src/S.java"))],
                &[],
            )
            .unwrap();
        e.store
            .replace_file(
                "core/src/C.java",
                &[plain(
                    NodeKind::Package,
                    "com.core",
                    Some("core/src/C.java"),
                )],
                &[],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        let md = |q| node_id(NodeKind::Module, q);
        let pk = |q| node_id(NodeKind::Package, q);
        assert!(has_edge(&e, &md("api"), EdgeKind::Contains, &pk("com.api")));
        assert!(has_edge(
            &e,
            &md("core"),
            EdgeKind::Contains,
            &pk("com.core")
        ));
        assert!(!has_edge(
            &e,
            &md("root"),
            EdgeKind::Contains,
            &pk("com.api")
        ));
        assert!(!has_edge(
            &e,
            &md("api"),
            EdgeKind::Contains,
            &pk("com.core")
        ));
    }

    #[test]
    fn req005_ac3_markdown_and_code_in_one_graph() {
        let mut e = env(&["S.java", "d.md"]);
        e.store
            .replace_file("S.java", &[ty("com.acme.OrderService", "S.java")], &[])
            .unwrap();
        let sec = node_id(NodeKind::Section, "docs/d.md#archi");
        e.store
            .replace_file(
                "d.md",
                &[
                    plain(NodeKind::Document, "docs/d.md", Some("d.md")),
                    plain(NodeKind::Section, "docs/d.md#archi", Some("d.md")),
                ],
                &[rf(&sec, EdgeKind::Mentions, "OrderService", None)],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &sec,
            EdgeKind::Mentions,
            &tid("com.acme.OrderService")
        ));
    }

    fn has_node(e: &Env, id: &str) -> bool {
        e.store.nodes().unwrap().iter().any(|n| n.id == id)
    }

    fn build_module(e: &mut Env, file: &str, q: &str, deps: &[&str]) {
        let id = node_id(NodeKind::Module, q);
        let refs: Vec<Ref> = deps
            .iter()
            .map(|d| rf(&id, EdgeKind::DependsOn, d, None))
            .collect();
        e.store
            .replace_file(file, &[plain(NodeKind::Module, q, Some(file))], &refs)
            .unwrap();
    }

    #[test]
    fn req008_ac6_module_and_derived_dependency() {
        let mut e = env(&["api/pom.xml", "core/pom.xml"]);
        let api = node_id(NodeKind::Module, "com.acme:api");
        let slf = node_id(NodeKind::Dependency, "org.slf4j:slf4j-api");
        build_module(
            &mut e,
            "api/pom.xml",
            "com.acme:api",
            &["com.acme:core", "org.slf4j:slf4j-api"],
        );
        build_module(&mut e, "core/pom.xml", "com.acme:core", &[]);
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 0);
        assert!(unresolved(&e.store).unwrap().is_empty());
        let core = node_id(NodeKind::Module, "com.acme:core");
        assert!(has_edge(&e, &api, EdgeKind::DependsOn, &core));
        assert!(has_edge(&e, &api, EdgeKind::DependsOn, &slf));
        assert!(!has_node(
            &e,
            &node_id(NodeKind::Dependency, "com.acme:core")
        ));
        let dep = e.store.nodes().unwrap().into_iter().find(|n| n.id == slf);
        let dep = dep.unwrap();
        assert_eq!((dep.file, dep.short.as_str()), (None, "slf4j-api"));
        resolve(&mut e.store).unwrap();
        assert!(has_node(&e, &slf));
        // La dépendance est retirée du pom : le nœud disparaît.
        build_module(&mut e, "api/pom.xml", "com.acme:api", &["com.acme:core"]);
        resolve(&mut e.store).unwrap();
        assert!(!has_node(&e, &slf));
        assert!(has_edge(&e, &api, EdgeKind::DependsOn, &core));
    }

    #[test]
    fn dependency_node_is_shared_and_kept_while_referenced() {
        let mut e = env(&["a/pom.xml", "b/pom.xml"]);
        build_module(&mut e, "a/pom.xml", "g:a", &["org.x:lib"]);
        build_module(&mut e, "b/pom.xml", "g:b", &["org.x:lib"]);
        resolve(&mut e.store).unwrap();
        let count = |e: &Env| {
            e.store
                .nodes()
                .unwrap()
                .iter()
                .filter(|n| n.kind == NodeKind::Dependency)
                .count()
        };
        assert_eq!(count(&e), 1);
        build_module(&mut e, "a/pom.xml", "g:a", &[]);
        resolve(&mut e.store).unwrap();
        assert_eq!(count(&e), 1);
        build_module(&mut e, "b/pom.xml", "g:b", &[]);
        resolve(&mut e.store).unwrap();
        assert_eq!(count(&e), 0);
    }

    #[test]
    fn unrelated_replace_keeps_derived_dependency_and_link() {
        let mut e = env(&["a/pom.xml", "b/pom.xml"]);
        build_module(&mut e, "a/pom.xml", "g:a", &["org.x:lib"]);
        resolve(&mut e.store).unwrap();
        let a = node_id(NodeKind::Module, "g:a");
        let dep = node_id(NodeKind::Dependency, "org.x:lib");
        build_module(&mut e, "b/pom.xml", "g:b", &[]);
        e.store.replace_file("b/pom.xml", &[], &[]).unwrap();
        assert!(has_node(&e, &dep));
        assert!(has_edge(&e, &a, EdgeKind::DependsOn, &dep));
    }

    #[test]
    fn blank_depends_on_target_is_ignored() {
        let mut e = env(&["a/pom.xml"]);
        build_module(&mut e, "a/pom.xml", "g:a", &["", "  "]);
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 0);
        assert!(e.store.edges().unwrap().is_empty());
        assert!(!e
            .store
            .nodes()
            .unwrap()
            .iter()
            .any(|n| n.kind == NodeKind::Dependency));
    }

    #[test]
    fn module_wins_over_dependency_with_same_coordinates() {
        let mut e = env(&["api/pom.xml", "core/pom.xml"]);
        build_module(&mut e, "api/pom.xml", "com.acme:api", &["com.acme:core"]);
        resolve(&mut e.store).unwrap();
        let dep = node_id(NodeKind::Dependency, "com.acme:core");
        assert!(has_node(&e, &dep));
        // Le module du dépôt apparaît : le lien bascule vers lui, la dépendance disparaît.
        build_module(&mut e, "core/pom.xml", "com.acme:core", &[]);
        resolve(&mut e.store).unwrap();
        let api = node_id(NodeKind::Module, "com.acme:api");
        assert!(!has_node(&e, &dep));
        assert!(has_edge(
            &e,
            &api,
            EdgeKind::DependsOn,
            &node_id(NodeKind::Module, "com.acme:core")
        ));
        assert!(!has_edge(&e, &api, EdgeKind::DependsOn, &dep));
    }

    #[test]
    fn gradle_project_path_resolves_to_module() {
        let mut e = env(&["api/build.gradle", "core/build.gradle"]);
        build_module(&mut e, "api/build.gradle", ":api", &[":core", "org.x:lib"]);
        build_module(&mut e, "core/build.gradle", ":core", &[]);
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 0);
        let api = node_id(NodeKind::Module, ":api");
        assert!(has_edge(
            &e,
            &api,
            EdgeKind::DependsOn,
            &node_id(NodeKind::Module, ":core")
        ));
        assert!(has_edge(
            &e,
            &api,
            EdgeKind::DependsOn,
            &node_id(NodeKind::Dependency, "org.x:lib")
        ));
    }

    #[test]
    fn req008_ac7_parse_mentions_not_unresolved_and_repaired() {
        let mut e = env(&["d.md", "I.java"]);
        let sec = node_id(NodeKind::Section, "docs/d.md#a");
        e.store
            .replace_file(
                "d.md",
                &[
                    plain(NodeKind::Document, "docs/d.md", Some("d.md")),
                    plain(NodeKind::Section, "docs/d.md#a", Some("d.md")),
                ],
                &[
                    rf(&sec, EdgeKind::Mentions, "Inexistant", None),
                    rf(&sec, EdgeKind::Mentions, "config.toml", None),
                ],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 0);
        assert!(unresolved(&e.store).unwrap().is_empty());
        assert_eq!(e.store.refs().unwrap().len(), 2, "mentions conservées");
        // La classe apparaît, le .md n'est pas touché : le lien existe.
        e.store
            .replace_file("I.java", &[ty("Inexistant", "I.java")], &[])
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(&e, &sec, EdgeKind::Mentions, &tid("Inexistant")));
    }

    #[test]
    fn llm_mentions_follow_normal_rules() {
        let mut e = env(&["d.md"]);
        let sec = node_id(NodeKind::Section, "docs/d.md#a");
        e.store
            .replace_file(
                "d.md",
                &[
                    plain(NodeKind::Document, "docs/d.md", Some("d.md")),
                    plain(NodeKind::Section, "docs/d.md#a", Some("d.md")),
                ],
                &[],
            )
            .unwrap();
        e.store
            .apply_enrichment(
                "d.md",
                "h",
                &[],
                &[],
                &[rf(&sec, EdgeKind::Mentions, "concept:Inconnu", None)],
            )
            .unwrap();
        let st = resolve(&mut e.store).unwrap();
        assert_eq!(st.unresolved, 1);
        assert_eq!(unresolved(&e.store).unwrap().len(), 1);
    }

    fn module_package_edge(build_file: &str, module_q: &str, src: &str, pkg: &str) {
        let mut e = env(&[build_file, src]);
        e.store
            .replace_file(
                build_file,
                &[plain(NodeKind::Module, module_q, Some(build_file))],
                &[],
            )
            .unwrap();
        e.store
            .replace_file(src, &[plain(NodeKind::Package, pkg, Some(src))], &[])
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &node_id(NodeKind::Module, module_q),
            EdgeKind::Contains,
            &node_id(NodeKind::Package, pkg)
        ));
    }

    #[test]
    fn req022_ac8_module_contains_package_by_folder_maven() {
        module_package_edge(
            "api/pom.xml",
            "com.acme:api",
            "api/src/main/java/com/acme/api/A.java",
            "com.acme.api",
        );
    }

    #[test]
    fn req022_ac8_module_contains_package_by_folder_gradle() {
        module_package_edge(
            "api/build.gradle",
            ":api",
            "api/src/main/java/com/acme/api/A.java",
            "com.acme.api",
        );
        module_package_edge(
            "services/api/build.gradle.kts",
            ":services:api",
            "services/api/src/main/kotlin/p/A.kt",
            "p",
        );
    }
}
