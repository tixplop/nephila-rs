//! Résolution des liens sans LLM (REQ-008) et liens `contains` dérivés (REQ-005).
//!
//! Deux passes au même résultat (DEC-077) : la passe complète est une fonction pure de `nodes`,
//! `refs` et `declarations` qui recalcule tous les liens en mémoire ; la passe incrémentale ne
//! réévalue que les sources et les cibles que le journal des changements du stockage désigne.
//! Toutes deux écrivent leurs différences, le statut des références et le résumé en une
//! transaction.
//!
//! Le contexte d'une référence est un texte libre dont les lignes (ou segments séparés par `;`)
//! `package a.b` et `import a.b.C` / `import a.b.*` sont reconnues.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::error::Result;
use crate::model::{node_id, parse_node_id, Edge, EdgeKind, Node, NodeKind, Ref, RefOrigin};
use crate::scan::module_of;
use crate::store::{ChangeLog, FileRef, IncrementalWrite, Store};

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
    /// Indices (dans `refs`) des références non résolues : aucune copie de contexte (REQ-063).
    pub unresolved: Vec<usize>,
    /// Nœuds `dependency` dérivés des références depends_on sans module du dépôt (DEC-020).
    pub dependencies: Vec<Node>,
}

/// Résout les liens du graphe (DEC-077 point 2) : passe incrémentale quand le stockage tient un
/// journal des changements sans nœud `module`, passe complète sinon (première passe d'un
/// `build`, graphe non marqué `à jour`, module inséré, supprimé ou modifié).
pub fn resolve(store: &mut Store) -> Result<ResolveStats> {
    match store.change_log() {
        Some(log) if !touches_module(log) => {
            let log = log.clone();
            resolve_incremental(store, &log)
        }
        _ => resolve_full(store),
    }
}

fn touches_module(log: &ChangeLog) -> bool {
    let prefix = format!("{}:", NodeKind::Module.as_str());
    log.inserted
        .iter()
        .chain(&log.deleted)
        .chain(&log.declarations_changed)
        .any(|id| id.starts_with(&prefix))
}

/// Passe complète : recalcule tous les liens du graphe à partir des nœuds et des références.
fn resolve_full(store: &mut Store) -> Result<ResolveStats> {
    crate::store::note_pass(false);
    let refs = store.file_refs()?;
    crate::store::note_refs_reevaluated(refs.len());
    let r = compute(
        &store.nodes()?,
        &refs,
        &store.file_contexts()?,
        &store.declarations()?,
    );
    let unresolved: Vec<i64> = r.unresolved.iter().map(|&i| refs[i].id).collect();
    // Dépendances dérivées, liens, statut des références, résumé global et marqueur : une seule
    // transaction (DEC-074, DEC-076).
    store.write_resolution(
        &r.dependencies,
        &r.edges,
        &unresolved,
        crate::summary::compute_stamped,
    )?;
    // Les liens sont dédupliqués par la clé primaire : on compte ce qui est stocké.
    Ok(ResolveStats {
        edges: store.count_edges()?,
        unresolved: store.count_unresolved()?,
    })
}

/// Passe incrémentale (DEC-077 point 3) : ne réévalue que les références des sources concernées
/// par le journal et les liens `contains` des cibles concernées ; même résultat que la passe
/// complète sur le même état.
fn resolve_incremental(store: &mut Store, log: &ChangeLog) -> Result<ResolveStats> {
    crate::store::note_pass(true);
    let nodes = store.resolution_nodes()?;
    let idx = Index::new(&nodes);
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let by_id: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let moved: BTreeSet<&str> = log
        .inserted
        .iter()
        .chain(&log.deleted)
        .map(String::as_str)
        .collect();

    // Sources réévaluées : fichiers dont les références ont changé, sources retirées, et
    // références dont la clé ou la tête du nom visé est la clé d'un nœud inséré ou supprimé.
    let mut sources: BTreeSet<String> = log.removed_sources.clone();
    for f in &log.files {
        sources.extend(store.ref_sources_of_file(f)?);
    }
    let keys: BTreeSet<&str> = moved
        .iter()
        .filter_map(|id| parse_node_id(id).ok())
        .map(|(_, q)| last_segment(q))
        .collect();
    for k in keys {
        sources.extend(store.ref_sources_by_key(k, false)?);
        sources.extend(store.ref_sources_by_key(k, true)?);
    }
    sources.retain(|s| ids.contains(s.as_str()));

    let mut write = IncrementalWrite::default();
    let empty = FileContext::default();
    let mut contexts: HashMap<String, FileContext> = HashMap::new();
    let mut dependencies: HashMap<String, Node> = HashMap::new();
    let mut reevaluated = 0;
    for source in &sources {
        let refs = store.refs_of_source(source)?;
        reevaluated += refs.len();
        let mut wanted: Vec<Edge> = Vec::new();
        for (fr, was_unresolved) in &refs {
            if fr.origin == RefOrigin::Parse && !contexts.contains_key(&fr.file) {
                let ctx = parse_context(store.file_context(&fr.file)?.as_deref());
                contexts.insert(fr.file.clone(), ctx);
            }
            let ctx = if fr.origin == RefOrigin::Parse {
                &contexts[&fr.file]
            } else {
                &empty
            };
            let mut unresolved = false;
            match eval_ref(fr, ctx, &ids, &idx) {
                Outcome::Ignored => {}
                Outcome::Unresolved => unresolved = true,
                Outcome::Linked(e) => wanted.push(e),
                Outcome::Depends(e, dep) => {
                    if let Some(d) = dep {
                        dependencies.entry(d.id.clone()).or_insert(d);
                    }
                    wanted.push(e);
                }
            }
            if unresolved != *was_unresolved {
                write.statuses.push((fr.id, unresolved));
            }
        }
        write.sources.push((source.clone(), wanted));
    }
    crate::store::note_refs_reevaluated(reevaluated);
    write.dependencies = dependencies.into_values().collect();
    write.dependencies.sort_by(|a, b| a.id.cmp(&b.id));

    // Cibles des liens `contains` réévaluées : nœuds insérés, supprimés ou aux déclarations
    // changées, et enfants (par la règle de `incoming_contains`) des nœuds insérés ou supprimés.
    let mut targets: BTreeSet<&str> = log
        .inserted
        .iter()
        .chain(&log.deleted)
        .chain(&log.declarations_changed)
        .map(String::as_str)
        .filter(|id| ids.contains(id))
        .collect();
    let parents: HashSet<&str> = moved
        .iter()
        .filter_map(|id| parse_node_id(id).ok())
        .map(|(_, q)| q)
        .collect();
    for n in &nodes {
        let parent = match n.kind {
            NodeKind::Type | NodeKind::Method => parent_of(&n.qualified),
            NodeKind::Section => n.qualified.split_once('#').map(|(d, _)| d),
            _ => None,
        };
        if parent.is_some_and(|p| parents.contains(p)) {
            targets.insert(&n.id);
        }
    }
    for id in &moved {
        if let Ok((NodeKind::Document, file)) = parse_node_id(id) {
            for node in store.declared_nodes(file)? {
                if by_id
                    .get(node.as_str())
                    .is_some_and(|n| matches!(n.kind, NodeKind::Table | NodeKind::Key))
                {
                    targets.insert(by_id[node.as_str()].id.as_str());
                }
            }
        }
    }
    let dirs = ModuleDirs::new(&nodes);
    for id in targets {
        let n = by_id[id];
        let declaring = if matches!(n.kind, NodeKind::Package | NodeKind::Table | NodeKind::Key) {
            store.declaring_files(&n.id)?
        } else {
            Vec::new()
        };
        let declaring: Vec<&str> = declaring.iter().map(String::as_str).collect();
        let mut wanted = Vec::new();
        incoming_contains(n, &declaring, &idx, &dirs, &mut wanted);
        write.contained.push((n.id.clone(), wanted));
    }

    store.write_incremental(&write, crate::summary::compute_stamped)?;
    Ok(ResolveStats {
        edges: store.count_edges()?,
        unresolved: store.count_unresolved()?,
    })
}

