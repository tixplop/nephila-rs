//! Commande `build` : reconstruction complète du graphe (REQ-009, REQ-006).

use std::path::Path;
use std::time::Instant;

use crate::config::{Config, DEFAULT_CONCURRENCY, DEFAULT_MAX_CHUNK_CHARS};
use crate::error::{warn, Error, Result};
use crate::index::lock::RepoLock;
use crate::index::{enrich, structure};
use crate::llm::{build_llm, Llm};
use crate::model::{FileRecord, FileState};
use crate::resolve;
use crate::scan;
use crate::store::{db_path, Store};
use crate::trace;

/// Bilan d'un build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildReport {
    pub processed: usize,
    pub pending: usize,
    pub nodes: usize,
    pub edges: usize,
    pub unresolved: usize,
    /// Nombre de fichiers dont l'analyse ou l'enrichissement a échoué.
    pub failed: usize,
    /// Fichiers indexés dont le contenu n'a pas été enrichi (résumés et concepts).
    pub to_enrich: usize,
    /// Vrai si un fournisseur LLM était actif pour cette commande.
    pub llm_active: bool,
    /// Cause de l'arrêt de l'envoi de nouveaux fichiers (clé refusée, panne réseau répétée).
    pub stop_cause: Option<String>,
}

/// Réglages de l'indexation : ceux de la configuration, ou les défauts sans configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexOptions {
    /// Nombre de fils d'analyse et d'appels LLM simultanés.
    pub concurrency: usize,
    pub max_chunk_chars: usize,
    /// Nombre maximal de fichiers par appel LLM (REQ-027).
    pub batch_files: usize,
}

impl Default for IndexOptions {
    fn default() -> Self {
        IndexOptions {
            concurrency: DEFAULT_CONCURRENCY,
            max_chunk_chars: DEFAULT_MAX_CHUNK_CHARS,
            batch_files: crate::config::DEFAULT_BATCH_FILES,
        }
    }
}

impl From<&Config> for IndexOptions {
    fn from(c: &Config) -> Self {
        IndexOptions {
            concurrency: c.concurrency,
            max_chunk_chars: c.max_chunk_chars,
            batch_files: c.batch_files,
        }
    }
}

/// Réglages et fournisseur LLM (absent : indexation sans enrichissement).
pub(crate) struct Setup {
    pub opts: IndexOptions,
    pub llm: Option<Box<dyn Llm>>,
}

/// Prépare une commande d'indexation : avec `--no-llm` la configuration n'est pas lue ; sans
/// fichier de configuration le LLM est inactif ; sinon validation et vérification de la clé.
pub(crate) fn setup(no_llm: bool, batch_files: Option<usize>) -> Result<Setup> {
    if no_llm {
        trace!("config: llm inactif (--no-llm)");
        return Ok(Setup {
            opts: IndexOptions::default(),
            llm: None,
        });
    }
    match Config::load_optional()? {
        None => {
            trace!("config: llm inactif (aucune configuration)");
            Ok(Setup {
                opts: IndexOptions::default(),
                llm: None,
            })
        }
        Some(mut config) => {
            if let Some(n) = batch_files {
                config.batch_files = n;
            }
            let mut tls_trace = String::new();
            if let Some(p) = &config.tls_ca_cert {
                tls_trace.push_str(&format!(" tls_ca_cert={}", p.display()));
            }
            if config.tls_insecure {
                tls_trace.push_str(" tls_insecure");
            }
            trace!(
                "config: llm={} model={} concurrency={} timeout_secs={} max_chunk_chars={} batch_files={}{}",
                config.provider.as_str(),
                config.model,
                config.concurrency,
                config.timeout_secs,
                config.max_chunk_chars,
                config.batch_files,
                tls_trace
            );
            let llm = build_llm(&config)?;
            let started = Instant::now();
            let verified = llm.verify_key();
            let ms = started.elapsed().as_millis();
            match &verified {
                Ok(()) => trace!("verify_key: ok {ms} ms"),
                Err(e) => trace!(
                    "verify_key: {} {ms} ms : {}",
                    trace::classify(e),
                    trace::short(&e.to_string(), trace::MAX_MESSAGE_CHARS)
                ),
            }
            verified?;
            Ok(Setup {
                opts: IndexOptions::from(&config),
                llm: Some(llm),
            })
        }
    }
}

impl BuildReport {
    pub fn line(&self) -> String {
        self.line_for("build")
    }

