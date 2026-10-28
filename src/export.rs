//! Commande `export` (REQ-018) : le graphe entier en JSON, lecture seule, sans LLM.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::ExportFormat;
use crate::error::{Error, Result};
use crate::out::Out;
use crate::resolve;
use crate::store::Store;

/// Version du format d'export.
pub const EXPORT_VERSION: u32 = 1;

/// Erreur d'un export en flux : écriture (sortie fermée, disque plein) ou lecture du graphe.
#[derive(Debug)]
pub enum ExportError {
    Write(std::io::Error),
    Read(Error),
}

impl From<Error> for ExportError {
    fn from(e: Error) -> Self {
        ExportError::Read(e)
    }
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        ExportError::Write(e)
    }
}

impl From<serde_json::Error> for ExportError {
    fn from(e: serde_json::Error) -> Self {
        ExportError::Write(e.into())
    }
}

/// Éléments sérialisés par référence, champs dans l'ordre alphabétique du document de la 0.23.2.
#[derive(Serialize)]
struct NodeOut<'a> {
    community: Option<i64>,
    file: Option<&'a str>,
    id: &'a str,
    kind: &'a str,
    qualified: &'a str,
    short: &'a str,
    subkind: Option<&'a str>,
    summary: Option<&'a str>,
}

#[derive(Serialize)]
struct CommunityOut<'a> {
    cohesion: f64,
    id: i64,
    label: &'a str,
    pivot: &'a str,
    size: i64,
}

#[derive(Serialize)]
struct LinkOut<'a> {
    kind: &'a str,
    source: &'a str,
    target: &'a str,
}

/// Écrit un élément précédé d'une virgule sauf le premier.
fn element<W: Write, T: Serialize>(
    w: &mut W,
    first: &mut bool,
    item: &T,
) -> std::result::Result<(), ExportError> {
    if !std::mem::replace(first, false) {
        w.write_all(b",")?;
    }
    serde_json::to_writer(&mut *w, item)?;
    Ok(())
}

/// Écrit le document JSON de l'export (REQ-018, REQ-080) au fil de la lecture du graphe : aucune
/// liste de nœuds, de liens ni de non résolues n'est gardée en mémoire (DEC-092). Texte identique
/// à `Value::to_string()` de la 0.23.2 : clés dans l'ordre alphabétique, format compact.
///
/// Le `Store` doit être en lecture seule : tout est lu dans l'instantané qu'il tient depuis son
/// ouverture (REQ-084, DEC-094), sans `BEGIN` propre.
pub fn write_json<W: Write>(store: &Store, w: &mut W) -> std::result::Result<(), ExportError> {
    debug_assert!(store.in_read_snapshot());
    w.write_all(b"{\"communities\":[")?;
    let mut first = true;
    for c in store.community_list()? {
        let item = CommunityOut {
            cohesion: c.cohesion,
            id: c.id,
            label: &c.label,
            pivot: &c.pivot,
            size: c.size,
        };
        element(w, &mut first, &item)?;
    }
    w.write_all(b"],\"edges\":[")?;
    first = true;
    store.for_each_edge(|e| {
        let item = LinkOut {
            kind: e.kind.as_str(),
            source: &e.source,
            target: &e.target,
        };
        element(w, &mut first, &item)
    })?;
    w.write_all(b"],\"nodes\":[")?;
    first = true;
    store.for_each_node_with_community(|n, community| {
        let item = NodeOut {
            community,
            file: n.file.as_deref(),
            id: &n.id,
            kind: n.kind.as_str(),
            qualified: &n.qualified,
            short: &n.short,
            subkind: n.subkind.map(|s| s.as_str()),
            summary: n.summary.as_deref(),
        };
        element(w, &mut first, &item)
    })?;
    w.write_all(b"],\"unresolved\":[")?;
    first = true;
    if store.resolution_is_current()? {
        store.for_each_unresolved(|source, kind, target| {
            let item = LinkOut {
                kind: kind.as_str(),
                source,
                target,
            };
            element(w, &mut first, &item)
        })?;
    } else {
        for r in resolve::unresolved(store)? {
            let item = LinkOut {
                kind: r.kind.as_str(),
                source: &r.source,
                target: &r.target_name,
            };
            element(w, &mut first, &item)?;
        }
    }
    write!(w, "],\"version\":{EXPORT_VERSION}}}")?;
    Ok(())
}

