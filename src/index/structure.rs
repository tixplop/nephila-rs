//! Phase structure (DEC-019) : analyse syntaxique parallèle des fichiers, une transaction par
//! fichier écrite par le thread principal (seul écrivain, DEC-013).

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::mpsc;
use std::sync::Mutex;

use crate::error::{warn, Error, Result};
use crate::index::build::BuildReport;
use crate::model::{Node, NodeKind, Ref};
use crate::parse::{self, FileStructure, ParseCtx};
use crate::scan::ScannedFile;
use crate::store::Store;

enum Outcome {
    Parsed(String, FileStructure),
    Failed(String, Error),
}

/// Analyse `files` avec `concurrency` fils et enregistre leur structure. Un fichier illisible ou
/// dont l'analyse échoue reste « en attente » (avertissement nommant la cause) ; les autres
/// continuent. `gradle_settings` : table des `settings.gradle(.kts)` du parcours.
pub(crate) fn analyze(
    repo: &Path,
    files: &[ScannedFile],
    concurrency: usize,
    gradle_settings: &HashMap<String, Option<String>>,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    let ctx = ParseCtx {
        repo_root: Some(repo.to_path_buf()),
        gradle_settings: gradle_settings.clone(),
    };
    let queue: Mutex<VecDeque<&ScannedFile>> = Mutex::new(files.iter().collect());
    let (tx, rx) = mpsc::channel::<Outcome>();
    let workers = concurrency.clamp(1, files.len().max(1));
    let mut write_error: Option<Error> = None;

    std::thread::scope(|scope| {
        let queue = &queue;
        let ctx = &ctx;
        for _ in 0..workers {
            let tx = tx.clone();
            scope.spawn(move || loop {
                let Some(file) = queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front() else {
                    break;
                };
                let outcome = match std::fs::read_to_string(repo.join(&file.path)) {
                    Err(e) => Outcome::Failed(
                        file.path.clone(),
                        Error::Message(format!("lecture de {} impossible : {e}", file.path)),
                    ),
                    Ok(text) => match parse::extract(Path::new(&file.path), &text, ctx) {
                        Ok(s) => Outcome::Parsed(file.path.clone(), s),
                        Err(e) => Outcome::Failed(file.path.clone(), e),
                    },
                };
                if tx.send(outcome).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for outcome in rx {
            match outcome {
                Outcome::Failed(path, e) => {
                    warn(&format!("{path} : {e}"));
                    report.failed += 1;
                }
                Outcome::Parsed(path, s) => {
                    if write_error.is_some() {
                        continue;
                    }
                    for w in &s.warnings {
                        warn(w);
                    }
                    let (nodes, refs, dropped) = to_records(&path, &s);
                    if dropped > 0 {
                        warn(&format!(
                            "{path} : {dropped} référence(s) ignorée(s), source non déclarée"
                        ));
                    }
                    match store.replace_structure(&path, &nodes, &refs) {
                        Ok(()) => report.processed += 1,
                        Err(e) => {
                            // Vide la file : les fils s'arrêtent, les résultats restants sont ignorés.
                            queue.lock().unwrap_or_else(|p| p.into_inner()).clear();
                            write_error = Some(e);
                        }
                    }
                }
            }
        }
    });
    match write_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Nom court d'un nœud, selon son genre.
fn short_name(kind: NodeKind, qualified: &str) -> &str {
    let after = |seps: &[char]| qualified.rsplit(seps).next().unwrap_or(qualified);
    match kind {
        NodeKind::Document => after(&['/']),
        NodeKind::Module => after(&[':', '/']),
        NodeKind::Section => after(&['#']),
        NodeKind::Dependency => after(&[':']),
        _ => after(&['.']),
    }
}

/// Convertit la structure d'un fichier en nœuds et références rattachés à ce fichier. Les
/// références dont la source n'est pas déclarée sont écartées (leur nombre est renvoyé).
fn to_records(path: &str, x: &FileStructure) -> (Vec<Node>, Vec<Ref>, usize) {
    let mut nodes: Vec<Node> = Vec::new();
    for d in &x.declares {
        let id = d.id();
        if nodes.iter().any(|n| n.id == id) {
            continue;
        }
        // Dépendances externes et concepts n'ont pas de fichier propre.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Declared;

    fn module(q: &str, short: Option<&str>) -> FileStructure {
        FileStructure {
            declares: vec![Declared {
                kind: NodeKind::Module,
                qualified: q.into(),
                subkind: None,
                summary: None,
                short: short.map(str::to_string),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn dec024_nom_court_porte_par_la_declaration() {
        let (n, _, _) = to_records("api/pom.xml", &module("com.acme:api", Some("api")));
        assert_eq!(n[0].short, "api");
    }

    #[test]
    fn dec024_nom_court_derive_du_module() {
        let (n, _, _) = to_records("pom.xml", &module("com.acme:core", None));
        assert_eq!(n[0].short, "core");
        let (n, _, _) = to_records("pom.xml", &module("core", None));
        assert_eq!(n[0].short, "core");
        let (n, _, _) = to_records("pom.xml", &module("a/b", None));
        assert_eq!(n[0].short, "b");
    }
}