    pub fn line_for(&self, verb: &str) -> String {
        let mut line = format!(
            "{verb} terminé : {} fichiers traités, {} nœuds, {} liens, {} références non résolues, {} fichiers en attente, {} fichiers à enrichir",
            self.processed, self.nodes, self.edges, self.unresolved, self.pending, self.to_enrich
        );
        if !self.llm_active {
            line.push_str(", enrichissement : désactivé");
        }
        line
    }
}

/// Point d'entrée de `nephila build` : valide le dépôt, la configuration et la clé, puis construit.
pub fn run(repo: &Path, no_llm: bool, verbose: bool, batch_files: Option<usize>) -> Result<()> {
    trace::init(verbose);
    check_repo(repo)?;
    let setup = setup(no_llm, batch_files)?;
    // Le verrou précède toute écriture, mais suit les contrôles qui ne doivent rien créer.
    let lock = RepoLock::acquire(repo)?;
    let report = build(repo, &setup.opts, setup.llm.as_deref(), &lock)?;
    finish(&report, "build")
}

/// Affiche le bilan ; un fichier resté en attente, un fichier à enrichir (LLM actif) ou un arrêt
/// de l'envoi rend une erreur (code non nul).
pub(crate) fn finish(report: &BuildReport, verb: &str) -> Result<()> {
    println!("{}", report.line_for(verb));
    let mut problems = Vec::new();
    if let Some(cause) = &report.stop_cause {
        problems.push(format!("envoi interrompu : {cause}"));
    }
    if report.pending > 0 {
        problems.push(format!("{} fichier(s) en attente", report.pending));
    }
    if report.llm_active && report.to_enrich > 0 {
        problems.push(format!("{} fichier(s) à enrichir", report.to_enrich));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Message(problems.join(" ; ")))
    }
}

/// Une passe de résolution suivie du rafraîchissement du résumé, tracée.
fn resolve_pass(store: &mut Store) -> Result<resolve::ResolveStats> {
    let started = Instant::now();
    let stats = resolve::resolve(store)?;
    trace!(
        "résolution: {} ms, {} liens, {} non résolues",
        started.elapsed().as_millis(),
        stats.edges,
        stats.unresolved
    );
    crate::summary::refresh(store)?;
    Ok(stats)
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

/// Construit le graphe de `repo` (DEC-019) : préparation, phase structure, résolution et résumé
/// global, puis, `llm` présent, phase enrichissement suivie d'une nouvelle résolution. Le verrou
/// d'écriture, pris par l'appelant, prouve l'exclusivité avant toute écriture.
pub fn build(
    repo: &Path,
    opts: &IndexOptions,
    llm: Option<&dyn Llm>,
    _lock: &RepoLock,
) -> Result<BuildReport> {
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
    trace!("parcours: {} fichiers", records.len());
    store.reset_with_pending_files(&records)?;
    if records.is_empty() {
        warn("aucun fichier pris en charge dans le dépôt : graphe vide");
    }

    let mut report = BuildReport::default();
    structure::analyze(
        repo,
        &scanned.files,
        opts.concurrency,
        &scanned.gradle_settings,
        &mut store,
        &mut report,
    )?;
    finalize(&mut store, llm, opts, repo, &mut report)?;
    Ok(report)
}

/// Fin commune à `build` et `update` : résolution et résumé global (graphe interrogeable), puis
/// enrichissement des fichiers à enrichir si `llm` est présent, et bilan.
pub(crate) fn finalize(
    store: &mut Store,
    llm: Option<&dyn Llm>,
    opts: &IndexOptions,
    repo: &Path,
    report: &mut BuildReport,
) -> Result<()> {
    let mut stats = resolve_pass(store)?;
    report.llm_active = llm.is_some();
    if let Some(llm) = llm {
        let files = store.files_to_enrich()?;
        // Sans fichier à enrichir, le graphe n'a pas changé : pas de seconde résolution.
        if !files.is_empty() {
            enrich::enrich_all(repo, &files, opts, llm, store, report)?;
            stats = resolve_pass(store)?;
        }
    }
    report.nodes = store.count_nodes()?;
    report.edges = stats.edges;
    report.unresolved = stats.unresolved;
    report.pending = store
        .files()?
        .iter()
        .filter(|f| f.state == FileState::Pending)
        .count();
    report.to_enrich = store.count_files_to_enrich()?;
    Ok(())
}
