//! Commande `update` : mise à jour incrémentale du graphe (REQ-011, REQ-010, REQ-008).

use std::path::Path;

use crate::config::Config;
use crate::diff::{self, Change};
use crate::error::Result;
use crate::index::build::{self, BuildReport};
use crate::index::lock::RepoLock;
use crate::llm::{build_llm, Llm};
use crate::model::{FileRecord, FileState};
use crate::resolve;
use crate::scan::ScannedFile;
use crate::store::{db_path, Store};

/// Point d'entrée de `rkg update` : mêmes contrôles que `build` (dépôt, configuration, clé,
/// verrou) ; sans graphe, bascule sur un build complet.
pub fn run(repo: &Path) -> Result<()> {
    build::check_repo(repo)?;
    let config = Config::load()?;
    let llm = build_llm(&config)?;
    llm.verify_key()?;
    let lock = RepoLock::acquire(repo)?;
    // Le test d'existence se fait sous verrou : aucun build concurrent ne peut le créer entre-temps.
    if !db_path(repo).is_file() {
        eprintln!("aucun graphe : build complet");
        let report = build::build(repo, &config, llm.as_ref(), &lock)?;
        return build::finish(&report, "build");
    }
    match update(repo, &config, llm.as_ref(), &lock)? {
        None => {
            println!("graphe à jour");
            Ok(())
        }
        Some(report) => build::finish(&report, "update"),
    }
}

/// Met à jour le graphe ; `None` si rien n'a changé (aucun appel LLM, résumé conservé). Le verrou,
/// pris par l'appelant, prouve l'exclusivité avant toute écriture.
pub fn update(
    repo: &Path,
    config: &Config,
    llm: &dyn Llm,
    _lock: &RepoLock,
) -> Result<Option<BuildReport>> {
    build::check_repo(repo)?;
    let mut store = Store::open_existing(repo)?;
    let d = diff::compute(repo, &store)?;

    // Fichiers dont seule la date a changé : l'enregistrement est rafraîchi pour ne plus
    // recalculer leur empreinte, sans toucher au graphe.
    let touched: Vec<FileRecord> = d
        .scanned
        .iter()
        .filter(|f| {
            d.known.iter().any(|k| {
                k.path == f.path
                    && k.state == FileState::Indexed
                    && k.hash == f.hash
                    && (f.rehashed || k.size != f.size || k.mtime != f.mtime)
            })
        })
        .map(|f| record(f, FileState::Indexed))
        .collect();
    if !touched.is_empty() {
        store.insert_files(&touched)?;
    }
    if d.is_up_to_date() {
        return Ok(None);
    }

    for path in d.paths(Change::Deleted) {
        store.remove_file(path)?;
    }
    let todo: Vec<ScannedFile> = d
        .scanned
        .iter()
        .filter(|f| {
            d.changes
                .iter()
                .any(|c| c.path == f.path && c.change != Change::Deleted)
        })
        .cloned()
        .collect();
    // Nouveaux et modifiés passent « en attente » (empreinte à jour) jusqu'à leur extraction :
    // un échec ou une interruption les laisse repérables par le prochain update.
    let records: Vec<FileRecord> = todo.iter().map(|f| record(f, FileState::Pending)).collect();
    store.insert_files(&records)?;

    let mut report = BuildReport::default();
    build::extract_all(
        repo,
        &todo,
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
    Ok(Some(report))
}

fn record(f: &ScannedFile, state: FileState) -> FileRecord {
    FileRecord {
        path: f.path.clone(),
        hash: f.hash.clone(),
        size: f.size,
        mtime: f.mtime,
        recorded_at: 0,
        state,
    }
}