/// Nœuds et liens de l'export, sans les non résolues : entrée de la vue HTML (REQ-025), qui ne les
/// affiche pas, et donc sans passe de résolution (DEC-092).
fn nodes_and_edges(store: &Store) -> Result<Value> {
    let nodes: Vec<Value> = store
        .nodes()?
        .into_iter()
        .map(|n| {
            json!({
                "id": n.id,
                "kind": n.kind.as_str(),
                "qualified": n.qualified,
                "short": n.short,
                "file": n.file,
                "subkind": n.subkind.map(|s| s.as_str()),
                "summary": n.summary,
            })
        })
        .collect();
    let edges: Vec<Value> = store
        .edges()?
        .into_iter()
        .map(|e| json!({"source": e.source, "target": e.target, "kind": e.kind.as_str()}))
        .collect();
    Ok(json!({"version": EXPORT_VERSION, "nodes": nodes, "edges": edges}))
}

/// Nom de base des fichiers d'export : dernier composant du chemin canonique du dépôt,
/// en minuscules ASCII, suites hors `[a-z0-9]` réduites à un `-` (AC-018-7).
pub fn slug(repo: &Path) -> String {
    let canon = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let name = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "graphe".to_string()
    } else {
        out.to_string()
    }
}

/// Destination d'un export.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Stdout,
    File(PathBuf),
}

/// `None` : `<slug>.<ext>` dans le dossier courant ; `-` : stdout ; sinon le chemin donné.
pub fn output_path(repo: &Path, output: Option<&Path>, ext: &str) -> Target {
    match output {
        None => Target::File(PathBuf::from(format!("{}.{ext}", slug(repo)))),
        Some(p) if p == Path::new("-") => Target::Stdout,
        Some(p) => Target::File(p.to_path_buf()),
    }
}

/// Enveloppe du flux de sortie (les tests y branchent un écrivain qui échoue).
pub(crate) type Wrap<'a> = &'a dyn Fn(Box<dyn Write>) -> Box<dyn Write>;

pub fn run(repo: &Path, output: Option<&Path>, format: ExportFormat, out: &mut Out) -> Result<()> {
    run_wrapped(repo, output, format, &|w| w, out)
}

/// Écrit le document dans `w` puis vide le tampon.
fn write_document(
    store: &Store,
    page: Option<&str>,
    sink: Box<dyn Write>,
) -> std::result::Result<(), ExportError> {
    let mut w = BufWriter::new(sink);
    match page {
        None => write_json(store, &mut w)?,
        Some(page) => w.write_all(page.as_bytes())?,
    }
    w.write_all(b"\n")?;
    w.flush()?;
    Ok(())
}

