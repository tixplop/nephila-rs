//! Arbre de commandes clap.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::error::Result;
use crate::filter::{compile_pattern, FileType, Filters, TYPE_LIST};

#[derive(Debug, Parser)]
#[command(
    name = "nephila",
    version,
    about = "Graphe de connaissances d'un dépôt"
)]
pub struct Cli {
    /// Racine du dépôt à analyser
    #[arg(short = 'C', long = "repo", global = true, default_value = ".")]
    pub repo: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct JsonOpt {
    /// Sortie au format JSON
    #[arg(long)]
    pub json: bool,
}

/// Entier strictement positif (REQ-028).
fn parse_batch_files(s: &str) -> std::result::Result<usize, String> {
    match s.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!("`{s}` n'est pas un entier strictement positif")),
    }
}

/// Entier positif ou nul tenant sur 32 bits (REQ-038).
fn parse_format_retries(s: &str) -> std::result::Result<u32, String> {
    s.parse::<u32>()
        .map_err(|_| format!("`{s}` n'est pas un entier positif ou nul"))
}

const FORMAT_RETRIES_HELP: &str = "Nombre de nouveaux essais d'un appel LLM dont la réponse est non conforme (remplace `format_retries` de la configuration ; défaut 2 ; 0 = aucun nouvel essai)";

/// Motif `--exclude` : compilé seul, pour que l'erreur d'usage le nomme (REQ-037).
fn parse_exclude(s: &str) -> std::result::Result<String, String> {
    compile_pattern(std::path::Path::new("/"), s)?;
    Ok(s.to_string())
}

/// Valeur de `--exclude-type` : types séparés par des virgules, découpés ici pour que clap
/// nomme la valeur reçue entière (REQ-037).
fn parse_exclude_types(s: &str) -> std::result::Result<Vec<FileType>, String> {
    s.split(',')
        .map(|item| {
            if item.is_empty() {
                Err("type vide dans la liste (séparer les types par une seule virgule, ex. json,yaml)"
                    .to_string())
            } else {
                FileType::parse(item)
                    .ok_or_else(|| format!("type inconnu `{item}` (types valides : {TYPE_LIST})"))
            }
        })
        .collect()
}

const FILTERS_HELP: &str =
    "Filtres : --exclude et --exclude-type valent pour cette commande seulement.