/// Références qui ne se résolvent vers aucun nœud.
pub fn unresolved(store: &Store) -> Result<Vec<Ref>> {
    let refs = store.file_refs()?;
    let contexts = store.file_contexts()?;
    let res = compute(&store.nodes()?, &refs, &contexts, &store.declarations()?);
    Ok(materialize_unresolved(&res, &refs, &contexts))
}

/// Rattache à chaque non résolue le contexte de son fichier (références d'analyse seulement).
fn materialize_unresolved(
    res: &Resolution,
    refs: &[FileRef],
    contexts: &HashMap<String, String>,
) -> Vec<Ref> {
    res.unresolved
        .iter()
        .map(|&i| {
            let fr = &refs[i];
            Ref {
                context: if fr.origin == RefOrigin::Parse {
                    contexts.get(&fr.file).cloned()
                } else {
                    None
                },
                ..fr.r.clone()
            }
        })
        .collect()
}

#[cfg(test)]
fn compute_from(store: &Store) -> Result<Resolution> {
    Ok(compute(
        &store.nodes()?,
        &store.file_refs()?,
        &store.file_contexts()?,
        &store.declarations()?,
    ))
}

#[derive(Default)]
struct FileContext {
    package: Option<String>,
    imports: Vec<String>,
}

#[cfg(test)]
thread_local! {
    /// Compteur de test des appels de `parse_context` (AC-063-4).
    static PARSE_CONTEXT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn parse_context(ctx: Option<&str>) -> FileContext {
    #[cfg(test)]
    PARSE_CONTEXT_CALLS.with(|c| c.set(c.get() + 1));
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

/// Clé et tête d'un nom visé par une référence (DEC-076), seule définition : la clé est le
/// dernier segment du nom (séparateurs de `SEPARATORS`), la tête le texte qui précède le premier
/// `.` quand le nom en contient un. Le nom est d'abord débarrassé de ses espaces de bord.
pub fn name_keys(name: &str) -> (String, Option<String>) {
    let name = name.trim();
    let head = name.find('.').map(|i| name[..i].to_string());
    (last_segment(name).to_string(), head)
}

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
fn resolve_ref(
    r: &Ref,
    ctx: &FileContext,
    source_kind: NodeKind,
    source_q: &str,
    idx: &Index,
) -> Option<String> {
    let ok = |n: &Node| r.kind.allows(source_kind, n.kind);
    let single = |v: Vec<&Node>| -> Option<Option<String>> {
        match v.len() {
            0 => None,
            1 => Some(Some(v[0].id.clone())),
            _ => Some(None),
        }
    };
    let name = r.target_name.trim();

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

    // Membre d'un type importé (`T.m`) que les imports n'ont pas résolu : si le nom du type
    // désigne plusieurs types du dépôt, le receveur est ambigu, donc la référence reste non
    // résolue (un lien manquant plutôt qu'un lien faux, DEC-021).
    if !rest.is_empty()
        && ctx
            .imports
            .iter()
            .any(|i| !i.ends_with(".*") && last_segment(i) == first)
        && idx
            .suffix(first, &|n: &Node| n.kind == NodeKind::Type)
            .len()
            > 1
    {
        return None;
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

/// Issue de l'évaluation d'une référence : seule définition, partagée par les deux passes.
enum Outcome {
    /// Ni lien ni non résolue (source absente, `depends_on` vide, mention d'analyse libre).
    Ignored,
    Unresolved,
    Linked(Edge),
    /// `depends_on` : lien, et nœud `dependency` à dériver quand la cible n'est pas un module.
    Depends(Edge, Option<Node>),
}

/// Évalue une référence : `ctx` est le contexte de son fichier (vide pour une référence `llm`),
/// `ids` les identifiants des nœuds, `idx` l'index de résolution.
fn eval_ref(fr: &FileRef, ctx: &FileContext, ids: &HashSet<&str>, idx: &Index) -> Outcome {
    let r = &fr.r;
    let Ok((skind, sq)) = parse_node_id(&r.source) else {
        return Outcome::Unresolved;
    };
    if !ids.contains(r.source.as_str()) {
        return Outcome::Ignored;
    }
    // depends_on vise des coordonnées : module du dépôt, sinon dépendance dérivée (DEC-020).
    if r.kind == EdgeKind::DependsOn && skind == NodeKind::Module {
        let name = r.target_name.trim();
        if name.is_empty() {
            return Outcome::Ignored;
        }
        let module = node_id(NodeKind::Module, name);
        let (target, dep) = if ids.contains(module.as_str()) {
            (module, None)
        } else {
            let dep = Node::new(
                NodeKind::Dependency,
                name,
                name.rsplit(':').next().unwrap_or(name),
                None,
                None,
                None,
            );
            (dep.id.clone(), Some(dep))
        };
        let edge = Edge {
            source: r.source.clone(),
            target,
            kind: r.kind,
        };
        return Outcome::Depends(edge, dep);
    }
    match resolve_ref(r, ctx, skind, sq, idx) {
        Some(target) => Outcome::Linked(Edge {
            source: r.source.clone(),
            target,
            kind: r.kind,
        }),
        // Une mention issue de l'analyse syntaxique non résolue est normale (texte libre).
        None if r.kind == EdgeKind::Mentions && fr.origin == RefOrigin::Parse => Outcome::Ignored,
        None => Outcome::Unresolved,
    }
}

/// Calcule les liens résolus, les liens `contains` dérivés et les références non résolues.
/// `contexts` donne le texte d'imports de chaque fichier ; il est analysé une fois par fichier
/// (REQ-063). Les non résolues sont rendues par indice dans `refs`, sans copie de contexte.
pub fn compute(
    nodes: &[Node],
    refs: &[FileRef],
    contexts: &HashMap<String, String>,
    declarations: &[(String, String)],
) -> Resolution {
    let idx = Index::new(nodes);
    let parsed: HashMap<&str, FileContext> = contexts
        .iter()
        .map(|(f, c)| (f.as_str(), parse_context(Some(c))))
        .collect();
    let empty = FileContext::default();
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut out = Resolution::default();

    let mut dependencies: HashMap<String, Node> = HashMap::new();
    for (i, fr) in refs.iter().enumerate() {
        let ctx = if fr.origin == RefOrigin::Parse {
            parsed.get(fr.file.as_str()).unwrap_or(&empty)
        } else {
            &empty
        };
        match eval_ref(fr, ctx, &ids, &idx) {
            Outcome::Ignored => {}
            Outcome::Unresolved => out.unresolved.push(i),
            Outcome::Linked(e) => out.edges.push(e),
            Outcome::Depends(e, dep) => {
                if let Some(d) = dep {
                    dependencies.entry(d.id.clone()).or_insert(d);
                }
                out.edges.push(e);
            }
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

/// Dossiers des modules du dépôt, par écosystème : modules déclarés par un `package.json`
/// (modules ES) et par un fichier de build JVM (le reste), chacun rattaché à son fichier de
/// build le plus proche.
struct ModuleDirs<'a> {
    jvm_set: HashSet<String>,
    npm_set: HashSet<String>,
    jvm: HashMap<String, &'a Node>,
    npm: HashMap<String, &'a Node>,
}

impl<'a> ModuleDirs<'a> {
    fn new(nodes: &'a [Node]) -> Self {
        let mut jvm: HashMap<String, &Node> = HashMap::new();
        let mut npm: HashMap<String, &Node> = HashMap::new();
        for n in nodes.iter().filter(|n| n.kind == NodeKind::Module) {
            if let Some(f) = &n.file {
                let (dir, name) = f.rsplit_once('/').unwrap_or(("", f.as_str()));
                let table = if name == "package.json" {
                    &mut npm
                } else {
                    &mut jvm
                };
                table.entry(dir.to_string()).or_insert(n);
            }
        }
        ModuleDirs {
            jvm_set: jvm.keys().cloned().collect(),
            npm_set: npm.keys().cloned().collect(),
            jvm,
            npm,
        }
    }
}

/// Liens `contains` entrants du nœud `n` (REQ-005), seule définition de la règle, utilisée par
/// les deux passes. `declaring` : fichiers qui déclarent `n` (utile aux packages, tables et clés).
fn incoming_contains(
    n: &Node,
    declaring: &[&str],
    idx: &Index,
    dirs: &ModuleDirs,
    edges: &mut Vec<Edge>,
) {
    let is_kind = |q: &str, k: NodeKind| {
        idx.by_qualified
            .get(q)
            .is_some_and(|v| v.iter().any(|n| n.kind == k))
    };
    let mut push = |from: NodeKind, fq: &str| {
        edges.push(Edge {
            source: node_id(from, fq),
            target: n.id.clone(),
            kind: EdgeKind::Contains,
        });
    };
    match n.kind {
        NodeKind::Type => {
            if let Some(p) = parent_of(&n.qualified).filter(|p| is_kind(p, NodeKind::Package)) {
                push(NodeKind::Package, p);
            }
        }
        NodeKind::Method => {
            // Un parent type l'emporte ; sinon un package (fonction de module ES, DEC-040).
            if let Some(parent) = parent_of(&n.qualified) {
                if is_kind(parent, NodeKind::Type) {
                    push(NodeKind::Type, parent);
                } else if is_kind(parent, NodeKind::Package) {
                    push(NodeKind::Package, parent);
                }
            }
        }
        NodeKind::Section => {
            if let Some((d, _)) = n.qualified.split_once('#') {
                if is_kind(d, NodeKind::Document) {
                    push(NodeKind::Document, d);
                }
            }
        }
        // Document de données -> table | clé : le fichier déclarant est le document (son nom
        // qualifié est son chemin) ; une table partagée est contenue par chacun de ses fichiers.
        // Le nom qualifié d'une clé n'est pas découpé (un nom de membre peut contenir `#`).
        NodeKind::Table | NodeKind::Key => {
            for file in declaring {
                if is_kind(file, NodeKind::Document) {
                    push(NodeKind::Document, file);
                }
            }
        }
        // Module -> package : module du fichier déclarant le package (AC-005-5).
        NodeKind::Package => {
            for file in declaring {
                let js = crate::parse::Lang::from_path(std::path::Path::new(file))
                    .is_some_and(|l| l.is_js_ts());
                let (set, table) = if js {
                    (&dirs.npm_set, &dirs.npm)
                } else {
                    (&dirs.jvm_set, &dirs.jvm)
                };
                if let Some(m) = module_of(file, set).and_then(|d| table.get(&d)) {
                    push(NodeKind::Module, &m.qualified);
                }
            }
        }
        _ => {}
    }
}

fn derive_contains(
    nodes: &[Node],
    declarations: &[(String, String)],
    idx: &Index,
    edges: &mut Vec<Edge>,
) {
    let mut declared_by: HashMap<&str, Vec<&str>> = HashMap::new();
    for (file, node) in declarations {
        declared_by.entry(node).or_default().push(file);
    }
    let dirs = ModuleDirs::new(nodes);
    for n in nodes {
        let declaring = declared_by
            .get(n.id.as_str())
            .map_or(&[][..], Vec::as_slice);
        incoming_contains(n, declaring, idx, &dirs, edges);
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
    fn req005_ac5_modules_par_ecosysteme() {
        let mut e = env(&["pom.xml", "package.json", "src/A.java", "web/src/main.ts"]);
        let m = |q, f| plain(NodeKind::Module, q, Some(f));
        e.store
            .replace_file("pom.xml", &[m("com.acme:app", "pom.xml")], &[])
            .unwrap();
        e.store
            .replace_file("package.json", &[m("app-front", "package.json")], &[])
            .unwrap();
        e.store
            .replace_file(
                "src/A.java",
                &[plain(NodeKind::Package, "com.acme", Some("src/A.java"))],
                &[],
            )
            .unwrap();
        e.store
            .replace_file(
                "web/src/main.ts",
                &[plain(
                    NodeKind::Package,
                    "web/src/main",
                    Some("web/src/main.ts"),
                )],
                &[],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        let md = |q| node_id(NodeKind::Module, q);
        let pk = |q| node_id(NodeKind::Package, q);
        let contains: Vec<_> = e
            .store
            .edges()
            .unwrap()
            .into_iter()
            .filter(|x| x.kind == EdgeKind::Contains && x.source.starts_with("module:"))
            .collect();
        assert_eq!(contains.len(), 2, "{contains:?}");
        assert!(has_edge(
            &e,
            &md("com.acme:app"),
            EdgeKind::Contains,
            &pk("com.acme")
        ));
        assert!(has_edge(
            &e,
            &md("app-front"),
            EdgeKind::Contains,
            &pk("web/src/main")
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
                "m",
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

    fn calls_target(
        source: Node,
        ctx: &str,
        target: &str,
        others: Vec<Node>,
    ) -> (Resolution, String) {
        let src_id = source.id.clone();
        let mut nodes = vec![source];
        nodes.extend(others);
        let refs = vec![FileRef {
            id: 1,
            r: rf(&src_id, EdgeKind::Calls, target, None),
            origin: RefOrigin::Parse,
            file: "S".into(),
        }];
        let contexts = HashMap::from([("S".to_string(), ctx.to_string())]);
        (compute(&nodes, &refs, &contexts, &[]), src_id)
    }
    fn calls_of(r: &Resolution, src: &str) -> Vec<String> {
        r.edges
            .iter()
            .filter(|e| e.source == src && e.kind == EdgeKind::Calls)
            .map(|e| e.target.clone())
            .collect()
    }

    fn js_nodes(with_other: bool) -> (Node, Vec<Node>) {
        let mut v = vec![
            ty("src/services/x.X", "src/services/x.ts"),
            method("src/services/x.X.run", "src/services/x.ts"),
        ];
        if with_other {
            v.push(ty("src/other/x.X", "src/other/x.ts"));
        }
        (method("src/user.f", "src/user.ts"), v)
    }
    const JS_CTX: &str = "package src/user\nimport src/services.X\nimport src/services/index.X";

    #[test]
    fn req008_ac10_js_ambiguous_imported_receiver_unresolved() {
        let (src, others) = js_nodes(true);
        let (r, id) = calls_target(src, JS_CTX, "X.run", others);
        assert!(calls_of(&r, &id).is_empty());
        assert_eq!(r.unresolved.len(), 1);
    }

    #[test]
    fn req008_ac10_js_unique_imported_receiver_resolved() {
        let (src, others) = js_nodes(false);
        let (r, id) = calls_target(src, JS_CTX, "X.run", others);
        assert_eq!(calls_of(&r, &id), vec![mid("src/services/x.X.run")]);
    }

    fn jvm_nodes() -> Vec<Node> {
        vec![
            ty("com.a.Foo", "a/Foo.java"),
            method("com.a.Foo.bar", "a/Foo.java"),
            ty("com.b.Foo", "b/Foo.java"),
            method("com.b.Foo.bar", "b/Foo.java"),
        ]
    }

    #[test]
    fn req008_ac10_jvm_wildcard_import_keeps_same_package() {
        let (r, id) = calls_target(
            method("com.a.User.m", "a/User.java"),
            "package com.a\nimport java.util.*",
            "Foo.bar",
            jvm_nodes(),
        );
        assert_eq!(calls_of(&r, &id), vec![mid("com.a.Foo.bar")]);
    }

    #[test]
    fn req008_ac5_explicit_import_resolves_despite_namesake_member() {
        let (r, id) = calls_target(
            method("com.a.User.m", "a/User.java"),
            "package com.a\nimport com.b.Foo",
            "Foo.bar",
            jvm_nodes(),
        );
        assert_eq!(calls_of(&r, &id), vec![mid("com.b.Foo.bar")]);
    }

    /// Oracle : `parse_context`, `resolve_ref` et `compute` de la 0.20.0 (contexte par référence).
    mod oracle {
        use super::super::*;

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

        fn resolve_ref(
            r: &Ref,
            source_kind: NodeKind,
            source_q: &str,
            idx: &Index,
        ) -> Option<String> {
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
                    ctx.imports.iter().any(|i| {
                        !i.ends_with(".*") && (**c == *i || c.starts_with(&format!("{i}.")))
                    })
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

            // Membre d'un type importé (`T.m`) que les imports n'ont pas résolu : si le nom du type
            // désigne plusieurs types du dépôt, le receveur est ambigu, donc la référence reste non
            // résolue (un lien manquant plutôt qu'un lien faux, DEC-021).
            if !rest.is_empty()
                && ctx
                    .imports
                    .iter()
                    .any(|i| !i.ends_with(".*") && last_segment(i) == first)
                && idx
                    .suffix(first, &|n: &Node| n.kind == NodeKind::Type)
                    .len()
                    > 1
            {
                return None;
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

        #[derive(Default)]
        pub struct OracleResolution {
            pub edges: Vec<Edge>,
            pub unresolved: Vec<Ref>,
            pub dependencies: Vec<Node>,
        }

        pub fn compute(
            nodes: &[Node],
            refs: &[(Ref, RefOrigin)],
            declarations: &[(String, String)],
        ) -> OracleResolution {
            let idx = Index::new(nodes);
            let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
            let mut out = OracleResolution::default();

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
    }

    /// Générateur congruentiel à graine fixe (sans dépendance).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
        fn pick<'a>(&mut self, v: &'a [&'a str]) -> &'a str {
            v[self.next(v.len())]
        }
    }

    const PKGS: [&str; 4] = ["p0", "p1", "p2", "p3"];
    const NAMES: [&str; 5] = ["A", "B", "C", "D", "E"];

    fn random_context(g: &mut Lcg) -> Option<String> {
        if g.next(5) == 0 {
            return None;
        }
        let mut parts = Vec::new();
        if g.next(4) != 0 {
            parts.push(format!("package {}", g.pick(&PKGS)));
        }
        for _ in 0..g.next(4) {
            let pkg = g.pick(&PKGS);
            let name = g.pick(&NAMES);
            parts.push(match g.next(4) {
                0 => format!("import {pkg}.*"),
                1 => format!("import static {pkg}.{name}.m1"),
                _ => format!("import {pkg}.{name}"),
            });
        }
        let sep = if g.next(2) == 0 {
            "
"
        } else {
            ";"
        };
        Some(parts.join(sep))
    }

    fn random_env(seed: u64) -> Env {
        let mut g = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(7));
        let nfiles = 3 + g.next(6);
        let exts = ["java", "kt", "ts"];
        let files: Vec<String> = (0..nfiles)
            .map(|i| format!("f{i}.{}", exts[g.next(3)]))
            .collect();
        let mut all: Vec<&str> = files.iter().map(String::as_str).collect();
        all.push("pom.xml");
        let mut e = env(&all);
        let targets = [
            "A", "B", "C", "D", "E", "A.m1", "B.m2", "p0.A", "p1.B.m1", "p2.C", "p3.D.m2", "m1",
            "Zzz", "E.m1",
        ];
        for f in &files {
            let mut nodes = Vec::new();
            let mut sources = Vec::new();
            for _ in 0..1 + g.next(3) {
                let q = format!("{}.{}", g.pick(&PKGS), g.pick(&NAMES));
                nodes.push(ty(&q, f));
                sources.push(tid(&q));
                for j in 0..g.next(3) {
                    let mq = format!("{q}.m{}", j + 1);
                    nodes.push(method(&mq, f));
                    sources.push(mid(&mq));
                }
            }
            let ctx = random_context(&mut g);
            let kinds = [EdgeKind::Calls, EdgeKind::Imports, EdgeKind::Extends];
            let refs: Vec<Ref> = (0..g.next(8))
                .map(|_| {
                    let s = sources[g.next(sources.len())].clone();
                    rf(&s, kinds[g.next(3)], g.pick(&targets), ctx.as_deref())
                })
                .collect();
            e.store.replace_file(f, &nodes, &refs).unwrap();
            if g.next(3) == 0 {
                let llm: Vec<Ref> = (0..1 + g.next(2))
                    .map(|_| {
                        let s = sources[g.next(sources.len())].clone();
                        rf(&s, EdgeKind::Calls, g.pick(&targets), None)
                    })
                    .collect();
                e.store
                    .apply_enrichment(f, "h", "m", &[], &[], &llm)
                    .unwrap();
            }
        }
        e.store
            .replace_file(
                "pom.xml",
                &[plain(NodeKind::Module, "m", Some("pom.xml"))],
                &[rf(
                    &node_id(NodeKind::Module, "m"),
                    EdgeKind::DependsOn,
                    "org.x:lib",
                    None,
                )],
            )
            .unwrap();
        e
    }

    #[test]
    fn req063_ac3_same_result_as_per_reference_context_oracle() {
        let mut with_ctx = 0;
        for seed in 0..200 {
            let e = random_env(seed);
            let refs = e.store.refs_with_origin().unwrap();
            if refs.iter().any(|(r, _)| r.context.is_some()) {
                with_ctx += 1;
            }
            let want = oracle::compute(
                &e.store.nodes().unwrap(),
                &refs,
                &e.store.declarations().unwrap(),
            );
            let got = compute_from(&e.store).unwrap();
            let got_unresolved = materialize_unresolved(
                &got,
                &e.store.file_refs().unwrap(),
                &e.store.file_contexts().unwrap(),
            );
            assert_eq!(
                format!("{:?}", got.edges),
                format!("{:?}", want.edges),
                "{seed}"
            );
            assert_eq!(got_unresolved, want.unresolved, "graine {seed}");
            assert_eq!(
                format!("{:?}", got.dependencies),
                format!("{:?}", want.dependencies),
                "{seed}"
            );
            assert_eq!(unresolved(&e.store).unwrap(), want.unresolved, "{seed}");
        }
        assert!(with_ctx > 100, "générateur trop pauvre : {with_ctx}");
    }

    #[test]
    fn req063_ac4_context_parsed_once_per_file() {
        let mut e = env(&[
            "a1.java", "a2.java", "a3.java", "a4.java", "a5.java", "b1.md", "b2.md",
        ]);
        let sizes = [1usize, 7, 40, 3, 12];
        for (i, n) in sizes.iter().enumerate() {
            let f = format!("a{}.java", i + 1);
            let q = format!("p.T{i}");
            let refs: Vec<Ref> = (0..*n)
                .map(|k| {
                    rf(
                        &tid(&q),
                        EdgeKind::Imports,
                        &format!("X{k}"),
                        Some(
                            "package p;
import q.R;",
                        ),
                    )
                })
                .collect();
            e.store.replace_file(&f, &[ty(&q, &f)], &refs).unwrap();
        }
        for f in ["b1.md", "b2.md"] {
            let q = format!("p.{f}");
            e.store
                .replace_file(
                    f,
                    &[ty(&q, f)],
                    &[rf(&tid(&q), EdgeKind::Imports, "Y", None)],
                )
                .unwrap();
        }
        PARSE_CONTEXT_CALLS.with(|c| c.set(0));
        resolve(&mut e.store).unwrap();
        assert_eq!(PARSE_CONTEXT_CALLS.with(|c| c.get()), 5);
    }

    /// Séquence de la 0.20.0 : trois écritures séparées.
    fn old_pass(store: &mut Store) {
        let r = compute_from(store).unwrap();
        store.replace_derived_dependencies(&r.dependencies).unwrap();
        store.replace_edges(&r.edges).unwrap();
        let v = crate::summary::compute(store).unwrap();
        store.set_summary(&v.to_string(), 1).unwrap();
    }

    fn seed_modules(e: &mut Env) {
        build_module(
            e,
            "api/pom.xml",
            "com.acme:api",
            &["com.acme:core", "org.slf4j:slf4j-api"],
        );
        build_module(e, "core/pom.xml", "com.acme:core", &["org.x:lib"]);
        e.store
            .replace_file(
                "S.java",
                &[ty("com.c.Svc", "S.java"), ty("com.c.Repo", "S.java")],
                &[rf(
                    &tid("com.c.Svc"),
                    EdgeKind::Imports,
                    "Repo",
                    Some("package com.c;"),
                )],
            )
            .unwrap();
    }

    fn state(e: &Env) -> (Vec<Node>, Vec<crate::model::Edge>, String) {
        (
            e.store.nodes().unwrap(),
            e.store.edges().unwrap(),
            e.store.summary().unwrap().unwrap().0,
        )
    }

    /// AC-060-3 : une passe de résolution suivie du résumé global = une transaction, même résultat.
    #[test]
    fn req060_ac3_resolution_pass_is_one_transaction_with_same_result() {
        let files = ["api/pom.xml", "core/pom.xml", "S.java"];
        let mut new = env(&files);
        let mut old = env(&files);
        seed_modules(&mut new);
        seed_modules(&mut old);
        crate::store::reset_diagnostics();
        resolve(&mut new.store).unwrap();
        assert_eq!(crate::store::diagnostics().commits(), 1);
        old_pass(&mut old.store);
        assert_eq!(state(&new), state(&old));
        assert!(!state(&new).1.is_empty());

        // Seconde passe après modification : dépendance retirée, lien disparu, résumé recalculé.
        for e in [&mut new, &mut old] {
            build_module(e, "api/pom.xml", "com.acme:api", &["com.acme:core"]);
            e.store
                .replace_file("S.java", &[ty("com.c.Svc", "S.java")], &[])
                .unwrap();
        }
        crate::store::reset_diagnostics();
        resolve(&mut new.store).unwrap();
        assert_eq!(crate::store::diagnostics().commits(), 1);
        old_pass(&mut old.store);
        assert_eq!(state(&new), state(&old));
        assert!(!has_node(
            &new,
            &node_id(NodeKind::Dependency, "org.slf4j:slf4j-api")
        ));
    }

    // ---- Passe incrémentale (REQ-066, DEC-077) ----

    const CODE_POOL: [&str; 8] = [
        "j1.java",
        "j2.java",
        "m1/j3.java",
        "m2/j4.java",
        "m1/k1.kt",
        "k2.kt",
        "t1.ts",
        "m1/t2.ts",
    ];
    const DOC_POOL: [&str; 3] = ["d1.md", "d2.md", "docs/d3.md"];
    const DATA_POOL: [&str; 2] = ["data/s1.json", "data/s2.json"];
    const POM_POOL: [&str; 3] = ["pom.xml", "m1/pom.xml", "m2/pom.xml"];
    const REF_NAMES: [&str; 30] = [
        "A",
        "B",
        "C",
        "A.m1",
        "B.m2",
        "C.m1",
        "a.b.A",
        "a.c.B",
        "a.c.B.m1",
        "x.C.m2",
        "m1",
        "m2",
        "fn1",
        "Zed",
        "Zed.m1",
        "lib/str.fn1",
        "lib/str",
        "web.ui.Panel",
        "Panel",
        "Panel.m1",
        "src/x#y",
        "g:a:1.0",
        "org.x:lib1",
        "com.acme:m1",
        "users",
        "orders",
        "d1.md",
        "d2.md",
        "idée",
        "Cache",
    ];
    const DEPS: [&str; 5] = [
        "com.acme:m1",
        "com.acme:m2",
        "com.acme:root",
        "org.x:lib1",
        "org.x:lib2",
    ];
    const CONCEPTS: [&str; 3] = ["idée", "Cache", "A"];

    fn concept(name: &str) -> Node {
        Node::new(NodeKind::Concept, name, name, None, None, None)
    }

    fn pending(path: &str) -> FileRecord {
        FileRecord {
            path: path.into(),
            hash: "h".into(),
            size: 1,
            mtime: 1,
            recorded_at: 0,
            state: FileState::Pending,
        }
    }

    fn random_file_context(g: &mut Lcg, pkg: &str) -> Option<String> {
        match g.next(7) {
            0 => None,
            6 => Some("import q.B\nimport q.A".to_string()),
            1 => Some(format!("package {pkg}")),
            2 => Some(format!("package {pkg}\nimport a.c.A\nimport a.c.B")),
            3 => Some("import a.b.*".to_string()),
            4 => Some(format!("import static a.c.B.m1\nimport {pkg}.C")),
            _ => Some(format!("package {pkg}\nimport a.b.A\nimport a.c.*")),
        }
    }

    /// Nœuds et références aléatoires d'un fichier du réservoir (mêmes noms d'un fichier à
    /// l'autre : nœuds partagés, noms ambigus).
    fn gen_content(g: &mut Lcg, path: &str) -> (Vec<Node>, Vec<Ref>) {
        let mut nodes: Vec<Node> = Vec::new();
        let mut refs: Vec<Ref> = Vec::new();
        if path.ends_with("pom.xml") {
            let dir = path.trim_end_matches("pom.xml").trim_end_matches('/');
            let q = format!("com.acme:{}", if dir.is_empty() { "root" } else { dir });
            let m = plain(NodeKind::Module, &q, Some(path));
            for _ in 0..g.next(4) {
                refs.push(rf(&m.id, EdgeKind::DependsOn, g.pick(&DEPS), None));
            }
            nodes.push(m);
        } else if path.ends_with(".md") {
            let doc = Node::new(NodeKind::Document, path, path, Some(path), None, None);
            let mut sources = vec![doc.id.clone()];
            nodes.push(doc);
            for s in 0..g.next(3) {
                let sec = Node::new(
                    NodeKind::Section,
                    &format!("{path}#S{s}"),
                    &format!("S{s}"),
                    Some(path),
                    None,
                    None,
                );
                sources.push(sec.id.clone());
                nodes.push(sec);
            }
            for _ in 0..g.next(5) {
                let src = sources[g.next(sources.len())].clone();
                if src.starts_with("document:") && g.next(3) == 0 {
                    refs.push(rf(&src, EdgeKind::LinksTo, g.pick(&REF_NAMES), None));
                } else {
                    refs.push(rf(&src, EdgeKind::Mentions, g.pick(&REF_NAMES), None));
                }
            }
        } else if path.ends_with(".json") {
            let doc = Node::new(
                NodeKind::Document,
                path,
                path,
                Some(path),
                Some(SubKind::Json),
                None,
            );
            let mut sources = vec![doc.id.clone()];
            nodes.push(doc);
            for t in ["users", "orders"] {
                if g.next(2) == 0 {
                    let table = Node::new(
                        NodeKind::Table,
                        t,
                        t,
                        Some(path),
                        Some(SubKind::Table),
                        None,
                    );
                    sources.push(table.id.clone());
                    nodes.push(table);
                    if g.next(2) == 0 {
                        let key = Node::new(
                            NodeKind::Key,
                            &format!("{t}.id"),
                            "id",
                            Some(path),
                            None,
                            None,
                        );
                        sources.push(key.id.clone());
                        nodes.push(key);
                    }
                }
            }
            for _ in 0..g.next(4) {
                let src = sources[g.next(sources.len())].clone();
                let kind = if src.starts_with("table:") {
                    EdgeKind::References
                } else {
                    EdgeKind::Mentions
                };
                refs.push(rf(&src, kind, g.pick(&REF_NAMES), None));
            }
        } else {
            let ts = path.ends_with(".ts");
            let pkg = if ts {
                g.pick(&["lib/str", "web.ui"])
            } else {
                g.pick(&["a.b", "a.c", "x"])
            };
            // Le package, le type ou son membre peuvent être déclarés par un autre fichier.
            if g.next(4) != 0 {
                nodes.push(plain(NodeKind::Package, pkg, Some(path)));
            }
            let types: [&str; 3] = if ts {
                ["Panel", "Fmt", "A"]
            } else {
                ["A", "B", "C"]
            };
            for t in types {
                if g.next(5) == 0 {
                    nodes.push(method(&format!("{pkg}.{t}.m1"), path));
                }
                if g.next(2) == 0 {
                    nodes.push(ty(&format!("{pkg}.{t}"), path));
                    for m in ["m1", "m2"] {
                        if g.next(2) == 0 {
                            nodes.push(method(&format!("{pkg}.{t}.{m}"), path));
                        }
                    }
                }
            }
            if g.next(3) == 0 {
                nodes.push(method(&format!("{pkg}.fn1"), path));
            }
            let ctx = random_file_context(g, pkg);
            for _ in 0..g.next(5) {
                if nodes.is_empty() {
                    break;
                }
                let src = &nodes[g.next(nodes.len())];
                let kinds: &[EdgeKind] = match src.kind {
                    NodeKind::Method => &[EdgeKind::Calls, EdgeKind::Calls, EdgeKind::Mentions],
                    NodeKind::Type => &[
                        EdgeKind::Imports,
                        EdgeKind::Extends,
                        EdgeKind::Implements,
                        EdgeKind::Mentions,
                    ],
                    _ => &[EdgeKind::Imports, EdgeKind::Mentions],
                };
                let kind = kinds[g.next(kinds.len())];
                let r = rf(&src.id, kind, g.pick(&REF_NAMES), ctx.as_deref());
                refs.push(r);
            }
        }
        (nodes, refs)
    }

    /// État d'une séquence aléatoire d'écritures sur le stockage.
    struct Seq {
        g: Lcg,
        known: std::collections::BTreeSet<String>,
        specs: std::collections::BTreeMap<String, (Vec<Node>, Vec<Ref>)>,
        modules: std::collections::BTreeSet<String>,
        /// Un nœud `module` a été inséré ou supprimé depuis la dernière passe.
        module_touched: bool,
    }

    impl Seq {
        fn write(&mut self, st: &mut Store, path: &str, content: (Vec<Node>, Vec<Ref>)) {
            if self.known.insert(path.to_string()) {
                st.insert_pending_files(&[pending(path)]).unwrap();
            }
            let has = content.0.iter().any(|n| n.kind == NodeKind::Module);
            if has != self.modules.contains(path) {
                self.module_touched = true;
                if has {
                    self.modules.insert(path.to_string());
                } else {
                    self.modules.remove(path);
                }
            }
            st.replace_structure(path, &content.0, &content.1).unwrap();
            self.specs.insert(path.to_string(), content);
        }

        fn remove(&mut self, st: &mut Store, path: &str) {
            if self.modules.remove(path) {
                self.module_touched = true;
            }
            st.remove_files(&[path]).unwrap();
            self.known.remove(path);
            self.specs.remove(path);
        }

        fn any_path(&mut self) -> String {
            let all: Vec<&str> = CODE_POOL
                .iter()
                .chain(&DOC_POOL)
                .chain(&DATA_POOL)
                .chain(&POM_POOL)
                .copied()
                .collect();
            all[self.g.next(all.len())].to_string()
        }

        /// Une écriture aléatoire.
        fn step(&mut self, st: &mut Store) {
            match self.g.next(20) {
                0..=9 => {
                    let p = self.any_path();
                    let c = gen_content(&mut self.g, &p);
                    self.write(st, &p, c);
                }
                10..=12 => {
                    let known: Vec<String> = self.known.iter().cloned().collect();
                    if !known.is_empty() {
                        let p = known[self.g.next(known.len())].clone();
                        self.remove(st, &p);
                    } else {
                        let p = self.any_path();
                        let c = gen_content(&mut self.g, &p);
                        self.write(st, &p, c);
                    }
                }
                13..=16 => {
                    let with_nodes: Vec<String> = self
                        .specs
                        .iter()
                        .filter(|(_, (n, _))| !n.is_empty())
                        .map(|(p, _)| p.clone())
                        .collect();
                    if with_nodes.is_empty() {
                        return self.step(st);
                    }
                    let p = with_nodes[self.g.next(with_nodes.len())].clone();
                    let declared: Vec<String> =
                        self.specs[&p].0.iter().map(|n| n.id.clone()).collect();
                    let concepts: Vec<Node> = (0..self.g.next(3))
                        .map(|_| concept(self.g.pick(&CONCEPTS)))
                        .collect();
                    let refs: Vec<Ref> = (0..self.g.next(4))
                        .map(|_| {
                            let src = declared[self.g.next(declared.len())].clone();
                            let kind = if self.g.next(5) == 0 {
                                EdgeKind::Calls
                            } else {
                                EdgeKind::Mentions
                            };
                            rf(&src, kind, self.g.pick(&REF_NAMES), None)
                        })
                        .collect();
                    st.apply_enrichments(
                        "m",
                        &[crate::store::EnrichmentWrite {
                            path: p,
                            hash: "h".into(),
                            summaries: vec![],
                            concepts,
                            refs,
                        }],
                    )
                    .unwrap();
                }
                _ => {
                    // Déplacement d'un fichier de code : mêmes nœuds et références, autre chemin.
                    let movable: Vec<String> = self
                        .specs
                        .keys()
                        .filter(|p| CODE_POOL.contains(&p.as_str()))
                        .cloned()
                        .collect();
                    let free: Vec<&str> = CODE_POOL
                        .iter()
                        .copied()
                        .filter(|p| !self.known.contains(*p))
                        .collect();
                    if movable.is_empty() || free.is_empty() {
                        return self.step(st);
                    }
                    let from = movable[self.g.next(movable.len())].clone();
                    let to = free[self.g.next(free.len())];
                    let (mut nodes, refs) = self.specs[&from].clone();
                    for n in &mut nodes {
                        if n.file.is_some() {
                            n.file = Some(to.to_string());
                        }
                    }
                    self.remove(st, &from);
                    self.write(st, to, (nodes, refs));
                }
            }
        }
    }

    type GraphState = (
        Vec<Node>,
        Vec<Edge>,
        Vec<i64>,
        ResolveStats,
        Vec<(String, String, String)>,
        String,
    );

    fn graph_state(repo: &std::path::Path, st: &Store, stats: &ResolveStats) -> GraphState {
        let conn = rusqlite::Connection::open(crate::store::db_path(repo)).unwrap();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM refs WHERE unresolved = 1 ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let mut un: Vec<(String, String, String)> = unresolved(st)
            .unwrap()
            .into_iter()
            .map(|r| (r.source, r.kind.as_str().to_string(), r.target_name))
            .collect();
        un.sort();
        (
            st.nodes().unwrap(),
            st.edges().unwrap(),
            ids,
            stats.clone(),
            un,
            st.summary().unwrap().unwrap().0,
        )
    }

    /// Copie du graphe (`VACUUM INTO`) rouverte en écriture : marqueur `à résoudre`, donc passe
    /// complète.
    fn oracle_copy(main: &std::path::Path, oracle: &std::path::Path) -> Store {
        let dest = crate::store::db_path(oracle);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", dest.display()));
        }
        rusqlite::Connection::open(crate::store::db_path(main))
            .unwrap()
            .execute("VACUUM INTO ?1", [dest.to_str().unwrap()])
            .unwrap();
        Store::open_or_create(oracle).unwrap()
    }

    /// AC-066-1 : 300 séquences aléatoires, après chaque passe le graphe est celui de la passe
    /// complète sur une copie du même graphe.
    #[test]
    fn req066_ac1_incremental_pass_equals_full_pass_on_a_copy() {
        let main_dir = tempfile::tempdir().unwrap();
        let oracle_dir = tempfile::tempdir().unwrap();
        let (mut incremental, mut full, mut skipped, mut edges_seen) = (0, 0, 0, 0);
        let mut st = Store::open_or_create(main_dir.path()).unwrap();
        for seed in 0..300u64 {
            st.reset().unwrap();
            let mut seq = Seq {
                g: Lcg(seed * 7919 + 13),
                known: Default::default(),
                specs: Default::default(),
                modules: Default::default(),
                module_touched: false,
            };
            // Graphe résolu de départ.
            for _ in 0..3 {
                seq.step(&mut st);
            }
            resolve(&mut st).unwrap();
            for round in 0..3 {
                seq.module_touched = false;
                for _ in 0..1 + seq.g.next(5) {
                    seq.step(&mut st);
                }
                let mut oracle = oracle_copy(main_dir.path(), oracle_dir.path());
                let ctx = format!("séquence {seed}, pas {round}");
                // Cas sans oracle : une mention vise une dépendance dérivée que la même passe
                // supprime (clé étrangère de `edges`, limite connue de la passe complète, qui
                // échoue alors aussi sur le graphe réel quand elle est choisie).
                let oracle_result = resolve(&mut oracle);
                crate::store::reset_diagnostics();
                let main_result = resolve(&mut st);
                let d = crate::store::diagnostics();
                if seq.module_touched {
                    full += 1;
                } else {
                    incremental += 1;
                    assert_eq!((d.full_passes(), d.incremental_passes()), (0, 1), "{ctx}");
                }
                let Ok(expected_stats) = oracle_result else {
                    skipped += 1;
                    if seq.module_touched {
                        assert!(main_result.is_err(), "{ctx}");
                        break;
                    }
                    continue;
                };
                let stats = main_result.unwrap_or_else(|e| panic!("{ctx} : {e}"));
                if seq.module_touched {
                    assert_eq!((d.full_passes(), d.incremental_passes()), (1, 0), "{ctx}");
                }
                let got = graph_state(main_dir.path(), &st, &stats);
                let want = graph_state(oracle_dir.path(), &oracle, &expected_stats);
                assert_eq!(got.0, want.0, "nœuds, {ctx}");
                assert_eq!(got.1, want.1, "liens, {ctx}");
                assert_eq!(got.2, want.2, "statut des références, {ctx}");
                assert_eq!(got.3, want.3, "bilan, {ctx}");
                assert_eq!(got.4, want.4, "non résolues, {ctx}");
                assert_eq!(got.5, want.5, "résumé, {ctx}");
                edges_seen += got.1.len();
            }
        }
        assert!(incremental > 500, "{incremental} passes incrémentales");
        assert!(full > 15, "{full} passes complètes");
        assert!(skipped < 25, "{skipped} pas sans oracle");
        assert!(edges_seen > 3000, "{edges_seen} liens comparés");
    }

    /// Comparaison d'un graphe réel et d'une copie passée par la passe complète.
    fn assert_same_as_full_pass(e: &mut Env, what: &str) -> ResolveStats {
        let oracle_dir = tempfile::tempdir().unwrap();
        let mut oracle = oracle_copy(e._dir.path(), oracle_dir.path());
        let stats = resolve(&mut e.store).unwrap();
        let want_stats = resolve(&mut oracle).unwrap();
        let got = graph_state(e._dir.path(), &e.store, &stats);
        let want = graph_state(oracle_dir.path(), &oracle, &want_stats);
        assert_eq!(got, want, "{what}");
        stats
    }

    /// DEC-077 : la tête d'un nom pointé (`B.m2`) désigne les références à réévaluer quand un
    /// type `B` apparaît, même si la clé du nom (`m2`) n'est celle d'aucun nœud inséré.
    #[test]
    fn req066_head_of_a_dotted_name_reevaluates_the_reference() {
        let mut e = env(&["B.java", "U.java", "C.java"]);
        e.store
            .replace_file(
                "B.java",
                &[ty("p.B", "B.java"), method("p.B.m2", "B.java")],
                &[],
            )
            .unwrap();
        e.store
            .replace_file(
                "U.java",
                &[ty("u.U", "U.java"), method("u.U.run", "U.java")],
                &[rf(
                    &mid("u.U.run"),
                    EdgeKind::Calls,
                    "B.m2",
                    Some("package u\nimport q.B"),
                )],
            )
            .unwrap();
        resolve(&mut e.store).unwrap();
        assert!(has_edge(
            &e,
            &mid("u.U.run"),
            EdgeKind::Calls,
            &mid("p.B.m2")
        ));
        e.store
            .replace_file("C.java", &[ty("r.B", "C.java")], &[])
            .unwrap();
        crate::store::reset_diagnostics();
        assert_same_as_full_pass(&mut e, "type B ajouté");
        assert_eq!(crate::store::diagnostics().incremental_passes(), 1);
        assert!(!has_edge(
            &e,
            &mid("u.U.run"),
            EdgeKind::Calls,
            &mid("p.B.m2")
        ));
        // Le type retiré, la référence retrouve sa cible.
        e.store.remove_file("C.java").unwrap();
        assert_same_as_full_pass(&mut e, "type B retiré");
        assert!(has_edge(
            &e,
            &mid("u.U.run"),
            EdgeKind::Calls,
            &mid("p.B.m2")
        ));
    }

    /// 400 fichiers Java en 40 packages de 10 : types et méthodes aux noms tous distincts, chaque
    /// type appelé par 3 autres, 5 références non résolues vers `Zed` dans 5 fichiers.
    fn big_graph() -> Env {
        let paths: Vec<String> = (0..400).map(|i| format!("p{}/T{i}.java", i / 10)).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut e = env(&refs);
        let items: Vec<(String, Vec<Node>, Vec<Ref>)> =
            (0..400).map(|i| big_file(i, &paths[i])).collect();
        e.store.replace_structures(&items).unwrap();
        resolve(&mut e.store).unwrap();
        e
    }

    fn big_file(i: usize, path: &str) -> (String, Vec<Node>, Vec<Ref>) {
        let pkg = format!("p{}", i / 10);
        let (t, m) = (format!("{pkg}.T{i}"), format!("{pkg}.T{i}.m{i}"));
        let mut refs: Vec<Ref> = (1..=3)
            .map(|d| {
                rf(
                    &mid(&m),
                    EdgeKind::Calls,
                    &format!("m{}", (i + d) % 400),
                    None,
                )
            })
            .collect();
        if i % 80 == 3 {
            refs.push(rf(&mid(&m), EdgeKind::Calls, "Zed.run", None));
        }
        (
            path.to_string(),
            vec![
                plain(NodeKind::Package, &pkg, Some(path)),
                ty(&t, path),
                method(&m, path),
            ],
            refs,
        )
    }

    /// Nombre de références dont la source est déclarée par l'un des `files`.
    fn refs_of_files(st: &Store, files: &[&str]) -> u64 {
        let declared: HashSet<String> = st
            .declarations()
            .unwrap()
            .into_iter()
            .filter(|(f, _)| files.contains(&f.as_str()))
            .map(|(_, n)| n)
            .collect();
        st.refs()
            .unwrap()
            .iter()
            .filter(|r| declared.contains(&r.source))
            .count() as u64
    }

    /// AC-066-2 (a) : un fichier modifié ne réévalue que ses références et ne lit que ses liens.
    #[test]
    fn req066_ac2_a_one_modified_file_reads_only_its_links() {
        let mut e = big_graph();
        let total_edges = e.store.count_edges().unwrap() as u64;
        let nodes = e.store.count_nodes().unwrap() as u64;
        let f = "p0/T0.java";
        let (_, mut n, mut r) = big_file(0, f);
        n.push(method("p0.T0.extra", f));
        r.push(rf(&mid("p0.T0.extra"), EdgeKind::Calls, "m77", None));
        r[0] = rf(&mid("p0.T0.m0"), EdgeKind::Calls, "m200", None);
        e.store.replace_file(f, &n, &r).unwrap();
        let sources = refs_of_files(&e.store, &[f]);
        let outgoing: u64 = [mid("p0.T0.m0"), mid("p0.T0.extra")]
            .iter()
            .map(|id| {
                e.store
                    .edges_at(id, true)
                    .unwrap()
                    .iter()
                    .filter(|(_, k)| *k != EdgeKind::Contains)
                    .count() as u64
            })
            .sum();
        let contained = 3; // package -> type, type -> méthodes
        crate::store::reset_diagnostics();
        resolve(&mut e.store).unwrap();
        let d = crate::store::diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (0, 1));
        assert_eq!(d.refs_reevaluated(), sources);
        // Le résumé global relit encore tous les liens et tous les nœuds (remplacé en T-149) :
        // on les retire des compteurs pour ne mesurer que la passe.
        let edges_read = d.edge_rows_read() - e.store.count_edges().unwrap() as u64;
        assert!(edges_read <= outgoing + contained, "{edges_read} liens lus");
        assert!(
            edges_read * 20 < total_edges,
            "{edges_read} / {total_edges}"
        );
        assert_eq!(
            d.node_rows_read() - e.store.count_nodes().unwrap() as u64,
            nodes + 1
        );
    }

    /// AC-066-2 (b) : un fichier ajouté qui rend 5 références résolubles.
    #[test]
    fn req066_ac2_b_added_target_resolves_the_five_unresolved_references() {
        let mut e = big_graph();
        let before = e.store.count_unresolved().unwrap();
        assert_eq!(before, 5);
        e.store
            .insert_pending_files(&[pending("Zed.java")])
            .unwrap();
        e.store
            .replace_file(
                "Zed.java",
                &[
                    plain(NodeKind::Package, "pz", Some("Zed.java")),
                    ty("pz.Zed", "Zed.java"),
                    method("pz.Zed.run", "Zed.java"),
                ],
                &[],
            )
            .unwrap();
        let visiting: Vec<String> = (0..400)
            .filter(|i| i % 80 == 3)
            .map(|i| mid(&format!("p{}.T{i}.m{i}", i / 10)))
            .collect();
        assert_eq!(visiting.len(), 5);
        let bound = refs_of_files(&e.store, &["Zed.java"])
            + e.store
                .refs()
                .unwrap()
                .iter()
                .filter(|r| visiting.contains(&r.source))
                .count() as u64;
        crate::store::reset_diagnostics();
        let stats = resolve(&mut e.store).unwrap();
        let d = crate::store::diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (0, 1));
        assert!(
            d.refs_reevaluated() <= bound,
            "{} > {bound}",
            d.refs_reevaluated()
        );
        assert_eq!(stats.unresolved, 0);
        for v in &visiting {
            assert!(has_edge(&e, v, EdgeKind::Calls, &mid("pz.Zed.run")), "{v}");
        }
    }
}