pub(crate) fn run_wrapped(
    repo: &Path,
    output: Option<&Path>,
    format: ExportFormat,
    wrap: Wrap,
    out: &mut Out,
) -> Result<()> {
    let store = Store::open_readonly(repo)?;
    // La page est construite avant d'être écrite (V-013).
    let (page, ext) = match format {
        ExportFormat::Json => (None, "json"),
        ExportFormat::Html => (
            Some(crate::view::render(&slug(repo), &nodes_and_edges(&store)?)),
            "html",
        ),
    };
    match output_path(repo, output, ext) {
        Target::Stdout => {
            let sink = wrap(Box::new(std::io::stdout().lock()));
            write_document(&store, page.as_deref(), sink).map_err(|e| match e {
                ExportError::Write(e) => crate::error::stdout_write(&e),
                ExportError::Read(e) => e,
            })
        }
        Target::File(path) => {
            let failed = |e: std::io::Error| {
                Error::Message(format!("écriture de {} impossible : {e}", path.display()))
            };
            // Le fichier est ouvert (donc vidé) avant la lecture du graphe ; commencé puis en
            // échec, il est supprimé : pas d'export tronqué sous son nom (DEC-092).
            let file = std::fs::File::create(&path).map_err(failed)?;
            match write_document(&store, page.as_deref(), wrap(Box::new(file))) {
                Ok(()) => writeln!(out, "{}", path.display()),
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    Err(match e {
                        ExportError::Write(e) => failed(e),
                        ExportError::Read(e) => e,
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req018_ac7_slug_rules() {
        assert_eq!(slug(Path::new("/x/Mon Dépôt_2")), "mon-d-p-t-2");
        assert_eq!(slug(Path::new("/x/--x--")), "x");
        assert_eq!(slug(Path::new("/")), "graphe");
        assert_eq!(slug(Path::new("/x/ééé")), "graphe");
    }

    #[test]
    fn req018_ac7_output_path_choices() {
        let repo = Path::new("/x/Proj");
        assert_eq!(
            output_path(repo, None, "json"),
            Target::File("proj.json".into())
        );
        assert_eq!(
            output_path(repo, Some(Path::new("-")), "json"),
            Target::Stdout
        );
        assert_eq!(
            output_path(repo, Some(Path::new("a/b.json")), "json"),
            Target::File("a/b.json".into())
        );
    }

    use crate::model::{EdgeKind, FileRecord, FileState, Node, NodeKind, Ref, SubKind};

    fn graph(repo: &Path, resolved: bool) {
        let mut st = Store::open_or_create(repo).unwrap();
        let names = ["A", "B", "C"];
        let files: Vec<FileRecord> = names
            .iter()
            .map(|n| FileRecord {
                path: format!("{n}.java"),
                hash: "h".into(),
                size: 3,
                mtime: 7,
                recorded_at: 0,
                state: FileState::Indexed,
            })
            .collect();
        st.insert_pending_files(&files).unwrap();
        let structures: Vec<_> = names
            .iter()
            .map(|n| {
                let node = Node::new(
                    NodeKind::Type,
                    &format!("p.{n}"),
                    n,
                    Some(&format!("{n}.java")),
                    Some(SubKind::Class),
                    None,
                );
                let refs = vec![
                    Ref {
                        source: node.id.clone(),
                        kind: EdgeKind::Imports,
                        target_name: "p.B".into(),
                        context: Some("package p;".into()),
                    },
                    Ref {
                        source: node.id.clone(),
                        kind: EdgeKind::Imports,
                        target_name: format!("absent.{n}"),
                        context: Some("package p;".into()),
                    },
                ];
                (format!("{n}.java"), vec![node], refs)
            })
            .collect();
        st.replace_structures(&structures).unwrap();
        if resolved {
            crate::resolve::resolve(&mut st).unwrap();
            assert!(st.resolution_is_current().unwrap());
        } else {
            assert!(!st.resolution_is_current().unwrap());
        }
    }

    fn json_of(store: &Store) -> String {
        let mut out = Vec::new();
        write_json(store, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn req084_ac2_export_reads_one_snapshot_despite_concurrent_writes() {
        for resolved in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            graph(dir.path(), resolved);
            let before_text = json_of(&Store::open_readonly(dir.path()).unwrap());
            assert!(before_text.contains("\"unresolved\":[{"));
            let before_view = nodes_and_edges(&Store::open_readonly(dir.path()).unwrap()).unwrap();

            let reader = Store::open_readonly(dir.path()).unwrap();
            assert!(reader.in_read_snapshot());
            let mut writer = Store::open_existing(dir.path()).unwrap();
            assert_eq!(writer.remove_files(&["B.java", "C.java"]).unwrap(), 2);
            assert!(!writer.resolution_is_current().unwrap());
            drop(writer);

            assert_eq!(json_of(&reader), before_text, "résolu={resolved}");
            assert_eq!(nodes_and_edges(&reader).unwrap(), before_view);
            assert!(reader.in_read_snapshot());
            drop(reader);
            let after = json_of(&Store::open_readonly(dir.path()).unwrap());
            assert_ne!(after, before_text, "résolu={resolved}");
            assert!(!after.contains("p.C"));
        }
    }
}