Filtres durables (lus aussi par status et query) : fichier .nephila.toml à la racine du dépôt
  exclude = [\"docs/legacy/\", \"*.min.js\"]
  exclude_types = [\"json\"]
Réglages LLM du dépôt, dans le même fichier (prioritaires sur la configuration personnelle) :
  model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries
Exemple : nephila build --exclude '*.min.js' --exclude-type json,yaml";

#[derive(Debug, Args)]
#[command(after_help = FILTERS_HELP)]
pub struct IndexOpts {
    /// N'appelle pas le LLM (la configuration n'est pas lue) : structure seule, sans enrichissement
    #[arg(long)]
    pub no_llm: bool,
    /// Enrichit aussi le code source et les fichiers de build et de données, pas seulement les documents Markdown (remplace `enrich_code` de la configuration ; défaut : documents seulement)
    #[arg(long)]
    pub enrich_code: bool,
    /// Traces synthétiques des étapes et des appels LLM sur stderr (préfixe `trace:` ; incompatible avec --show-progress)
    #[arg(short, long)]
    pub verbose: bool,
    /// Avancement des phases extraction et enrichissement sur stderr, par paliers de 10 % (préfixe `progression:` ; incompatible avec --verbose)
    #[arg(long, conflicts_with = "verbose")]
    pub show_progress: bool,
    /// Nombre maximal de fichiers par appel d'enrichissement (remplace `batch_files` de la configuration)
    #[arg(long, value_name = "N", value_parser = parse_batch_files, allow_hyphen_values = true)]
    pub batch_files: Option<usize>,
    #[arg(help = FORMAT_RETRIES_HELP, long, value_name = "N", value_parser = parse_format_retries, allow_hyphen_values = true)]
    pub format_retries: Option<u32>,
    /// Exclut de l'analyse les fichiers qui correspondent au motif (syntaxe .gitignore, relatif à la racine du dépôt ; répétable ; !motif réinclut un fichier exclu par un motif précédent)
    #[arg(long, value_name = "MOTIF", action = clap::ArgAction::Append, value_parser = parse_exclude)]
    pub exclude: Vec<String>,
    /// Exclut de l'analyse un ou plusieurs types de fichiers, séparés par des virgules (répétable). Types : markdown, java, kotlin, groovy, scala, javascript, typescript, sql, xml, json, yaml, maven, ivy, gradle, npm
    #[arg(long, value_name = "TYPES", action = clap::ArgAction::Append, value_parser = parse_exclude_types)]
    pub exclude_type: Vec<Vec<FileType>>,
}

impl IndexOpts {
    /// Filtres donnés en ligne de commande.
    pub fn filters(&self) -> Filters {
        Filters {
            patterns: self.exclude.clone(),
            types: self.exclude_type.iter().flatten().copied().collect(),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Reconstruit entièrement le graphe
    Build(IndexOpts),
    /// Met à jour le graphe pour les fichiers modifiés
    Update(IndexOpts),
    /// Affiche l'état de l'index
    Status(JsonOpt),
    /// Interroge le graphe
    #[command(subcommand)]
    Query(Query),
    /// Exporte le graphe
    Export {
        /// Fichier de sortie (défaut : <slug>.json ou <slug>.html dans le dossier courant ; - pour stdout)
        #[arg(long)]
        output: Option<PathBuf>,
        /// Format de sortie
        #[arg(long, value_enum, default_value_t = ExportFormat::Json)]
        format: ExportFormat,
    },
    /// Résumé du dépôt
    Summary(JsonOpt),
}

/// Format de `export`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ExportFormat {
    Json,
    Html,
}

#[derive(Debug, Subcommand)]
pub enum Query {
    /// Recherche de nœuds par nom
    Search {
        terme: String,
        #[arg(long)]
        limit: Option<usize>,
        #[command(flatten)]
        json: JsonOpt,
    },
    /// Voisins d'un nœud
    Neighbors {
        nœud: String,
        #[arg(long, default_value_t = 1)]
        depth: usize,
        #[arg(long)]
        limit: Option<usize>,
        #[command(flatten)]
        json: JsonOpt,
    },
    /// Chemin entre deux nœuds
    Path {
        source: String,
        cible: String,
        #[command(flatten)]
        json: JsonOpt,
    },
    /// Question en langage naturel
    Ask {
        question: String,
        #[command(flatten)]
        json: JsonOpt,
        #[arg(help = FORMAT_RETRIES_HELP, long, value_name = "N", value_parser = parse_format_retries, allow_hyphen_values = true)]
        format_retries: Option<u32>,
    },
}

/// Exécute la commande.
pub fn run(cli: Cli) -> Result<()> {
    match &cli.command {
        Command::Build(o) => {
            crate::progress::init(o.show_progress);
            crate::index::build::run(
                &cli.repo,
                o.no_llm,
                o.enrich_code,
                o.verbose,
                o.batch_files,
                o.format_retries,
                o.filters(),
            )
        }
        Command::Update(o) => {
            crate::progress::init(o.show_progress);
            crate::index::update::run(
                &cli.repo,
                o.no_llm,
                o.enrich_code,
                o.verbose,
                o.batch_files,
                o.format_retries,
                o.filters(),
            )
        }
        Command::Status(j) => crate::status::run(&cli.repo, j.json),
        Command::Summary(j) => crate::summary::run(&cli.repo, j.json),
        Command::Export { output, format } => {
            crate::export::run(&cli.repo, output.as_deref(), *format)
        }
        Command::Query(Query::Search { terme, limit, json }) => {
            crate::query::search::run(&cli.repo, terme, *limit, json.json)
        }
        Command::Query(Query::Neighbors {
            nœud,
            depth,
            limit,
            json,
        }) => crate::query::neighbors::run(&cli.repo, nœud, *depth, *limit, json.json),
        Command::Query(Query::Path {
            source,
            cible,
            json,
        }) => crate::query::path::run(&cli.repo, source, cible, json.json),
        Command::Query(Query::Ask {
            question,
            json,
            format_retries,
        }) => crate::query::ask::run(&cli.repo, question, json.json, *format_retries),
    }
}

#[cfg(test)]
mod help_tests {
    #[test]
    fn req037_ac18_aide_cite_les_reglages_de_projet() {
        assert!(super::FILTERS_HELP.contains(&crate::config::PROJECT_SETTINGS.join(", ")));
    }
}
