//! Commande `build` : reconstruction complète du graphe (REQ-009, REQ-006).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

use crate::chunk;
use crate::config::Config;
use crate::error::{warn, Error, Result};
use crate::index::lock::RepoLock;
use crate::llm::{build_llm, Extraction, Llm};
use crate::model::{FileRecord, FileState, Node, NodeKind, Ref};
use crate::resolve;
use crate::scan::{self, ScannedFile};
use crate::store::{db_path, Store};

/// Bilan d'un build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildReport {
    pub processed: usize,
    pub pending: usize,
    pub nodes: usize,
    pub edges: usize,
    pub unresolved: usize,
    /// Premier échec d'extraction rencontré, s'il y en a un.
    pub failure: Option<String>,
}

impl BuildReport {
    pub fn line(&self) -> String {
        self.line_for("build")
    }

    pub fn line_for(&self, verb: &str) -> String {
        format!(
            "{verb} terminé : {} fichiers traités, {} nœuds, {} liens, {} références non résolues, {} fichiers en attente",
            self.processed, self.nodes, self.edges, self.unresolved, self.pending
        )
    }
}

/// Point d'entrée de `rkg build` : valide le dépôt, la configuration et la clé, puis construit.
pub fn run(repo: &Path) -> Result<()> {
    check_repo(repo)?;
    let config = Config::load()?;
    let llm = build_llm(&config)?;
    llm.verify_key()?;
    // Le verrou précède toute écriture, mais suit les contrôles qui ne doivent rien créer.
    let lock = RepoLock::acquire(repo)?;
    let report = build(repo, &config, llm.as_ref(), &lock)?;
    finish(&report, "build")
}

/// Affiche le bilan ; un échec d'extraction rend une erreur (code non nul).
pub(crate) fn finish(report: &BuildReport, verb: &str) -> Result<()> {
    println!("{}", report.line_for(verb));
    match &report.failure {
        None => Ok(()),
        Some(msg) => Err(Error::Message(format!(
            "{} fichier(s) laissé(s) en attente ; premier échec : {msg}",
            report.pending
        ))),
    }
}

pub(crate) fn check_repo(repo: &Path) -> Result<()> {
    if !repo.is_dir() {
        return Err(Error::Message(format!(
            "{} n'est pas un dossier lisible",
            repo.display()
        )));
    }
    std::fs::read_dir(repo).map_err(|e| {
        Error::Message(format!(
            "{} n'est pas un dossier lisible : {e}",
            repo.display()
        ))
    })?;
    Ok(())
}

/// Construit le graphe de `repo` avec le fournisseur donné (config et clé déjà validées). Le
/// verrou d'écriture, pris par l'appelant, prouve l'exclusivité avant toute écriture.
pub fn build(repo: &Path, config: &Config, llm: &dyn Llm, _lock: &RepoLock) -> Result<BuildReport> {
    check_repo(repo)?;
    let mut store = Store::open_or_create(repo)?;
    let known = if db_path(repo).is_file() {
        store.files()?
    } else {
        Vec::new()
    };
    let scanned = scan::scan(repo, &known)?;
    let records: Vec<FileRecord> = scanned
        .files
        .iter()
        .map(|f| FileRecord {
            path: f.path.clone(),
            hash: f.hash.clone(),
            size: f.size,
            mtime: f.mtime,
            recorded_at: 0,
            state: FileState::Pending,
        })
        .collect();
    store.reset_with_pending_files(&records)?;
    if records.is_empty() {
        warn("aucun fichier pris en charge dans le dépôt : graphe vide");
    }

    let mut report = BuildReport::default();
    extract_all(
        repo,
        &scanned.files,
        config.concurrency,
        config.max_chunk_chars,
        llm,
        &mut store,
        &mut report,
    )?;

    let stats = resolve::resolve(&mut store)?;
    report.nodes = store.nodes()?.len();
    report.edges = stats.edges;
    report.unresolved = stats.unresolved;
    report.pending = store
        .files()?
        .iter()
        .filter(|f| f.state == FileState::Pending)
        .count();
    crate::summary::refresh(&mut store)?;
    Ok(report)
}

