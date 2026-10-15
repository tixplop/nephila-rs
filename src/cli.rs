//! Arbre de commandes clap.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::error::Result;

#[derive(Debug, Parser)]
#[command(name = "rkg", version, about = "Graphe de connaissances d'un dépôt")]
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

#[derive(Debug, Args)]
pub struct IndexOpts {
    /// N'appelle pas le LLM (la configuration n'est pas lue) : structure seule, sans enrichissement
    #[arg(long)]
    pub no_llm: bool,
    /// Traces synthétiques des étapes et des appels LLM sur stderr (préfixe `trace:`)
    #[arg(short, long)]
    pub verbose: bool,
    /// Nombre maximal de fichiers par appel d'enrichissement (remplace `batch_files` de la configuration)
    #[arg(long, value_name = "N", value_parser = parse_batch_files, allow_hyphen_values = true)]
    pub batch_files: Option<usize>,
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
    },
}

/// Exécute la commande.
pub fn run(cli: Cli) -> Result<()> {
    match &cli.command {
        Command::Build(o) => {
            crate::index::build::run(&cli.repo, o.no_llm, o.verbose, o.batch_files)
        }
        Command::Update(o) => {
            crate::index::update::run(&cli.repo, o.no_llm, o.verbose, o.batch_files)
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
        Command::Query(Query::Ask { question, json }) => {
            crate::query::ask::run(&cli.repo, question, json.json)
        }
    }
}
