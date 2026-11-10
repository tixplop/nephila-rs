//! Commande `build` : reconstruction complète du graphe (REQ-009, REQ-006).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::{
    Config, ModelOverride, ProjectSettings, DEFAULT_CONCURRENCY, DEFAULT_MAX_CHUNK_CHARS,
};
use crate::error::{warn, Error, Result};
use crate::filter::{EnrichScope, Filters, Matcher, ProjectFile};
use crate::index::estimate::{self, Estimate};
use crate::index::lock::RepoLock;
use crate::index::{enrich, structure};
use crate::llm::redact::RedactTally;
use crate::llm::{build_llm_for, Llm, LlmTotals};
use crate::model::{FileRecord, FileState};
use crate::out::Out;
use crate::resolve;
use crate::scan;
use crate::store::{db_path, Store};
use crate::trace;

/// Bilan d'un build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildReport {
    pub processed: usize,
    /// Totaux des appels LLM de la phase d'enrichissement (REQ-165).
    pub llm: LlmTotals,
    /// Fichiers dont l'enrichissement conservé a été réappliqué par cette commande (REQ-049).
    pub retained: usize,
    /// Enrichissements conservés illisibles, écartés par cette commande (REQ-131).
    pub unreadable: usize,
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
    /// Première clé refusée (REQ-054) ou requête refusée (REQ-091), quand elle a arrêté l'envoi.
    pub refused: Option<Error>,
    /// Plafond d'appels `<m>` de la commande, quand il a été atteint (REQ-186).
    pub capped: Option<usize>,
    /// Fichiers sans issue laissés « à enrichir » par le plafond (REQ-186).
    pub left_by_cap: usize,
    /// Vrai sous `--batch` : les relances conseillées nomment l'option (REQ-216).
    pub batch: bool,
    /// Vrai si l'envoi a été arrêté par Ctrl-C (REQ-189) : la fin rend `Error::Interrupted`.
    pub interrupted: bool,
    /// Fichiers indexés écartés par les motifs de la portée d'enrichissement (REQ-207).
    pub out_of_scope: usize,
    /// Fichiers exclus par les filtres (REQ-037) ; `None` sans filtre actif.
    pub excluded: Option<usize>,
    /// Relevé de l'estimation sous `--dry-run` (REQ-184) ; sa présence remplace le segment
    /// `enrichissement : désactivé` par `enrichissement : estimé sans envoi`.
    pub estimate: Option<Estimate>,
    /// Nœuds résumés après fusion, des fichiers écrits par cette commande (REQ-235).
    pub summarized: usize,
    /// Nœuds demandés des fichiers écrits par cette commande (REQ-235).
    pub requested: usize,
    /// Bilan de la passe `--subsystems` ; `None` si elle n'a pas eu lieu (REQ-261).
    pub subsystems: Option<crate::subsystems::SubsystemsOutcome>,
    /// Vrai si `--subsystems` n'a fait aucun appel parce que des fichiers restent à enrichir (A-21).
    pub subsystems_skipped: bool,
    /// Bilan sans appel quand `subsystems_skipped` : tout nom absent du cache est laissé (V-002).
    pub subsystems_tally: Option<crate::subsystems::SubsystemsOutcome>,
}

/// Réglages de l'indexation : ceux de la configuration, ou les défauts sans configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOptions {
    /// Nombre d'appels LLM simultanés.
    pub concurrency: usize,
    /// Nombre de fils d'extraction (REQ-071) : `--jobs`, sinon `default_jobs()`.
    pub jobs: usize,
    pub max_chunk_chars: usize,
    /// Nombre maximal de fichiers par appel LLM (REQ-027).
    pub batch_files: usize,
    /// Nouveaux essais d'un appel LLM dont la réponse est non conforme (REQ-038).
    pub format_retries: u32,
    /// Nœuds demandés au plus par appel (REQ-271), dérivé de `max_output_tokens` et `thinking`.
    pub nodes_per_call: usize,
    /// Enrichit aussi les fichiers de code : option ou clé `enrich_code` (REQ-050).
    pub enrich_code: bool,
    /// Masque les valeurs sensibles avant l'envoi : clé `enrich_redact` (REQ-265).
    pub enrich_redact: bool,
    /// Plafond d'appels d'enrichissement de la commande (REQ-186) ; `None` : aucun plafond.
    pub max_calls: Option<usize>,
    /// Motifs de la portée d'enrichissement, personnels et du dépôt (REQ-207).
    pub enrich_scope: EnrichScope,
    /// `--batch` (REQ-211) : l'enrichissement passe par le mode lot du fournisseur.
    pub batch: bool,
    /// `--subsystems` (REQ-261) : passe des noms de sous-systèmes à la fin de la commande.
    pub subsystems: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        IndexOptions {
            concurrency: DEFAULT_CONCURRENCY,
            jobs: crate::index::default_jobs(),
            max_chunk_chars: DEFAULT_MAX_CHUNK_CHARS,
            batch_files: crate::config::DEFAULT_BATCH_FILES,
            format_retries: crate::config::DEFAULT_FORMAT_RETRIES,
            nodes_per_call: enrich::MAX_NODES_PER_CALL,
            enrich_code: false,
            enrich_redact: true,
            max_calls: None,
            enrich_scope: EnrichScope::default(),
            batch: false,
            subsystems: false,
        }
    }
}

impl From<&Config> for IndexOptions {
    fn from(c: &Config) -> Self {
        IndexOptions {
            concurrency: c.concurrency,
            jobs: crate::index::default_jobs(),
            max_chunk_chars: c.max_chunk_chars,
            batch_files: c.batch_files,
            format_retries: c.format_retries,
            nodes_per_call: enrich::nodes_per_call(c.max_output_tokens, c.thinking),
            enrich_code: c.enrich_code,
            enrich_redact: c.enrich_redact,
            max_calls: c.max_calls,
            enrich_scope: EnrichScope::default(),
            batch: false,
            subsystems: false,
        }
    }
}

/// Réglages de l'estimation sous `--dry-run` (REQ-184, DEC-148) ; les autres réglages
/// (`enrich_code`, `max_chunk_chars`, `batch_files`, `format_retries`) sont ceux d'`IndexOptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRun {
    /// Modèle effectif ; `None` sans configuration (aucun enrichissement ne vaut alors).
    pub model: Option<String>,
    /// Langue des résumés effective.
    pub language: String,
    /// Chemin de la configuration personnelle, quand elle n'existe pas.
    pub no_config: Option<PathBuf>,
}

/// Réglages et fournisseur LLM (absent : indexation sans enrichissement).
pub(crate) struct Setup {
    pub opts: IndexOptions,
    pub llm: Option<Box<dyn Llm>>,
    pub dry_run: Option<DryRun>,
    /// Modèle du dépôt qui remplace le modèle personnel sous `model_override = "warn"`.
    pub project_model: Option<ProjectModel>,
}

/// Les deux modèles d'un changement imposé par `.nephila.toml` (REQ-196).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectModel {
    pub project: String,
    pub personal: String,
}