/// Extrait un fichier, en plusieurs appels s'il dépasse `max_chunk_chars` ; le premier échec
/// d'un morceau fait échouer le fichier entier (REQ-007).
fn extract_chunks(
    llm: &dyn Llm,
    path: &str,
    text: &str,
    max_chunk_chars: usize,
) -> Result<Extraction> {
    let parts = chunk::split(text, max_chunk_chars);
    if parts.len() == 1 {
        return llm.extract(path, 1, &parts[0]);
    }
    let mut results = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        results.push(llm.extract(path, i + 1, part)?);
    }
    Ok(chunk::merge(results))
}

enum Outcome {
    Done(String, Extraction),
    Failed(String, Error),
}

/// Pool de threads LLM ; le thread courant écrit une transaction par fichier.
pub(crate) fn extract_all(
    repo: &Path,
    files: &[ScannedFile],
    concurrency: usize,
    max_chunk_chars: usize,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    let queue: Mutex<VecDeque<&ScannedFile>> = Mutex::new(files.iter().collect());
    let stop = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<Outcome>();
    let workers = concurrency.clamp(1, files.len().max(1));
    let mut write_error: Option<Error> = None;

    std::thread::scope(|scope| {
        let queue = &queue;
        let stop = &stop;
        for _ in 0..workers {
            let tx = tx.clone();
            scope.spawn(move || loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Some(file) = queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front() else {
                    break;
                };
                let outcome = match std::fs::read_to_string(repo.join(&file.path)) {
                    Err(e) => Outcome::Failed(
                        file.path.clone(),
                        Error::Message(format!("lecture de {} impossible : {e}", file.path)),
                    ),
                    Ok(text) => match extract_chunks(llm, &file.path, &text, max_chunk_chars) {
                        Ok(x) => Outcome::Done(file.path.clone(), x),
                        Err(e) => Outcome::Failed(file.path.clone(), e),
                    },
                };
                if matches!(outcome, Outcome::Failed(..)) {
                    stop.store(true, Ordering::SeqCst);
                }
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
                    report
                        .failure
                        .get_or_insert_with(|| format!("{path} : {e}"));
                }
                Outcome::Done(path, x) => {
                    if write_error.is_some() {
                        continue;
                    }
                    let (nodes, refs, dropped) = to_records(&path, x);
                    if dropped > 0 {
                        warn(&format!(
                            "{path} : {dropped} référence(s) ignorée(s), source non déclarée"
                        ));
                    }
                    match store.replace_file(&path, &nodes, &refs) {
                        Ok(()) => report.processed += 1,
                        Err(e) => {
                            stop.store(true, Ordering::SeqCst);
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
        NodeKind::Document | NodeKind::Module => after(&['/']),
        NodeKind::Section => after(&['#']),
        NodeKind::Dependency => after(&[':']),
        _ => after(&['.']),
    }
}

/// Convertit une extraction en nœuds et références rattachés au fichier d'origine. Les références
/// dont la source n'est pas déclarée sont écartées (leur nombre est renvoyé).
fn to_records(path: &str, x: Extraction) -> (Vec<Node>, Vec<Ref>, usize) {
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
            short_name(d.kind, &d.qualified),
            file,
            d.subkind,
            d.summary.as_deref(),
        ));
    }
    let mut refs = Vec::new();
    let mut dropped = 0;
    for r in x.references {
        if !nodes.iter().any(|n| n.id == r.source) {
            dropped += 1;
            continue;
        }
        refs.push(Ref {
            source: r.source,
            kind: r.kind,
            target_name: r.target,
            context: x.context.clone(),
        });
    }
    (nodes, refs, dropped)
}
