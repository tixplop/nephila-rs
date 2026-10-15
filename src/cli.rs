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

#[derive(Debug, Args)]
pub struct IndexOpts {
    /// N'appelle pas le LLM (la configuration n'est pas lue) : structure seule, sans enrichissement
    #[arg(long)]
    pub no_llm: bool,
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
        /// Fichier de sortie (stdout par défaut)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Résumé du dépôt
    Summary(JsonOpt),
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
        Command::Build(o) => crate::index::build::run(&cli.repo, o.no_llm),
        Command::Update(o) => crate::index::update::run(&cli.repo, o.no_llm),
        Command::Status(j) => crate::status::run(&cli.repo, j.json),
        Command::Summary(j) => crate::summary::run(&cli.repo, j.json),
        Command::Export { output } => crate::export::run(&cli.repo, output.as_deref()),
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
