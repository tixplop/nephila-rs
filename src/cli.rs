//! Arbre de commandes clap.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::error::{Error, Result};

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

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Reconstruit entièrement le graphe
    Build,
    /// Met à jour le graphe pour les fichiers modifiés
    Update,
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

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Build => "build",
            Command::Update => "update",
            Command::Status(_) => "status",
            Command::Query(Query::Search { .. }) => "query search",
            Command::Query(Query::Neighbors { .. }) => "query neighbors",
            Command::Query(Query::Path { .. }) => "query path",
            Command::Query(Query::Ask { .. }) => "query ask",
            Command::Export { .. } => "export",
            Command::Summary(_) => "summary",
        }
    }
}

/// Exécute la commande ; les commandes pas encore réalisées répondent « non implémenté ».
pub fn run(cli: Cli) -> Result<()> {
    if let Command::Build = cli.command {
        return crate::index::build::run(&cli.repo);
    }
    if let Command::Update = cli.command {
        return crate::index::update::run(&cli.repo);
    }
    if let Command::Status(j) = &cli.command {
        return crate::status::run(&cli.repo, j.json);
    }
    match &cli.command {
        Command::Summary(j) => return crate::summary::run(&cli.repo, j.json),
        Command::Export { output } => return crate::export::run(&cli.repo, output.as_deref()),
        Command::Query(Query::Search { terme, limit, json }) => {
            return crate::query::search::run(&cli.repo, terme, *limit, json.json)
        }
        Command::Query(Query::Neighbors {
            nœud,
            depth,
            limit,
            json,
        }) => return crate::query::neighbors::run(&cli.repo, nœud, *depth, *limit, json.json),
        Command::Query(Query::Path {
            source,
            cible,
            json,
        }) => return crate::query::path::run(&cli.repo, source, cible, json.json),
        _ => {}
    }
    Err(Error::NotImplemented(format!(
        "commande `{}`",
        cli.command.name()
    )))
}