/// Applique les réglages du projet et les options à la configuration, puis trace la ligne
/// `config:` (la même avec ou sans `--dry-run`).
fn effective_config(
    config: Config,
    enrich_code: bool,
    project: &ProjectSettings,
    batch_files: Option<usize>,
    format_retries: Option<u32>,
    max_calls: Option<usize>,
) -> Config {
    let deny = config.model_override == ModelOverride::Deny;
    let (mut config, from_project) = config.with_project(project, batch_files, format_retries);
    config.enrich_code |= enrich_code;
    // L'option remplace la clé (REQ-186) ; le fichier de projet ne la porte pas.
    config.max_calls = max_calls.or(config.max_calls);
    let max_calls_trace = config
        .max_calls
        .map(|m| format!(" max_calls={m}"))
        .unwrap_or_default();
    let mut tls_trace = config.tls_trace();
    if !from_project.is_empty() {
        tls_trace.push_str(&format!(" (.nephila.toml : {})", from_project.join(",")));
    }
    let same_model = config
        .model_names(&config.model)
        .into_iter()
        .filter(|n| *n != config.model)
        .collect::<Vec<_>>()
        .join(",");
    let same_model = if same_model.is_empty() {
        String::new()
    } else {
        format!(" (même modèle que {same_model})")
    };
    trace!(
        "config: llm={} model={}{} enrich_code={} summary_language=\"{}\" thinking={} effort={}{} max_output_tokens={} nodes_per_call={} concurrency={} timeout_secs={} max_chunk_chars={} batch_files={} format_retries={}{}{}",
        config.provider.as_str(),
        config.model,
        same_model,
        config.enrich_code,
        config.summary_language,
        config.thinking,
        config.effort.as_str(),
        config.temperature_trace(),
        config.max_output_tokens,
        enrich::nodes_per_call(config.max_output_tokens, config.thinking),
        config.concurrency,
        config.timeout_secs,
        config.max_chunk_chars,
        config.batch_files,
        config.format_retries,
        max_calls_trace,
        tls_trace
    );
    if let (true, Some(m)) = (deny, &project.model) {
        trace!(
            "modèle: {} de .nephila.toml ignoré (model_override = \"deny\")",
            m
        );
    }
    config
}

/// Prépare une commande d'indexation : avec `--no-llm` la configuration n'est pas lue ; sans
/// fichier de configuration le LLM est inactif ; sinon validation (la clé n'est constatée qu'au
/// premier appel, REQ-054). Sous `--dry-run`, la configuration est lue sans la clé et aucun
/// client n'est construit (REQ-184).
#[allow(clippy::too_many_arguments)]
pub(crate) fn setup(
    no_llm: bool,
    dry_run: bool,
    enrich_code: bool,
    project: &ProjectSettings,
    batch_files: Option<usize>,
    format_retries: Option<u32>,
    max_calls: Option<usize>,
    batch: bool,
) -> Result<Setup> {
    if no_llm {
        trace!("config: llm inactif (--no-llm)");
        return Ok(Setup {
            opts: IndexOptions {
                enrich_code,
                ..IndexOptions::default()
            },
            llm: None,
            dry_run: None,
            project_model: None,
        });
    }
    let loaded = if dry_run {
        Config::load_optional_without_key()?
    } else {
        Config::load_optional()?
    };
    match loaded {
        None => {
            trace!("config: llm inactif (aucune configuration)");
            let defaults = EnrichScope::default();
            Ok(Setup {
                opts: IndexOptions {
                    enrich_code,
                    enrich_scope: EnrichScope::new(
                        defaults.personal_include,
                        defaults.personal_exclude,
                        project,
                    ),
                    ..IndexOptions::default()
                },
                llm: None,
                dry_run: dry_run.then(|| DryRun {
                    model: None,
                    language: crate::config::DEFAULT_SUMMARY_LANGUAGE.to_string(),
                    no_config: crate::config::config_path().ok(),
                }),
                project_model: None,
            })
        }
        Some(config) => {
            let project_model = match &project.model {
                Some(m) if config.model_override == ModelOverride::Warn && *m != config.model => {
                    Some(ProjectModel {
                        project: m.clone(),
                        personal: config.model.clone(),
                    })
                }
                _ => None,
            };
            let config = effective_config(
                config,
                enrich_code,
                project,
                batch_files,
                format_retries,
                max_calls,
            );
            let opts = IndexOptions {
                enrich_scope: EnrichScope::new(
                    config.enrich_include.clone(),
                    config.enrich_exclude.clone(),
                    project,
                ),
                batch,
                ..IndexOptions::from(&config)
            };
            if dry_run {
                return Ok(Setup {
                    opts,
                    llm: None,
                    dry_run: Some(DryRun {
                        model: Some(config.model.clone()),
                        language: config.summary_language.clone(),
                        no_config: None,
                    }),
                    project_model,
                });
            }
            let llm = build_llm_for(&config, batch)?;
            Ok(Setup {
                opts,
                llm: Some(llm),
                dry_run: None,
                project_model,
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
        if self.estimate.is_some() {
            line.push_str(", enrichissement : estimé sans envoi");
        } else if !self.llm_active {
            line.push_str(", enrichissement : désactivé");
        }
        if self.requested > 0 {
            line.push_str(", ");
            line.push_str(&coverage_segment(self.summarized, self.requested));
        }
        if self.out_of_scope > 0 {
            line.push_str(&format!(
                ", {} fichiers hors portée d'enrichissement",
                self.out_of_scope
            ));
        }
        if let Some(k) = self.excluded {
            line.push_str(&format!(", {k} fichiers exclus par les filtres"));
        }
        line
    }
}

/// Options de la ligne de commande qui remplacent les réglages (REQ-071, REQ-028, REQ-038).
#[derive(Debug, Clone, Copy, Default)]
pub struct Overrides {
    pub jobs: Option<usize>,
    pub batch_files: Option<usize>,
    pub format_retries: Option<u32>,
    /// `--max-calls` (REQ-186) : plafond d'appels d'enrichissement de la commande.
    pub max_calls: Option<usize>,
    /// `--accept-project-model` (REQ-197) : lève le refus E-29 pour cette commande.
    pub accept_project_model: bool,
    /// `--dry-run` (REQ-184) : estime l'enrichissement sans client LLM ni clé.
    pub dry_run: bool,
    /// `--batch` (REQ-211) : enrichissement par le mode lot du fournisseur.
    pub batch: bool,
    /// `--subsystems` (REQ-261) : nomme les grandes communautés à la fin de la commande.
    pub subsystems: bool,
}

/// Point d'entrée de `nephila build` : valide le dépôt, la configuration et la clé, puis construit.
pub fn run(
    repo: &Path,
    no_llm: bool,
    enrich_code: bool,
    verbose: bool,
    over: Overrides,
    cli_filters: Filters,
    out: &mut Out,
) -> Result<()> {
    trace::init(verbose);
    check_repo(repo)?;
    // Le fichier de projet est lu avant la configuration LLM, le verrou et toute écriture.
    let project = ProjectFile::load(repo)?;
    let matcher = effective_matcher(repo, &project.filters, &cli_filters)?;
    let mut setup = setup(
        no_llm,
        over.dry_run,
        enrich_code,
        &project.settings,
        over.batch_files,
        over.format_retries,
        over.max_calls,
        over.batch,
    )?;
    if let Some(j) = over.jobs {
        setup.opts.jobs = j;
    }
    setup.opts.subsystems = over.subsystems;
    // Le verrou précède toute écriture, mais suit les contrôles qui ne doivent rien créer.
    let lock = RepoLock::acquire(repo)?;
    check_project_model(repo, &setup, over.accept_project_model, over.dry_run)?;
    let report = build_with_dry_run(
        repo,
        &setup.opts,
        setup.llm.as_deref(),
        setup.dry_run.as_ref(),
        &lock,
        &matcher,
    )?;
    write_report(out, &report, "build")?;
    write_subsystems(out, &report)?;
    write_estimate(out, &report, &setup.opts, setup.dry_run.as_ref())?;
    let done = finish(&report);
    warn_transient(&cli_filters);
    warn_capped(&report);
    warn_subsystems(&report, &setup.opts);
    done
}

/// Modèle imposé par `.nephila.toml` sous `model_override = "warn"` (REQ-196, DEC-159) : compte
/// les fichiers déjà enrichis avec un autre modèle ; avertit (A-13) ou, au-delà de
/// `PROJECT_MODEL_CONFIRM_FILES` sans `accept` ni `dry_run`, refuse (E-29). N'écrit rien d'autre.
pub(crate) fn check_project_model(
    repo: &Path,
    setup: &Setup,
    accept: bool,
    dry_run: bool,
) -> Result<()> {
    let Some(pm) = &setup.project_model else {
        return Ok(());
    };
    if !db_path(repo).is_file() {
        return Ok(());
    }
    let n = {
        let counted = Store::open_readonly(repo).and_then(|st| {
            st.count_enriched_with_other_model(
                &pm.project,
                setup.opts.enrich_code,
                &setup.opts.enrich_scope,
            )
        });
        counted.unwrap_or(0)
    };
    if n == 0 {
        return Ok(());
    }
    let file = repo.join(crate::filter::PROJECT_FILE);
    let head = |verb: &str| {
        format!(
            "{} : modèle `{}` au lieu de `{}`, {n} fichiers déjà enrichis {verb}",
            file.display(),
            crate::escape::data(&pm.project),
            crate::escape::data(&pm.personal)
        )
    };
    let remedy = format!(
        "écrivez model_override = \"deny\" dans la configuration personnelle pour garder `{}`",
        crate::escape::data(&pm.personal)
    );
    if n > crate::config::PROJECT_MODEL_CONFIRM_FILES && !accept && !dry_run {
        return Err(Error::Message(format!(
            "{} (au plus {} sans confirmation) ; relancez avec --accept-project-model ou {remedy}",
            head("seraient réenrichis"),
            crate::config::PROJECT_MODEL_CONFIRM_FILES
        )));
    }
    warn(&format!("{} ; {remedy}", head("seront réenrichis")));
    Ok(())
}

/// Filtres effectifs (DEC-045) : fichier de projet puis options de la commande.
pub(crate) fn effective_matcher(
    repo: &Path,
    project_filters: &Filters,
    cli_filters: &Filters,
) -> Result<Matcher> {
    project_filters
        .clone()
        .merge(cli_filters.clone())
        .compile(repo)
}

/// Avertissement A-3 : les options de filtre ne valent que pour la commande en cours. Écrit une
/// seule fois, après le bilan, seulement si une option a été donnée.
pub(crate) fn warn_transient(cli_filters: &Filters) {
    if cli_filters.is_active() {
        warn("--exclude et --exclude-type valent pour cette commande seulement : status et update sans option ne les appliquent pas (filtre durable : .nephila.toml)");
    }
}

/// Segment de couverture (REQ-235) : `<n>/<m> nœuds résumés`, seul endroit qui l'écrit.
pub(crate) fn coverage_segment(n: usize, m: usize) -> String {
    format!("{n}/{m} nœuds résumés")
}

/// Écrit la ligne de bilan sur stdout ; en cas d'échec (E-17), la commande s'arrête là.
pub(crate) fn write_report(out: &mut Out, report: &BuildReport, verb: &str) -> Result<()> {
    writeln!(out, "{}", report.line_for(verb))
}

/// Ligne `sous-systèmes :` de V-002, après le bilan ou `graphe à jour` ; rien sans bilan de passe
/// (sans `--subsystems`, sans configuration LLM).
pub(crate) fn write_subsystems(out: &mut Out, report: &BuildReport) -> Result<()> {
    match report
        .subsystems
        .as_ref()
        .or(report.subsystems_tally.as_ref())
    {
        Some(s) => writeln!(out, "{}", s.line()),
        None => Ok(()),
    }
}

/// Écrit les lignes de l'estimation sur stdout, après le bilan (V-002) ; E-17 si stdout est
/// fermée. Sans relevé (pas de `--dry-run`), n'écrit rien.
pub(crate) fn write_estimate(
    out: &mut Out,
    report: &BuildReport,
    opts: &IndexOptions,
    dry: Option<&DryRun>,
) -> Result<()> {
    write_estimate_lines(out, report.estimate.as_ref(), opts, dry)
}

/// Comme `write_estimate`, pour un relevé donné.
pub(crate) fn write_estimate_lines(
    out: &mut Out,
    estimate: Option<&Estimate>,
    opts: &IndexOptions,
    dry: Option<&DryRun>,
) -> Result<()> {
    let (Some(est), Some(dry)) = (estimate, dry) else {
        return Ok(());
    };
    for line in estimate::stdout_lines(est, opts.format_retries + 1, dry.no_config.as_deref()) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Erreurs de fin de commande ; une clé refusée est rendue telle quelle (REQ-054) ; un fichier resté en attente, un fichier à enrichir (LLM actif) ou un arrêt
/// de l'envoi rend une erreur (code non nul).
pub(crate) fn finish(report: &BuildReport) -> Result<()> {
    if let Some(e) = &report.refused {
        return Err(e.clone());
    }
    let mut problems = Vec::new();
    if let Some(cause) = &report.stop_cause {
        problems.push(format!("envoi interrompu : {cause}"));
    }
    if report.pending > 0 {
        problems.push(format!("{} fichier(s) en attente", report.pending));
    }
    // Les fichiers laissés par le plafond sont dits par A-10, pas par la ligne `erreur:`.
    let to_enrich = report.to_enrich.saturating_sub(report.left_by_cap);
    if report.llm_active && to_enrich > 0 {
        problems.push(format!("{to_enrich} fichier(s) à enrichir"));
    }
    // Communautés restées sans nom à cause d'un échec ou d'un arrêt ; celles du plafond sont dites par A-22.
    if let Some(s) = report.subsystems.as_ref().filter(|s| s.failed > 0) {
        problems.push(format!("{} communauté(s) à nommer", s.failed));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        let line = problems.join(" ; ");
        Err(if report.interrupted {
            Error::Interrupted(line)
        } else {
            Error::Message(line)
        })
    }
}

/// Avertissement A-10 : plafond d'appels atteint. Écrit une seule fois, après tous les autres
/// avertissements de fin de commande et avant la ligne `erreur:` que `main` écrit.
pub(crate) fn warn_capped(report: &BuildReport) {
    if let Some(m) = report.capped {
        let command = if report.batch {
            "nephila update --batch"
        } else {
            "nephila update"
        };
        warn(&format!(
            "{} : {} fichiers laissés à enrichir ; relancez {command} pour continuer ou augmentez max_calls",
            crate::index::enrich::cap_label(m),
            report.left_by_cap
        ));
    }
}

/// Avertissements de la passe `--subsystems` (design.md V-006), après A-10 et avant la ligne
/// `erreur:` : A-20 sans configuration LLM, sinon A-21 (fichiers à enrichir), sinon A-22 (plafond).
pub(crate) fn warn_subsystems(report: &BuildReport, opts: &IndexOptions) {
    if !opts.subsystems {
        return;
    }
    let command = if opts.batch {
        "nephila update --batch --subsystems"
    } else {
        "nephila update --subsystems"
    };
    if !report.llm_active {
        let create = crate::config::config_path()
            .map(|p| format!(" ; créez {}", p.display()))
            .unwrap_or_default();
        warn(&format!(
            "sous-systèmes non nommés : aucune configuration LLM{create}"
        ));
    } else if report.subsystems_skipped {
        warn(&format!(
            "sous-systèmes non nommés : {} fichiers restent à enrichir ; relancez {command}",
            report.to_enrich
        ));
    } else if let Some(s) = report.subsystems.as_ref().filter(|s| s.capped > 0) {
        warn(&format!(
            "{} : {} communautés laissées sans nom ; relancez nephila update --subsystems pour continuer ou augmentez max_calls",
            crate::index::enrich::cap_label(s.max_calls.unwrap_or(0)),
            s.capped
        ));
    }
}

/// Une passe de résolution, résumé global compris (une transaction), tracée.
fn resolve_pass(store: &mut Store) -> Result<resolve::ResolveStats> {
    let started = Instant::now();
    let stats = resolve::resolve(store).map_err(|e| pass_error(&e))?;
    let kind = match stats.pass {
        resolve::Pass::Full => "complète".to_string(),
        resolve::Pass::Incremental { reevaluated } => {
            format!("incrémentale, {reevaluated} références réévaluées")
        }
    };
    // La durée de `résolution:` ne compte pas le calcul des communautés (V-005).
    let resolution = started
        .elapsed()
        .saturating_sub(std::time::Duration::from_millis(stats.community_ms as u64));
    trace!(
        "résolution: {} ms, {} liens, {} non résolues ({kind})",
        resolution.as_millis(),
        stats.edges,
        stats.unresolved
    );
    trace!(
        "communautés: {} ms, {} communautés",
        stats.community_ms,
        stats.communities
    );
    Ok(stats)
}

/// Erreur E-18 (V-006) : phase nommée et remède, sur une seule ligne ; seul point qui pose ce texte.
fn pass_error(cause: &Error) -> Error {
    let text = cause.to_string();
    let first = text.lines().next().unwrap_or("").trim_end();
    Error::Message(format!(
        "résolution : {first} ; lancez nephila build pour reconstruire le graphe"
    ))
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
    matcher: &Matcher,
) -> Result<BuildReport> {
    build_with_dry_run(repo, opts, llm, None, _lock, matcher)
}

/// `build` avec, sous `--dry-run`, les réglages de l'estimation (REQ-184) ; `llm` est alors absent.
pub fn build_with_dry_run(
    repo: &Path,
    opts: &IndexOptions,
    llm: Option<&dyn Llm>,
    dry: Option<&DryRun>,
    _lock: &RepoLock,
    matcher: &Matcher,
) -> Result<BuildReport> {
    build_impl(repo, opts, llm, dry, matcher, scan::RETAIN_BUDGET)
}

/// `build` avec un budget de contenus retenus explicite (réglé par les tests seulement).
#[cfg(test)]
pub(crate) fn build_with_budget(
    repo: &Path,
    opts: &IndexOptions,
    llm: Option<&dyn Llm>,
    _lock: &RepoLock,
    matcher: &Matcher,
    budget: usize,
) -> Result<BuildReport> {
    build_impl(repo, opts, llm, None, matcher, budget)
}

fn build_impl(
    repo: &Path,
    opts: &IndexOptions,
    llm: Option<&dyn Llm>,
    dry: Option<&DryRun>,
    matcher: &Matcher,
    budget: usize,
) -> Result<BuildReport> {
    check_repo(repo)?;
    let mut store = Store::open_or_create(repo)?;
    let known = if db_path(repo).is_file() {
        store.files()?
    } else {
        Vec::new()
    };
    let scanned = scan::scan_with_budget(repo, &known, matcher, opts.jobs, budget)?;
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
    trace!(
        "parcours: {} fichiers{}{}",
        records.len(),
        matcher.default_trace_segment(scanned.default_dirs, scanned.default_files),
        matcher.trace_suffix(scanned.excluded)
    );
    store.reset_with_pending_files(&records)?;
    store.load_orphan_enrichments()?;
    if let Some(llm) = llm {
        store.set_enrich_model(llm.model())?;
        store.set_enrich_language(llm.summary_language())?;
        store.set_enrich_redact(opts.enrich_redact)?;
        store.set_enrich_code(opts.enrich_code)?;
        store.set_enrich_scope(&opts.enrich_scope)?;
    }
    if records.is_empty() {
        warn("aucun fichier pris en charge dans le dépôt : graphe vide");
    }

    let mut report = BuildReport {
        excluded: matcher.is_active().then_some(scanned.excluded),
        ..BuildReport::default()
    };
    let abs = abs_paths(&scanned.files);
    structure::analyze(
        repo,
        scanned.files,
        opts.jobs,
        &scanned.gradle_settings,
        &mut store,
        &mut report,
    )?;
    finalize(&mut store, llm, dry, opts, repo, &abs, &mut report)?;
    Ok(report)
}

/// Chemin converti (celui du graphe) vers chemin réel, pour les lectures (DEC-070).
pub(crate) fn abs_paths(files: &[crate::scan::ScannedFile]) -> HashMap<String, PathBuf> {
    files
        .iter()
        .map(|f| (f.path.clone(), f.abs.clone()))
        .collect()
}

/// Fin commune à `build` et `update` : résolution et résumé global (graphe interrogeable), puis
/// enrichissement des fichiers à enrichir si `llm` est présent, et bilan.
pub(crate) fn finalize(
    store: &mut Store,
    llm: Option<&dyn Llm>,
    dry: Option<&DryRun>,
    opts: &IndexOptions,
    repo: &Path,
    abs: &HashMap<String, PathBuf>,
    report: &mut BuildReport,
) -> Result<()> {
    // Estimation : le réglage de la configuration, sans l'enregistrer (REQ-265).
    if dry.is_some() {
        store.use_enrich_redact(opts.enrich_redact);
    }
    store.prune_enrichments()?;
    let mut stats = resolve_pass(store)?;
    // Un seul décompte du masquage pour toute la commande (REQ-266).
    let tally = opts.enrich_redact.then(RedactTally::new);
    report.llm_active = llm.is_some();
    if let Some(llm) = llm {
        let files = store.files_to_enrich()?;
        // Sans fichier à enrichir, le graphe n'a pas changé : pas de seconde résolution.
        if !files.is_empty() {
            let sources = enrich::Sources {
                repo,
                abs,
                redact: tally.as_ref(),
                batch: false,
            };
            if opts.batch {
                crate::index::batch::enrich_batched(sources, &files, opts, llm, store, report)?;
            } else {
                // A-19 : des fichiers vont être payés de nouveau pendant qu'un lot les attend.
                if !store.enrich_batches()?.is_empty() {
                    warn("lot du fournisseur enregistré : les fichiers à enrichir seront payés de nouveau ; appuyez sur Ctrl-C et relancez nephila update --batch pour le reprendre");
                }
                enrich::enrich_all(sources, &files, opts, llm, store, report)?;
            }
            stats = resolve_pass(store)?;
        }
    }
    // REQ-262 : noms de sous-systèmes en cache réappliqués aux communautés recalculées.
    crate::subsystems::reapply(store)?;
    report.nodes = store.count_nodes()?;
    report.edges = stats.edges;
    report.unresolved = stats.unresolved;
    report.pending = store
        .files()?
        .iter()
        .filter(|f| f.state == FileState::Pending)
        .count();
    report.to_enrich = store.count_files_to_enrich()?;
    // Plus rien à enrichir : les lots du fournisseur enregistrés ne servent plus (REQ-215).
    if llm.is_some() && report.to_enrich == 0 {
        store.set_enrich_batches(&[])?;
    }
    if llm.is_some() || dry.is_some() {
        report.out_of_scope = store.count_out_of_scope(opts.enrich_code, &opts.enrich_scope)?;
    }
    if opts.subsystems {
        crate::subsystems::after_enrichment(store, llm, opts, report)?;
    }
    if let Some(dry) = dry {
        report.estimate = Some(estimate_enrichment(store, dry, opts, repo, abs)?);
    }
    Ok(())
}

/// Estimation de l'enrichissement sur le graphe écrit (REQ-184, DEC-148) : fichiers de la portée
/// selon les réglages effectifs, relevé, puis une trace par fichier envoyé seul. Rien n'est écrit.
pub(crate) fn estimate_enrichment(
    store: &Store,
    dry: &DryRun,
    opts: &IndexOptions,
    repo: &Path,
    abs: &HashMap<String, PathBuf>,
) -> Result<Estimate> {
    let files = store.files_to_enrich_for(
        dry.model.as_deref(),
        Some(&dry.language),
        opts.enrich_code,
        &opts.enrich_scope,
    )?;
    let ids = enrich::nodes_by_file(store)?;
    // Sans modèle, rien n'est réutilisé (REQ-228).
    let kept = match &dry.model {
        Some(model) => enrich::KeptCalls {
            results: store.call_results()?,
            model: model.clone(),
            language: dry.language.clone(),
            versions: store.prompt_versions(),
        },
        None => enrich::KeptCalls {
            results: HashMap::new(),
            model: String::new(),
            language: dry.language.clone(),
            versions: store.prompt_versions(),
        },
    };
    let tally = opts.enrich_redact.then(RedactTally::new);
    let est = enrich::estimate(
        enrich::Sources {
            repo,
            abs,
            redact: tally.as_ref(),
            batch: false,
        },
        &files,
        &ids,
        &dry.language,
        opts.batch_files,
        opts.max_chunk_chars,
        &kept,
        opts.nodes_per_call,
    );
    for line in estimate::trace_lines(&est) {
        trace!("{line}");
    }
    if let Some(line) = tally.as_ref().and_then(RedactTally::line) {
        trace!("{line}");
    }
    Ok(est)
}

#[cfg(test)]
mod tests {

    fn capped_report(left: usize, to_enrich: usize) -> BuildReport {
        BuildReport {
            llm_active: true,
            to_enrich,
            capped: Some(3),
            left_by_cap: left,
            ..BuildReport::default()
        }
    }

    fn message(r: Result<()>) -> String {
        match r {
            Err(Error::Message(m)) => m,
            other => panic!("attendu une erreur, obtenu {other:?}"),
        }
    }

    fn pm_setup(project: Option<&str>) -> Setup {
        Setup {
            opts: IndexOptions::default(),
            llm: None,
            dry_run: None,
            project_model: project.map(|p| ProjectModel {
                project: p.into(),
                personal: "m-perso".into(),
            }),
        }
    }

    fn pm_repo(n: usize) -> tempfile::TempDir {
        use crate::model::{FileRecord, FileState};
        let dir = tempfile::tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        let paths: Vec<String> = (0..n).map(|i| format!("d{i}.md")).collect();
        let files: Vec<_> = paths
            .iter()
            .map(|p| FileRecord {
                path: p.clone(),
                hash: "h".into(),
                size: 1,
                mtime: 1,
                recorded_at: 0,
                state: FileState::Indexed,
            })
            .collect();
        st.insert_pending_files(&files).unwrap();
        for p in &paths {
            st.replace_structure(p, &[], &[]).unwrap();
            st.apply_enrichment(p, "h", "m-perso", "français", &[], &[], &[])
                .unwrap();
        }
        drop(st);
        dir
    }

    #[test]
    fn req196_check_project_model_nothing_to_say() {
        let dir = pm_repo(3);
        // Aucun modèle de projet, ou aucun fichier à refaire : rien.
        assert!(check_project_model(dir.path(), &pm_setup(None), false, false).is_ok());
        assert!(check_project_model(dir.path(), &pm_setup(Some("m-perso")), false, false).is_ok());
        // Graphe absent.
        let empty = tempfile::tempdir().unwrap();
        assert!(check_project_model(empty.path(), &pm_setup(Some("m-p")), false, false).is_ok());
        // Warn : pas d'erreur.
        assert!(check_project_model(dir.path(), &pm_setup(Some("m-p")), false, false).is_ok());
    }

    #[test]
    fn req196_ac3_check_project_model_refuses_above_the_limit() {
        use crate::config::PROJECT_MODEL_CONFIRM_FILES as LIMIT;
        let dir = pm_repo(LIMIT);
        assert!(check_project_model(dir.path(), &pm_setup(Some("m-p")), false, false).is_ok());
        let dir = pm_repo(LIMIT + 1);
        let m = message(check_project_model(
            dir.path(),
            &pm_setup(Some("m-p")),
            false,
            false,
        ));
        assert_eq!(
            m,
            format!(
                "{}/.nephila.toml : modèle `m-p` au lieu de `m-perso`, 51 fichiers déjà enrichis seraient réenrichis (au plus 50 sans confirmation) ; relancez avec --accept-project-model ou écrivez model_override = \"deny\" dans la configuration personnelle pour garder `m-perso`",
                dir.path().display()
            )
        );
        assert!(check_project_model(dir.path(), &pm_setup(Some("m-p")), true, false).is_ok());
        assert!(check_project_model(dir.path(), &pm_setup(Some("m-p")), false, true).is_ok());
    }

    #[test]
    fn req196_check_project_model_escapes_names() {
        let dir = pm_repo(51);
        let m = message(check_project_model(
            dir.path(),
            &pm_setup(Some("m\u{1b}]0;x\u{7}")),
            false,
            false,
        ));
        assert!(!m.contains('\u{1b}') && !m.contains('\u{7}'), "{m}");
        assert!(m.contains("`m\\u{001B}]0;x\\u{0007}`"), "{m}");
    }

    #[test]
    fn req187_finish_cap_alone_is_ok() {
        assert!(finish(&capped_report(4, 4)).is_ok());
    }

    #[test]
    fn req187_ac2_finish_cap_and_failed_file_counts_only_the_failure() {
        let m = message(finish(&capped_report(4, 5)));
        assert_eq!(m, "1 fichier(s) à enrichir");
    }

    #[test]
    fn req187_ac3_finish_cap_and_pending_file() {
        let mut r = capped_report(4, 4);
        r.pending = 1;
        let m = message(finish(&r));
        assert_eq!(m, "1 fichier(s) en attente");
    }

    #[test]
    fn req187_finish_cap_and_stop_cause_without_count() {
        let mut r = capped_report(4, 4);
        r.stop_cause = Some("panne réseau répétée".into());
        let m = message(finish(&r));
        assert_eq!(m, "envoi interrompu : panne réseau répétée");
        assert!(!m.contains("plafond") && !m.contains("max_calls"));
    }

    #[test]
    fn req191_finish_interrupted_with_files_to_enrich_exits_130() {
        let r = BuildReport {
            llm_active: true,
            to_enrich: 4,
            interrupted: true,
            stop_cause: Some(crate::index::enrich::CAUSE_INTERRUPT.cause.into()),
            ..BuildReport::default()
        };
        let e = finish(&r).unwrap_err();
        assert!(matches!(e, Error::Interrupted(_)));
        assert_eq!(
            e.to_string(),
            "envoi interrompu : Ctrl-C reçu : relancez nephila update pour continuer ; 4 fichier(s) à enrichir"
        );
        assert_eq!(e.exit_code(), 130);
    }

    #[test]
    fn req191_finish_interrupted_with_only_cap_files_has_no_count() {
        let mut r = capped_report(4, 4);
        r.interrupted = true;
        r.stop_cause = Some(crate::index::enrich::CAUSE_INTERRUPT.cause.into());
        let e = finish(&r).unwrap_err();
        assert_eq!(
            e.to_string(),
            "envoi interrompu : Ctrl-C reçu : relancez nephila update pour continuer"
        );
        assert_eq!(e.exit_code(), 130);
    }

    #[test]
    fn req191_finish_not_interrupted_stays_message_with_code_1() {
        let r = BuildReport {
            llm_active: true,
            to_enrich: 2,
            ..BuildReport::default()
        };
        let e = finish(&r).unwrap_err();
        assert!(matches!(e, Error::Message(_)));
        assert_eq!(e.exit_code(), 1);
    }

    #[test]
    fn req187_finish_without_cap_unchanged() {
        let r = BuildReport {
            llm_active: true,
            to_enrich: 2,
            ..BuildReport::default()
        };
        assert_eq!(message(finish(&r)), "2 fichier(s) à enrichir");
    }
    fn sink() -> Out {
        Out::new(Box::new(Vec::new()))
    }

    use super::*;
    use crate::scan::reads;

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dest = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &dest);
            } else {
                std::fs::copy(e.path(), dest).unwrap();
            }
        }
    }

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel)
    }

    /// Export normalisé (DEC-081) : `unresolved` trié, doublons conservés.
    fn normalized_export(repo: &Path) -> String {
        let store = Store::open_readonly(repo).unwrap();
        let mut doc = oracle_build_json(&store).unwrap();
        if let Some(list) = doc.get_mut("unresolved").and_then(|u| u.as_array_mut()) {
            let key = |v: &serde_json::Value| {
                let f = |k: &str| v.get(k).map(|x| x.to_string()).unwrap_or_default();
                (f("source"), f("kind"), f("target"))
            };
            list.sort_by_key(key);
        }
        serde_json::to_string(&doc).unwrap()
    }

    fn indexed_paths(repo: &Path) -> Vec<String> {
        let store = Store::open_readonly(repo).unwrap();
        store.files().unwrap().into_iter().map(|f| f.path).collect()
    }

    fn do_build(repo: &Path, budget: usize) -> BuildReport {
        let lock = RepoLock::acquire(repo).unwrap();
        build_with_budget(
            repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
            budget,
        )
        .unwrap()
    }

    fn do_update(repo: &Path) {
        let lock = RepoLock::acquire(repo).unwrap();
        crate::index::update::update(
            repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap();
    }

    /// REQ-184 : segment de fin du bilan sous `--dry-run`, avant le segment des filtres.
    #[test]
    fn req184_bilan_segment_estime_sans_envoi_avec_et_sans_filtre() {
        let mut r = BuildReport {
            estimate: Some(Estimate::default()),
            ..BuildReport::default()
        };
        assert!(
            r.line()
                .ends_with("0 fichiers à enrichir, enrichissement : estimé sans envoi"),
            "{}",
            r.line()
        );
        r.excluded = Some(3);
        assert!(
            r.line()
                .ends_with("enrichissement : estimé sans envoi, 3 fichiers exclus par les filtres"),
            "{}",
            r.line()
        );
        r.estimate = None;
        assert!(r
            .line()
            .contains("enrichissement : désactivé, 3 fichiers exclus"));
    }

    /// REQ-184 : `build` sous `--dry-run` écrit le graphe de `--no-llm`, rend l'estimation, sans
    /// client ni écriture de `meta`.
    #[test]
    fn req184_build_dry_run_graphe_de_no_llm_et_estimation() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(
                d.join("guide.md"),
                "# Guide\n\nTexte.\n\n## Suite\n\nAutre.\n",
            )
            .unwrap();
            std::fs::write(d.join("C.java"), "public class C {}\n").unwrap();
        }
        do_build(&b, scan::RETAIN_BUDGET);
        let lock = RepoLock::acquire(&a).unwrap();
        let dry = DryRun {
            model: Some("m".to_string()),
            language: "français".to_string(),
            no_config: None,
        };
        let report = build_with_dry_run(
            &a,
            &IndexOptions::default(),
            None,
            Some(&dry),
            &lock,
            &Matcher::none(),
        )
        .unwrap();
        assert_eq!(normalized_export(&a), normalized_export(&b));
        let est = report.estimate.as_ref().expect("estimation");
        assert!(est.files > 0 && est.calls > 0 && est.bytes > 0, "{est:?}");
        assert!(report.line().contains("enrichissement : estimé sans envoi"));
        let store = Store::open_readonly(&a).unwrap();
        assert_eq!(store.enrich_model().unwrap(), None);
        assert!(!report.llm_active);
    }

    /// AC-073-1 : chaque fichier pris en charge est lu une fois ; graphe identique à celui d'un
    /// budget nul (aucun contenu retenu : deux lectures par fichier).
    #[test]
    fn req073_ac1_each_file_read_once_same_graph_as_zero_budget() {
        let _d = crate::diag::scope();
        for rel in ["maven3/repo", "gradle/groovy", "js/mixed"] {
            let dir = tempfile::tempdir().unwrap();
            let one = dir.path().join("one");
            let zero = dir.path().join("zero");
            copy_dir(&fixture(rel), &one);
            copy_dir(&fixture(rel), &zero);

            reads::reset();
            do_build(&one, scan::RETAIN_BUDGET);
            let paths = indexed_paths(&one);
            assert!(!paths.is_empty(), "{rel}");
            let got = reads::all();
            for p in &paths {
                assert_eq!(got.get(p), Some(&1), "{rel} : {p} lu {:?} fois", got.get(p));
            }
            assert_eq!(reads::kind(crate::scan::ReadKind::Extract), 0, "{rel}");
            assert_eq!(reads::kind(crate::scan::ReadKind::Settings), 0, "{rel}");

            reads::reset();
            do_build(&zero, 0);
            let got0 = reads::all();
            for p in &paths {
                assert!(got0.get(p).copied().unwrap_or(0) >= 2, "{rel} : {p}");
            }
            assert_eq!(normalized_export(&one), normalized_export(&zero), "{rel}");
        }
    }

    /// AC-073-2 : second build puis update.
    #[test]
    fn req073_ac2_second_build_and_update_read_each_file_once() {
        let _d = crate::diag::scope();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        copy_dir(&fixture("maven3/repo"), &repo);
        do_build(&repo, scan::RETAIN_BUDGET);
        let paths = indexed_paths(&repo);

        reads::reset();
        do_build(&repo, scan::RETAIN_BUDGET);
        let got = reads::all();
        for p in &paths {
            assert_eq!(got.get(p), Some(&1), "second build : {p}");
        }

        let java = paths.iter().find(|p| p.ends_with(".java")).unwrap().clone();
        let mut text = std::fs::read_to_string(repo.join(&java)).unwrap();
        text.push_str("\n// modifié\n");
        std::fs::write(repo.join(&java), text).unwrap();
        std::fs::write(repo.join("G.java"), "package p;\npublic class G {}\n").unwrap();
        reads::reset();
        do_update(&repo);
        let got = reads::all();
        assert_eq!(got.get(&java), Some(&1), "fichier modifié");
        assert_eq!(got.get("G.java"), Some(&1), "fichier ajouté");
        for (p, n) in &got {
            assert!(*n <= 1, "{p} lu {n} fois");
        }
    }

    /// AC-073-3 : budget de 1 000 octets, 5 fichiers de 400 octets.
    #[test]
    fn req073_ac3_budget_bounds_retained_contents() {
        let d = crate::diag::scope();
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small");
        let big = dir.path().join("big");
        for r in [&small, &big] {
            std::fs::create_dir_all(r).unwrap();
            for i in 0..5 {
                let mut t = format!("# Titre {i}\n\n");
                while t.len() < 399 {
                    t.push('x');
                }
                t.push('\n');
                assert_eq!(t.len(), 400);
                std::fs::write(r.join(format!("d{i}.md")), t).unwrap();
            }
        }
        reads::reset();
        d.set_retained_peak(0);
        do_build(&small, 1_000);
        let peak = d.retained_peak();
        assert!(peak > 0 && peak <= 1_000, "pic {peak}");
        assert_eq!(reads::kind(crate::scan::ReadKind::Hash), 5);
        assert_eq!(reads::kind(crate::scan::ReadKind::Extract), 3);
        do_build(&big, scan::RETAIN_BUDGET);
        assert_eq!(normalized_export(&small), normalized_export(&big));
    }

    /// AC-073-4 : contenu réécrit (même taille) entre `scan` et `analyze`.
    #[test]
    fn req073_ac4_analysis_uses_the_content_of_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let before = "package p;\npublic class A {}\n";
        let after = "package p;\npublic class B {}\n";
        assert_eq!(before.len(), after.len());
        std::fs::write(repo.join("A.java"), before).unwrap();
        let scanned = scan::scan(repo, &[], &Matcher::none(), 1).unwrap();
        let mut store = Store::open_or_create(repo).unwrap();
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
        store.reset_with_pending_files(&records).unwrap();
        std::fs::write(repo.join("A.java"), after).unwrap();
        let mut report = BuildReport::default();
        structure::analyze(
            repo,
            scanned.files,
            1,
            &scanned.gradle_settings,
            &mut store,
            &mut report,
        )
        .unwrap();
        let ids: Vec<String> = store.nodes().unwrap().into_iter().map(|n| n.id).collect();
        assert!(ids.contains(&"type:p.A".to_string()), "{ids:?}");
        assert!(!ids.contains(&"type:p.B".to_string()), "{ids:?}");
        let rec = store.files().unwrap().remove(0);
        assert_eq!(
            rec.hash,
            blake3::hash(before.as_bytes()).to_hex().to_string()
        );
        drop(store);

        // Changement de date : l'update suivant analyse le nouvel état.
        let f = std::fs::File::options()
            .write(true)
            .open(repo.join("A.java"))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();
        drop(f);
        do_update(repo);
        let store = Store::open_readonly(repo).unwrap();
        let ids: Vec<String> = store.nodes().unwrap().into_iter().map(|n| n.id).collect();
        assert!(ids.contains(&"type:p.B".to_string()), "{ids:?}");
        assert!(!ids.contains(&"type:p.A".to_string()), "{ids:?}");
        let rec = store.files().unwrap().remove(0);
        assert_eq!(
            rec.hash,
            blake3::hash(after.as_bytes()).to_hex().to_string()
        );
    }

    /// Export de la 0.23.2 (`build_json` de HEAD avant REQ-080), résolution complète comprise :
    /// oracle d'AC-080-1 et d'AC-080-2.
    fn oracle_build_json(store: &Store) -> Result<serde_json::Value> {
        use serde_json::json;
        let nodes: Vec<serde_json::Value> = store
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
        let edges: Vec<serde_json::Value> = store
            .edges()?
            .into_iter()
            .map(|e| json!({"source": e.source, "target": e.target, "kind": e.kind.as_str()}))
            .collect();
        let unresolved: Vec<serde_json::Value> = resolve::unresolved(store)?
            .into_iter()
            .map(|r| json!({"source": r.source, "kind": r.kind.as_str(), "target": r.target_name}))
            .collect();
        Ok(
            json!({"version": crate::export::EXPORT_VERSION, "nodes": nodes, "edges": edges, "unresolved": unresolved}),
        )
    }

    /// Dépôts de REQ-065 (liste de `tests/sqlite_lot3.rs`) : dossier et dossier du mock.
    const REQ065_REPOS: &[(&str, Option<&str>)] = &[
        ("maven3/repo", Some("maven3/mock")),
        ("maven2", Some("mock")),
        ("gradle/edge", None),
        ("gradle/groovy", None),
        ("gradle/kotlin", None),
        ("ivy", None),
        ("data/repo", Some("data/mock")),
        ("js/mixed", Some("js/mock")),
        ("langs/mixed", None),
    ];

    /// `build` avec le mock (enrichissement du code compris) quand `mock` est donné.
    fn build_with_mock(repo: &Path, mock: Option<&Path>) {
        let llm = mock.map(|d| {
            crate::llm::mock::MockLlm::new(crate::llm::mock::MockOptions {
                dir: Some(d.to_path_buf()),
                ..Default::default()
            })
        });
        let opts = IndexOptions {
            enrich_code: true,
            ..IndexOptions::default()
        };
        let lock = RepoLock::acquire(repo).unwrap();
        build_with_budget(
            repo,
            &opts,
            llm.as_ref().map(|l| l as &dyn Llm),
            &lock,
            &Matcher::none(),
            scan::RETAIN_BUDGET,
        )
        .unwrap();
    }

    fn graph_db_hash(repo: &Path) -> String {
        let bytes = std::fs::read(crate::store::db_path(repo)).unwrap();
        blake3::hash(&bytes).to_hex().to_string()
    }

    /// Schéma 7 de la 0.21.0 (copie de l'assistant du module de tests du stockage).
    const SCHEMA7_DOWNGRADE: &str = "BEGIN;
        DROP TABLE node_communities;
        DROP TABLE communities;
        DROP INDEX idx_refs_key;
        DROP INDEX idx_refs_head;
        DROP INDEX idx_refs_unresolved;
        ALTER TABLE refs DROP COLUMN target_key;
        ALTER TABLE refs DROP COLUMN target_head;
        ALTER TABLE refs DROP COLUMN unresolved;
        DELETE FROM meta WHERE key = 'resolution';
        UPDATE meta SET value = '7' WHERE key = 'schema_version';
        PRAGMA user_version = 7;
        COMMIT;";

    fn exec(repo: &Path, sql: &str) {
        rusqlite::Connection::open(crate::store::db_path(repo))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    /// AC-149-2 : retire du texte d'un export les clés `communities` (premier niveau, en tête) et
    /// `community` (en tête de chaque nœud). Les clés n'apparaissent avec des guillemets nus que
    /// comme clés : dans une chaîne, les guillemets sont échappés.
    fn strip_community_keys(text: &str) -> String {
        let rest = text
            .strip_prefix("{\"communities\":[")
            .expect("l'export commence par la clé communities");
        let (_, rest) = rest
            .split_once("],\"edges\":[")
            .expect("la clé edges suit communities");
        let mut out = String::from("{\"edges\":[");
        let mut rest = rest;
        while let Some(i) = rest.find("{\"community\":") {
            out.push_str(&rest[..=i]);
            let after = &rest[i + "{\"community\":".len()..];
            let (value, tail) = after.split_once(',').expect("une virgule suit community");
            assert!(
                value == "null" || value.bytes().all(|b| b.is_ascii_digit()),
                "{value}"
            );
            rest = tail;
        }
        out.push_str(rest);
        out
    }

    /// AC-080-1 : l'export en flux est celui de la 0.23.2, octet pour octet (flux en mémoire, puis
    /// fichier écrit par la commande), pour les dépôts de REQ-065 et pour un graphe vide.
    #[test]
    fn req080_ac1_stream_equals_the_0_23_2_export_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cases: Vec<(String, PathBuf)> = Vec::new();
        for (i, (rel, mock)) in REQ065_REPOS.iter().enumerate() {
            let repo = tmp.path().join(format!("r{i}"));
            copy_dir(&fixture(rel), &repo);
            let mock = mock.map(fixture);
            build_with_mock(&repo, mock.as_deref());
            cases.push((rel.to_string(), repo));
        }
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        drop(Store::open_or_create(&empty).unwrap());
        cases.push(("graphe vide".into(), empty));
        for (name, repo) in cases {
            let store = Store::open_readonly(&repo).unwrap();
            let expected = oracle_build_json(&store).unwrap().to_string();
            let mut out: Vec<u8> = Vec::new();
            crate::export::write_json(&store, &mut out).unwrap();
            let streamed = String::from_utf8(out).unwrap();
            assert_eq!(strip_community_keys(&streamed), expected, "{name}");
            drop(store);
            let file = tmp.path().join("out.json");
            crate::export::run(
                &repo,
                Some(&file),
                crate::cli::ExportFormat::Json,
                &mut sink(),
            )
            .unwrap();
            assert_eq!(
                strip_community_keys(
                    std::fs::read_to_string(&file)
                        .unwrap()
                        .trim_end_matches('\n')
                ),
                expected,
                "{name} (fichier)"
            );
            assert!(std::fs::read_to_string(&file).unwrap().ends_with("}\n"));
        }
    }

    /// AC-080-2 : une passe de résolution seulement si le graphe n'est pas `à jour` de schéma 8 ;
    /// jamais pour la page HTML ; le graphe n'est jamais modifié.
    #[test]
    fn req080_ac2_export_resolves_only_a_graph_that_is_not_current() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixture("maven2"), &repo);
        build_with_mock(&repo, Some(&fixture("mock")));
        let json = tmp.path().join("out.json");
        let html = tmp.path().join("out.html");
        let slug = crate::export::slug(&repo);
        let states: [(&str, Option<&str>, usize); 3] = [
            ("à jour", None, 0),
            (
                "à résoudre",
                Some("UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'"),
                1,
            ),
            ("schéma 7", Some(SCHEMA7_DOWNGRADE), 1),
        ];
        for (label, change, passes) in states {
            if let Some(sql) = change {
                exec(&repo, sql);
            }
            let before = graph_db_hash(&repo);
            resolve::passes::reset();
            crate::export::run(
                &repo,
                Some(&json),
                crate::cli::ExportFormat::Json,
                &mut sink(),
            )
            .unwrap();
            assert_eq!(resolve::passes::count(), passes, "{label} : json");
            resolve::passes::reset();
            crate::export::run(
                &repo,
                Some(&html),
                crate::cli::ExportFormat::Html,
                &mut sink(),
            )
            .unwrap();
            assert_eq!(resolve::passes::count(), 0, "{label} : html");
            assert_eq!(graph_db_hash(&repo), before, "{label} : graph.db modifié");
            let store = Store::open_readonly(&repo).unwrap();
            let doc = oracle_build_json(&store).unwrap();
            let written = std::fs::read_to_string(&json).unwrap();
            assert!(written.ends_with("}\n"), "{label}");
            assert_eq!(
                strip_community_keys(written.trim_end_matches('\n')),
                doc.to_string(),
                "{label}"
            );
            assert_eq!(
                std::fs::read_to_string(&html).unwrap(),
                // La page porte aussi les communautés (REQ-150) : l'oracle n'en a pas, l'export les donne.
                format!(
                    "{}\n",
                    crate::view::render(
                        &slug,
                        &serde_json::from_str::<serde_json::Value>(written.trim_end()).unwrap()
                    )
                ),
                "{label} : page"
            );
        }
    }

    /// Écrivain qui accepte `limit` octets puis échoue (AC-080-4, AC-080-7).
    struct FailAfter {
        limit: usize,
        written: usize,
    }

    impl std::io::Write for FailAfter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.written >= self.limit {
                return Err(std::io::Error::other("disque plein simulé"));
            }
            let n = buf.len().min(self.limit - self.written);
            self.written += n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// AC-080-4 et AC-080-7 : une écriture interrompue donne une erreur rendue sur une ligne, et le
    /// fichier commencé est supprimé (JSON comme HTML) ; vers stdout, le texte d'E-17.
    #[test]
    fn req080_ac4_ac7_failing_writer_removes_the_file_and_renders_the_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixture("maven2"), &repo);
        build_with_mock(&repo, Some(&fixture("mock")));
        let wrap: &dyn Fn(Box<dyn std::io::Write>) -> Box<dyn std::io::Write> = &|_real| {
            Box::new(FailAfter {
                limit: 100,
                written: 0,
            })
        };
        for format in [
            crate::cli::ExportFormat::Json,
            crate::cli::ExportFormat::Html,
        ] {
            let file = tmp.path().join("out.export");
            let err = crate::export::run_wrapped(&repo, Some(&file), format, wrap, &mut sink())
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "écriture de {} impossible : disque plein simulé",
                    file.display()
                ),
                "{format:?}"
            );
            assert!(!file.exists(), "{format:?} : fichier laissé");
            let err =
                crate::export::run_wrapped(&repo, Some(Path::new("-")), format, wrap, &mut sink())
                    .unwrap_err();
            assert_eq!(
                err.to_string(),
                "écriture sur stdout impossible : disque plein simulé",
                "{format:?}"
            );
        }
        // Un export du même nom qui existait est perdu (DEC-092, sans fichier temporaire).
        let file = tmp.path().join("old.json");
        std::fs::write(&file, "ancien").unwrap();
        crate::export::run_wrapped(
            &repo,
            Some(&file),
            crate::cli::ExportFormat::Json,
            wrap,
            &mut sink(),
        )
        .unwrap_err();
        assert!(!file.exists());
    }

    /// AC-089-2 : une panne de la base pendant la passe, complète comme incrémentale, donne le texte
    /// E-18 exact ; `build` et `update` passent par `resolve_pass` (lecture de `finalize`).
    #[test]
    fn req089_ac2_failing_pass_renders_the_named_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixture("maven3/repo"), &repo);
        build_with_mock(&repo, None);
        let trigger = "CREATE TRIGGER boom BEFORE INSERT ON edges \
            BEGIN SELECT RAISE(ABORT, 'panne simulée'); END;";
        exec(&repo, trigger);
        // SQLite rend `panne simulée` seul pour un RAISE(ABORT) : le texte exact en découle.
        let expect = |err: Error| {
            let text = err.to_string();
            assert!(
                text.starts_with("résolution : base du graphe : ")
                    && text.contains("panne simulée")
                    && text.ends_with(" ; lancez nephila build pour reconstruire le graphe")
                    && !text.contains('\n'),
                "{text}"
            );
            assert_eq!(text.matches(" ; lancez").count(), 1, "{text}");
        };
        // Passe complète : une référence à lier est écrite, graphe à résoudre (aucun journal).
        exec(
            &repo,
            "UPDATE refs SET unresolved = 1 WHERE id = (SELECT MIN(id) FROM refs);
             UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'",
        );
        let mut store = Store::open_or_create(&repo).unwrap();
        crate::store::reset_diagnostics();
        expect(resolve_pass(&mut store).unwrap_err());
        let d = crate::store::diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (1, 0));
        drop(store);
        // Passe incrémentale : graphe résolu, un fichier modifié, `update`.
        exec(&repo, "DROP TRIGGER boom");
        let mut store = Store::open_or_create(&repo).unwrap();
        resolve_pass(&mut store).unwrap();
        drop(store);
        exec(&repo, trigger);
        let java = repo.join("core/src/main/java/com/acme/core/OrderService.java");
        let mut text = std::fs::read_to_string(&java).unwrap();
        text.push_str("\nclass Added { void run() { new OrderService(); } }\n");
        std::fs::write(&java, text).unwrap();
        let lock = RepoLock::acquire(&repo).unwrap();
        crate::store::reset_diagnostics();
        let err = crate::index::update::update(
            &repo,
            &IndexOptions::default(),
            None,
            &lock,
            &Matcher::none(),
        )
        .unwrap_err();
        expect(err);
        let d = crate::store::diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (0, 1));
    }
}
