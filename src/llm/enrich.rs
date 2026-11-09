//! Prompt d'enrichissement et schéma JSON version 2 (DEC-019, REQ-024).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::filter::FileType;
use crate::llm::tidy;

/// Version du schéma de réponse d'enrichissement.
pub const ENRICH_SCHEMA_VERSION: u32 = 2;

/// Version du prompt d'enrichissement de la famille `code` (DEC-062, DEC-157). Règle d'incrément
/// (REQ-193) : changer un texte commun (consignes système communes, gabarits de requête, schémas
/// de réponse, notes de correction) incrémente les quatre versions de famille ; changer un texte
/// propre à une famille n'incrémente que la sienne. Tout enrichissement conservé avec une
/// autre version est refait. Un test fige l'empreinte BLAKE3 des textes de la famille à côté de
/// cette valeur. 1 de la 0.18.0 à la 0.25.x ; 9 depuis la 0.28.0 (consignes de forme des
/// résumés, REQ-167 ; extraits, REQ-169 ; genres et types de fichiers, REQ-171 ; langue,
/// REQ-172 ; concepts, REQ-175 ; notes de correction des nouveaux essais, REQ-177 ; liste
/// numérotée et réponses à numéros, REQ-236 ; les valeurs 2 à 8 n'ont pas été livrées) ; 10 depuis la 0.28.0 (gabarit réordonné, REQ-267).
pub const CODE_PROMPT_VERSION: u32 = 10;

/// Version du prompt d'enrichissement de la famille `build` (DEC-062, DEC-157). Règle d'incrément
/// (REQ-193) : changer un texte commun (consignes système communes, gabarits de requête, schémas
/// de réponse, notes de correction) incrémente les quatre versions de famille ; changer un texte
/// propre à une famille n'incrémente que la sienne. Tout enrichissement conservé avec une
/// autre version est refait. Un test fige l'empreinte BLAKE3 des textes de la famille à côté de
/// cette valeur. 1 de la 0.18.0 à la 0.25.x ; 9 depuis la 0.28.0 (consignes de forme des
/// résumés, REQ-167 ; extraits, REQ-169 ; genres et types de fichiers, REQ-171 ; langue,
/// REQ-172 ; concepts, REQ-175 ; notes de correction des nouveaux essais, REQ-177 ; liste
/// numérotée et réponses à numéros, REQ-236 ; les valeurs 2 à 8 n'ont pas été livrées) ; 10 depuis
/// la 0.28.0 (valeurs sensibles de `package.json` masquées avant l'envoi, REQ-265) ; 11 depuis la 0.28.0 (gabarit réordonné, REQ-267).
pub const BUILD_PROMPT_VERSION: u32 = 11;

/// Version du prompt d'enrichissement de la famille `données` (DEC-062, DEC-157). Règle d'incrément
/// (REQ-193) : changer un texte commun (consignes système communes, gabarits de requête, schémas
/// de réponse, notes de correction) incrémente les quatre versions de famille ; changer un texte
/// propre à une famille n'incrémente que la sienne. Tout enrichissement conservé avec une
/// autre version est refait. Un test fige l'empreinte BLAKE3 des textes de la famille à côté de
/// cette valeur. 1 de la 0.18.0 à la 0.25.x ; 9 depuis la 0.28.0 (consignes de forme des
/// résumés, REQ-167 ; extraits, REQ-169 ; genres et types de fichiers, REQ-171 ; langue,
/// REQ-172 ; concepts, REQ-175 ; notes de correction des nouveaux essais, REQ-177 ; liste
/// numérotée et réponses à numéros, REQ-236 ; les valeurs 2 à 8 n'ont pas été livrées) ; 10 depuis
/// la 0.28.0 (marques de l'extrait JSON, YAML et XML, REQ-239, et du SQL réduit, REQ-240) ; 11 depuis
/// la 0.28.0 (valeurs sensibles masquées avant l'envoi, REQ-265) ; 12 depuis la 0.28.0 (gabarit réordonné, REQ-267).
pub const DATA_PROMPT_VERSION: u32 = 12;

/// Version du prompt d'enrichissement de la famille `markdown` (DEC-062, DEC-157). Règle d'incrément
/// (REQ-193) : changer un texte commun (consignes système communes, gabarits de requête, schémas
/// de réponse, notes de correction) incrémente les quatre versions de famille ; changer un texte
/// propre à une famille n'incrémente que la sienne (`CONCEPT_RULES` pour celle-ci). Tout enrichissement conservé avec une
/// autre version est refait. Un test fige l'empreinte BLAKE3 des textes de la famille à côté de
/// cette valeur. 1 de la 0.18.0 à la 0.25.x ; 9 depuis la 0.28.0 (consignes de forme des
/// résumés, REQ-167 ; extraits, REQ-169 ; genres et types de fichiers, REQ-171 ; langue,
/// REQ-172 ; concepts, REQ-175 ; notes de correction des nouveaux essais, REQ-177 ; liste
/// numérotée et réponses à numéros, REQ-236 ; les valeurs 2 à 8 n'ont pas été livrées) ; 10 depuis la 0.28.0 (gabarit réordonné, REQ-267).
pub const MARKDOWN_PROMPT_VERSION: u32 = 10;

/// Versions du prompt d'enrichissement des quatre familles (REQ-193).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptVersions {
    pub code: u32,
    pub build: u32,
    pub data: u32,
    pub markdown: u32,
    /// `enrich_redact` est faux (REQ-265) : les fichiers visés par le masquage prennent + 1 000.
    pub masking_off: bool,
}

impl PromptVersions {
    /// Versions du code.
    pub const CURRENT: PromptVersions = PromptVersions {
        code: CODE_PROMPT_VERSION,
        build: BUILD_PROMPT_VERSION,
        data: DATA_PROMPT_VERSION,
        markdown: MARKDOWN_PROMPT_VERSION,
        masking_off: false,
    };

    /// Version de la famille du type `t`.
    pub fn of(&self, t: FileType) -> u32 {
        let family = match prompt_family(t) {
            PromptFamily::Code => self.code,
            PromptFamily::Build => self.build,
            PromptFamily::Data => self.data,
            PromptFamily::Markdown => self.markdown,
        };
        if self.masking_off && crate::llm::redact::is_targeted(t) {
            family + 1000
        } else {
            family
        }
    }

    /// Version de la famille du chemin ; un chemin sans type prend celle de `code`.
    pub fn of_path(&self, path: &str) -> u32 {
        FileType::of(path).map_or(self.code, |t| self.of(t))
    }
}

/// Consignes de forme des résumés : ligne commune aux deux prompts système (DEC-135).
pub const SUMMARY_RULES: &str = "Résumés (de nœud ou de concept) : une phrase de 25 mots au plus, qui dit le rôle ou la responsabilité de l'élément (ce qu'il fait et à quoi il sert), pas sa syntaxe ni sa signature ; ne répète pas son nom ; n'écris que ce que le contenu montre, sans rien supposer ni inventer. Omets (aucune entrée dans \"summaries\") un accesseur ou un mutateur trivial (il lit ou écrit un champ, sans autre effet) et une section vide ou réduite à son titre.";

/// Consignes d'extrait : ligne du prompt système du fichier envoyé seul, après `SUMMARY_RULES`
/// (REQ-169, DEC-136).
pub const EXCERPT_RULES: &str = "Un fichier trop long est envoyé en extraits : une ligne \"(extrait k/n, lignes a–b)\" suit alors la liste des nœuds et tu ne vois que ces lignes. À partir du deuxième extrait, le contenu commence par une copie partielle du début du fichier (package, imports, déclaration du type englobant, ou titres des sections englobantes), donnée pour contexte : n'en tire aucun résumé. Un nœud de la liste seulement cité dans un extrait se résume d'après ce que l'extrait en montre ; omets-le si l'extrait n'en montre pas le rôle.";

/// Angle du résumé selon le genre du nœud : ligne commune aux deux prompts système (DEC-137).
pub const KIND_RULES: &str = "Angle du résumé selon le genre du nœud, donné par le préfixe de son identifiant : module → ce que le module produit et à quoi lui servent ses dépendances ; type → sa responsabilité ; method → son effet et ce qu'elle renvoie ; table → son rôle métier et ses colonnes clés ; document → le sujet du document, ou ce que décrit ou configure un fichier de données (SQL, XML, JSON, YAML) ; section → le sujet qu'elle traite.";

/// Famille d'un type de fichier dans la phrase des types du prompt (REQ-171).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFamily {
    Code,
    Build,
    Data,
    Markdown,
}

/// Famille de prompt d'un type de fichier (REQ-193, DEC-157).
pub fn prompt_family(t: FileType) -> PromptFamily {
    file_type_label(t).0
}

/// Famille et libellé d'un type de fichier dans le prompt ; `match` exhaustif (REQ-171).
fn file_type_label(t: FileType) -> (PromptFamily, &'static str) {
    match t {
        FileType::Java => (PromptFamily::Code, "Java"),
        FileType::Kotlin => (PromptFamily::Code, "Kotlin"),
        FileType::Groovy => (PromptFamily::Code, "Groovy"),
        FileType::Scala => (PromptFamily::Code, "Scala"),
        FileType::JavaScript => (PromptFamily::Code, "JavaScript"),
        FileType::TypeScript => (PromptFamily::Code, "TypeScript"),
        FileType::Maven => (PromptFamily::Build, "Maven"),
        FileType::Ivy => (PromptFamily::Build, "Ivy"),
        FileType::Gradle => (PromptFamily::Build, "Gradle"),
        FileType::Npm => (PromptFamily::Build, "npm"),
        FileType::Sql => (PromptFamily::Data, "SQL"),
        FileType::Xml => (PromptFamily::Data, "XML"),
        FileType::Json => (PromptFamily::Data, "JSON"),
        FileType::Yaml => (PromptFamily::Data, "YAML"),
        FileType::Markdown => (PromptFamily::Markdown, "Markdown"),
    }
}

/// Types de fichiers de la première phrase des prompts système, tirés de `FileType::ALL`
/// (REQ-171) : familles `code`, `build`, `données`, puis les types sans famille nommée, jointes
/// par `, ` avec `ou ` devant la dernière.
pub fn file_types_phrase() -> String {
    let groups: Vec<String> = [
        (PromptFamily::Code, "code "),
        (PromptFamily::Build, "build "),
        (PromptFamily::Data, "données "),
        (PromptFamily::Markdown, ""),
    ]
    .into_iter()
    .filter_map(|(family, name)| {
        let labels: Vec<&str> = FileType::ALL
            .into_iter()
            .map(file_type_label)
            .filter(|(f, _)| *f == family)
            .map(|(_, l)| l)
            .collect();
        (!labels.is_empty()).then(|| format!("{name}{}", labels.join("/")))
    })
    .collect();
    match groups.split_last() {
        Some((last, head)) if !head.is_empty() => format!("{}, ou {last}", head.join(", ")),
        Some((last, _)) => last.clone(),
        None => String::new(),
    }
}

/// Morceau d'un fichier envoyé en plusieurs morceaux (REQ-169) : `rank` à partir de 1 sur `total`
/// morceaux du fichier, lignes `first_line` à `last_line` (à partir de 1, bornes comprises) de
/// son corps, en-tête répété exclu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Excerpt {
    pub rank: usize,
    pub total: usize,
    pub first_line: usize,
    pub last_line: usize,
}

/// Concept proposé pour un document Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Concept {
    /// Nom normalisé (REQ-180 : minuscules, espaces réduits, guillemets et ponctuation finale
    /// retirés) = nom qualifié du nœud concept.
    pub name: String,
    pub summary: Option<String>,
    /// Identifiants des sections du fichier qui mentionnent le concept.
    pub sections: Vec<String>,
}

/// Résultat filtré de l'enrichissement d'un morceau (ou d'un fichier après fusion).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Enrichment {
    /// Couples (identifiant de nœud, résumé), nœuds de la liste fournie uniquement.
    pub summaries: Vec<(String, String)>,
    pub concepts: Vec<Concept>,
    /// Éléments écartés (hors liste, concept hors Markdown…), décrits.
    pub rejected: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnrichment {
    #[serde(default)]
    version: Option<u32>,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
}

/// Cible d'un résumé : numéro de la liste demandée ou identifiant de nœud (REQ-237).
enum Target {
    Number(i64),
    Id(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryFields {
    n: Option<i64>,
    node: Option<String>,
    summary: String,
}

#[derive(Deserialize)]
#[serde(try_from = "SummaryFields")]
struct RawSummary {
    target: Target,
    summary: String,
}

impl TryFrom<SummaryFields> for RawSummary {
    type Error = String;
    fn try_from(f: SummaryFields) -> std::result::Result<Self, String> {
        let target = match (f.n, f.node) {
            (Some(n), None) => Target::Number(n),
            (None, Some(id)) => Target::Id(id),
            (None, None) => return Err("missing field `n`".into()),
            (Some(_), Some(_)) => return Err("duplicate field `n`".into()),
        };
        Ok(RawSummary {
            target,
            summary: f.summary,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConcept {
    name: String,
    summary: Option<String>,
    sections: Vec<String>,
}

fn node_empty_of_sense(id: &str) -> String {
    format!("résumé pour {id} (vide de sens)")
}

fn node_empty(id: &str) -> String {
    format!("résumé pour {id} (vide)")
}

fn node_duplicate(id: &str) -> String {
    format!("résumé en double pour {id} (premier gardé)")
}

fn node_capped(id: &str, received: usize) -> String {
    format!(
        "résumé pour {id} : coupé à {} caractères, fin écartée ({received} caractères reçus)",
        tidy::SUMMARY_MAX_CHARS
    )
}

fn concept_empty_of_sense(name: &str) -> String {
    format!("concept {name} : résumé vide de sens écarté")
}

fn concept_capped(name: &str, received: usize) -> String {
    format!(
        "concept {name} : résumé coupé à {} caractères, fin écartée ({received} caractères reçus)",
        tidy::SUMMARY_MAX_CHARS
    )
}

/// Résumé replié puis coupé : `(texte replié, longueur repliée si coupé, texte gardé)` ; `None`
/// si vide après repli.
fn fold_and_cap(raw: &str) -> Option<(String, Option<usize>, String)> {
    let folded = tidy::fold(raw);
    if folded.is_empty() {
        return None;
    }
    let received = folded.chars().count();
    match tidy::cap(&folded) {
        Some(cut) => Some((folded, Some(received), cut)),
        None => {
            let same = folded.clone();
            Some((folded, None, same))
        }
    }
}

/// Filtre les éléments bruts d'un fichier selon sa liste de nœuds (règles version 2, REQ-024) :
/// résumés repliés sur une ligne, coupés à 400 caractères et écartés s'ils sont vides de sens
/// (REQ-178) ; un résumé vide d'un nœud de la liste est signalé (REQ-234), celui d'un concept ne
/// l'est pas ; premier résumé d'un nœud gardé et concepts de même nom fusionnés (REQ-179), noms
/// de concepts normalisés (REQ-180) ; DEC-144.
fn filter_raw(
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
    file: &str,
    numbered: &[String],
    first: usize,
    node_ids: &[String],
) -> Enrichment {
    let known: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
    let mut out = Enrichment::default();
    let mut kept: HashMap<String, usize> = HashMap::new();

    for s in summaries {
        let node = match s.target {
            Target::Number(k) => {
                let line = k
                    .checked_sub(first as i64)
                    .and_then(|i| usize::try_from(i).ok())
                    .and_then(|i| numbered.get(i));
                match line {
                    Some(id) => id.clone(),
                    None => {
                        let m = format!("résumé pour le numéro {k} (hors liste)");
                        if !out.rejected.contains(&m) {
                            out.rejected.push(m);
                        }
                        continue;
                    }
                }
            }
            Target::Id(id) => id,
        };
        let summary = s.summary;
        if node.trim().is_empty() {
            out.rejected.push("résumé sans identifiant de nœud".into());
            continue;
        }
        if !known.contains(node.as_str()) {
            out.rejected
                .push(format!("résumé pour {} (nœud hors liste)", node));
            continue;
        }
        let Some((folded, received, text)) = fold_and_cap(&summary) else {
            out.rejected.push(node_empty(&node));
            continue;
        };
        if tidy::is_meaningless(&folded, tidy::node_name(&node)) {
            out.rejected.push(node_empty_of_sense(&node));
            continue;
        }
        if let Some(&i) = kept.get(&node) {
            if out.summaries[i].1 != text {
                out.rejected.push(node_duplicate(&node));
            }
            continue;
        }
        if let Some(n) = received {
            out.rejected.push(node_capped(&node, n));
        }
        kept.insert(node.clone(), out.summaries.len());
        out.summaries.push((node, text));
    }

    let markdown = is_markdown(file);
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for c in concepts {
        let name = normalize_concept_name(&c.name);
        if name.is_empty() {
            out.rejected.push("concept sans nom".into());
            continue;
        }
        if !markdown {
            out.rejected
                .push(format!("concept {name} (fichier non Markdown)"));
            continue;
        }
        let mut sections: Vec<String> = Vec::new();
        let mut dropped = false;
        for s in c.sections {
            if !(s.starts_with("section:") && known.contains(s.as_str())) {
                dropped = true;
            } else if !sections.contains(&s) {
                sections.push(s);
            }
        }
        if sections.is_empty() {
            out.rejected
                .push(format!("concept {name} (aucune section du fichier citée)"));
            continue;
        }
        if dropped {
            out.rejected
                .push(format!("concept {name} : sections hors liste écartées"));
        }
        let summary = match c.summary.as_deref().and_then(fold_and_cap) {
            None => None,
            Some((folded, received, text)) => {
                if tidy::is_meaningless(&folded, &name) {
                    out.rejected.push(concept_empty_of_sense(&name));
                    None
                } else {
                    if let Some(n) = received {
                        out.rejected.push(concept_capped(&name, n));
                    }
                    Some(text)
                }
            }
        };
        if let Some(&i) = by_name.get(&name) {
            let existing = &mut out.concepts[i];
            for s in sections {
                if !existing.sections.contains(&s) {
                    existing.sections.push(s);
                }
            }
            if existing.summary.is_none() {
                existing.summary = summary;
            }
        } else {
            by_name.insert(name.clone(), out.concepts.len());
            out.concepts.push(Concept {
                name,
                summary,
                sections,
            });
        }
    }
    out
}

fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::Format(format!("réponse LLM non conforme au schéma : {msg}"))
}

/// Nom de concept normalisé (REQ-180) : mots séparés par une espace, en minuscules, puis, jusqu'à
/// stabilité, guillemets de tête et de fin et ponctuation finale retirés, blancs compris.
pub fn normalize_concept_name(name: &str) -> String {
    const QUOTES: [char; 13] = [
        '"', '\'', '`', '«', '»', '“', '”', '‘', '’', '„', '‹', '›', ' ',
    ];
    const PUNCT: [char; 7] = ['.', ',', ';', ':', '!', '?', '…'];
    let mut cur = name
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    loop {
        let next = cur
            .trim_matches(|c: char| c.is_whitespace() || QUOTES.contains(&c))
            .trim_end_matches(|c: char| c.is_whitespace() || PUNCT.contains(&c));
        if next.len() == cur.len() {
            return cur;
        }
        cur = next.to_string();
    }
}

/// Vrai si le chemin désigne un fichier Markdown.
pub fn is_markdown(file: &str) -> bool {
    std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

impl Enrichment {
    /// Analyse strictement `json` puis filtre selon la liste de nœuds du fichier.
    pub fn parse(json: &str, file: &str, node_ids: &[String]) -> Result<Enrichment> {
        Self::parse_numbered(json, file, &[], node_ids)
    }

    /// Comme `parse`, avec la liste numérotée `requested` (numéros dès 1) pour retraduire `n`.
    pub fn parse_numbered(
        json: &str,
        file: &str,
        requested: &[String],
        node_ids: &[String],
    ) -> Result<Enrichment> {
        let raw: RawEnrichment = serde_json::from_str(json).map_err(invalid)?;
        if let Some(v) = raw.version.filter(|v| *v != ENRICH_SCHEMA_VERSION) {
            return Err(invalid(format!(
                "version {v} non prise en charge (attendu : {ENRICH_SCHEMA_VERSION})"
            )));
        }
        Ok(filter_raw(
            raw.summaries,
            raw.concepts,
            file,
            requested,
            1,
            node_ids,
        ))
    }
}

/// Règles de choix et de nommage des concepts : ligne commune aux deux prompts système, après la
/// consigne de langue (REQ-175, DEC-141). Règles seulement : aucun contrôle dans le code.
pub const CONCEPT_RULES: &str = "Concepts : chacun est une notion métier ou technique durable que le document définit ou explique, jamais un titre de section ni un mot générique (introduction, exemple, présentation, sommaire, remarque) ; son nom tient en 1 à 4 mots, au singulier, sans article, et désigne la notion elle-même (« commande », pas « les commandes » ni « gestion des commandes ») ; au plus 8 concepts par fichier dans une même réponse ; sans notion de ce genre, \"concepts\": [].";

/// Consigne de langue des résumés et des noms de concepts : ligne commune aux deux prompts
/// système (REQ-172, DEC-139). `language` est écrite telle quelle.
pub fn language_rules(language: &str) -> String {
    format!(
        "Langue : rédige les résumés et les noms de concepts en {language}, quelle que soit la langue du contenu ; garde tels quels les identifiants et les termes techniques."
    )
}

/// Consigne des numéros : ligne commune aux deux prompts système, juste après la ligne `Schéma`
/// (REQ-236, DEC-177).
pub const NUMBER_RULES: &str = "Chaque nœud de la liste s'écrit [numéro] nom — identifiant. Dans \"n\", donne le numéro écrit à côté du nom du nœud résumé, sans recopier son identifiant.";

/// Lignes `[k] nom — id` de la liste, numérotées à partir de `first` (REQ-236).
fn list_lines(ids: &[String], first: usize) -> String {
    ids.iter()
        .enumerate()
        .map(|(i, id)| format!("[{}] {} — {id}\n", first + i, list_name(id)))
        .collect()
}

/// Nom court d'une ligne de liste : dernier segment du chemin pour un document, sinon
/// `tidy::node_name`.
fn list_name(id: &str) -> &str {
    match id.strip_prefix("document:") {
        Some(path) => path.rsplit('/').next().unwrap_or(path),
        None => tidy::node_name(id),
    }
}

/// Prompt système d'enrichissement (identique pour tous les fournisseurs), langue demandée
/// `language`.
pub fn enrichment_system_prompt(language: &str) -> String {
    let types = file_types_phrase();
    let rules = language_rules(language);
    format!(
        "Tu enrichis le graphe d'un dépôt à partir d'un fichier ({types}) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Schéma : {{\"summaries\": [{{\"n\": <numéro de la liste>, \"summary\": \"<25 mots au plus>\"}}], \
         \"concepts\": [{{\"name\": \"nom du concept\", \"summary\": \"<25 mots au plus> (optionnel)\", \
         \"sections\": [\"<identifiant de section de la liste>\"]}}]}}.\n\
         {NUMBER_RULES}\n\
         {SUMMARY_RULES}\n\
         {EXCERPT_RULES}\n\
         {KIND_RULES}\n\
         {rules}\n\
         {CONCEPT_RULES}\n\
         Les résumés ne portent que sur les nœuds de la liste fournie. Les concepts ne concernent \
         que les fichiers Markdown et ne citent que des sections de la liste. Tu ne crées, ne \
         renomme ni ne supprimes aucun nœud et tu ne produis aucun autre lien. \
         Sans rien à signaler, réponds {{\"summaries\": [], \"concepts\": []}}."
    )
}

/// Message utilisateur d'enrichissement : contenu du morceau puis liste des nœuds (REQ-267).
pub fn enrichment_user_prompt(
    file: &str,
    excerpt: Option<Excerpt>,
    text: &str,
    requested: &[String],
) -> String {
    let list = list_lines(requested, 1);
    let hint = match excerpt {
        None => String::new(),
        Some(Excerpt {
            rank,
            total,
            first_line,
            last_line,
        }) => format!("(extrait {rank}/{total}, lignes {first_line}–{last_line})\n"),
    };
    format!(
        "Fichier : {file}\n\nContenu :\n{text}\n\nNœuds du fichier :\n{list}\n{hint}Réponds par l'objet JSON demandé, sur les seuls nœuds de cette liste."
    )
}

/// Version du schéma de réponse d'un lot de 2 fichiers ou plus (DEC-033).
pub const ENRICH_LOT_SCHEMA_VERSION: u32 = 3;

/// Un fichier d'un lot : chemin, contenu et identifiants de ses nœuds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotItem {
    pub file: String,
    pub text: String,
    pub node_ids: Vec<String>,
}

/// Résultat d'un lot : un résultat par fichier du lot (dans l'ordre du lot) et les éléments
/// écartés qui ne se rattachent à aucun fichier (entrée hors lot ou en double).
#[derive(Debug)]
pub struct LotEnrichment {
    pub files: Vec<(String, Result<Enrichment>)>,
    pub rejected: Vec<String>,
}

impl LotEnrichment {
    /// Résultats par fichier ; les éléments écartés propres au lot sont rattachés au premier
    /// fichier enrichi avec succès pour qu'ils soient signalés ; si aucun fichier n'a réussi,
    /// ils sont reportés dans le message d'erreur de chaque fichier absent.
    pub fn into_results(self) -> Vec<(String, Result<Enrichment>)> {
        let LotEnrichment {
            mut files,
            rejected,
        } = self;
        if let Some(Ok(e)) = files.iter_mut().map(|(_, r)| r).find(|r| r.is_ok()) {
            e.rejected.extend(rejected);
        } else if !rejected.is_empty() {
            let note = format!(" (éléments écartés du lot : {})", rejected.join(" ; "));
            for (_, r) in files.iter_mut() {
                if let Err(err) = r {
                    let text = format!("{err}{note}");
                    let is_format = matches!(err, Error::Format(_));
                    *r = Err(if is_format {
                        Error::Format(text)
                    } else {
                        Error::Message(text)
                    });
                }
            }
        }
        files
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLot {
    #[serde(default)]
    version: Option<u32>,
    files: Vec<RawLotEntry>,
}

/// Cible d'une entrée de lot : numéro du fichier ou chemin (REQ-237).
enum EntryTarget {
    Number(i64),
    Path(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryFields {
    f: Option<i64>,
    file: Option<String>,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
}

#[derive(Deserialize)]
#[serde(try_from = "EntryFields")]
struct RawLotEntry {
    target: EntryTarget,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
}

impl TryFrom<EntryFields> for RawLotEntry {
    type Error = String;
    fn try_from(e: EntryFields) -> std::result::Result<Self, String> {
        let target = match (e.f, e.file) {
            (Some(n), None) => EntryTarget::Number(n),
            (None, Some(p)) => EntryTarget::Path(p),
            (None, None) => return Err("missing field `f`".into()),
            (Some(_), Some(_)) => return Err("duplicate field `f`".into()),
        };
        Ok(RawLotEntry {
            target,
            summaries: e.summaries,
            concepts: e.concepts,
        })
    }
}

/// Forme normalisée d'un chemin de réponse : espaces de bord retirés, `\` en `/`, `./` de tête retirés.
fn norm_path(path: &str) -> String {
    let mut p = path.trim().replace('\\', "/");
    while let Some(rest) = p.strip_prefix("./") {
        p = rest.to_string();
    }
    p
}

/// Analyse strictement la réponse version 3 d'un lot puis filtre chaque entrée selon les nœuds
/// du fichier qu'elle nomme. Une réponse illisible ou d'une autre version rend tout le lot
/// non conforme (`Err`) ; un fichier du lot sans entrée échoue seul.
pub fn parse_lot(json: &str, items: &[LotItem]) -> Result<LotEnrichment> {
    let raw: RawLot = serde_json::from_str(json).map_err(invalid)?;
    if let Some(v) = raw.version.filter(|v| *v != ENRICH_LOT_SCHEMA_VERSION) {
        return Err(invalid(format!(
            "version {v} non prise en charge pour un lot (attendu : {ENRICH_LOT_SCHEMA_VERSION})"
        )));
    }
    Ok(attach_entries(raw.files, items))
}

/// Rattache les entrées brutes d'une réponse de lot aux fichiers du lot et filtre chacune selon
/// les nœuds de son fichier : entrée hors lot ou en double écartée, fichier sans entrée en échec.
fn attach_entries(entries: Vec<RawLotEntry>, items: &[LotItem]) -> LotEnrichment {
    let mut rejected = Vec::new();
    let mut found: Vec<Option<Enrichment>> = vec![None; items.len()];
    let firsts: Vec<usize> = items
        .iter()
        .scan(1, |next, it| {
            let first = *next;
            *next += it.node_ids.len();
            Some(first)
        })
        .collect();
    for entry in entries {
        let (i, name) = match &entry.target {
            EntryTarget::Number(f) => {
                let Some(i) = usize::try_from(*f)
                    .ok()
                    .filter(|f| (1..=items.len()).contains(f))
                    .map(|f| f - 1)
                else {
                    rejected.push(format!("entrée pour le fichier numéro {f} (hors du lot)"));
                    continue;
                };
                (i, items[i].file.clone())
            }
            EntryTarget::Path(path) => {
                let wanted = norm_path(path);
                let by_norm = || {
                    let same: Vec<usize> = (0..items.len())
                        .filter(|&i| norm_path(&items[i].file) == wanted)
                        .collect();
                    // Plusieurs fichiers de même forme normalisée : l'entrée n'est rattachable que
                    // si tous ont déjà la leur (elle est alors « en double »), sinon elle est hors
                    // du lot.
                    match same.as_slice() {
                        [i] => Some(*i),
                        [i, ..] if same.iter().all(|&j| found[j].is_some()) => Some(*i),
                        _ => None,
                    }
                };
                let Some(i) = items
                    .iter()
                    .position(|it| &it.file == path)
                    .or_else(by_norm)
                else {
                    rejected.push(format!("entrée pour {path} (fichier hors du lot)"));
                    continue;
                };
                (i, path.clone())
            }
        };
        if found[i].is_some() {
            rejected.push(format!("entrée en double pour {name}"));
            continue;
        }
        found[i] = Some(filter_raw(
            entry.summaries,
            entry.concepts,
            &items[i].file,
            &items[i].node_ids,
            firsts[i],
            &items[i].node_ids,
        ));
    }
    let files = items
        .iter()
        .zip(found)
        .map(|(it, e)| {
            let r = e.ok_or_else(|| {
                Error::Message(format!("{} : absent de la réponse du lot", it.file))
            });
            (it.file.clone(), r)
        })
        .collect();
    LotEnrichment { files, rejected }
}

/// Éléments complets lus dans le début d'une réponse (REQ-232).
#[derive(Default)]
struct Partial {
    version: Option<u32>,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
    files: Vec<RawLotEntry>,
}

/// Liste dont chaque élément est poussé dans `out` dès qu'il est complet ; une erreur plus loin
/// laisse les éléments déjà poussés.
struct PushSeq<'a, T>(&'a mut Vec<T>);

impl<'de, T: Deserialize<'de>> serde::de::DeserializeSeed<'de> for PushSeq<'_, T> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for PushSeq<'_, T> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("une liste")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<(), A::Error> {
        while let Some(x) = seq.next_element::<T>()? {
            self.0.push(x);
        }
        Ok(())
    }
}

/// Objet de la réponse, lu clé par clé dans `Partial`.
struct PartialObject<'a> {
    out: &'a mut Partial,
    lot: bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for PartialObject<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for PartialObject<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("un objet")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<(), A::Error> {
        use serde::de::Error as _;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" => self.out.version = Some(map.next_value()?),
                "summaries" if !self.lot => {
                    map.next_value_seed(PushSeq(&mut self.out.summaries))?
                }
                "concepts" if !self.lot => map.next_value_seed(PushSeq(&mut self.out.concepts))?,
                "files" if self.lot => map.next_value_seed(PushSeq(&mut self.out.files))?,
                other => {
                    let expected: &[&str] = if self.lot {
                        &["version", "files"]
                    } else {
                        &["version", "summaries", "concepts"]
                    };
                    return Err(A::Error::unknown_field(other, expected));
                }
            }
        }
        Ok(())
    }
}

/// Lit `text` à partir du premier `{` et rend les éléments complets lus avant la première
/// erreur (coupe, champ inconnu, élément mal formé) ; une version autre que `version` vide tout.
fn read_partial(text: &str, version: u32, lot: bool) -> Partial {
    let mut partial = Partial::default();
    if let Some(start) = text.find('{') {
        let mut de = serde_json::Deserializer::from_str(&text[start..]);
        let _ = serde::de::DeserializeSeed::deserialize(
            PartialObject {
                out: &mut partial,
                lot,
            },
            &mut de,
        );
    }
    if partial.version.is_some_and(|v| v != version) {
        return Partial::default();
    }
    partial
}

impl Enrichment {
    /// Éléments complets du début d'une réponse d'appel tronquée, filtrés comme `parse` ; ne
    /// rend jamais d'erreur : ce qui n'a pas été lu est le reste (REQ-232).
    pub fn parse_partial(
        text: &str,
        file: &str,
        requested: &[String],
        known: &[String],
    ) -> Enrichment {
        let p = read_partial(text, ENRICH_SCHEMA_VERSION, false);
        filter_raw(p.summaries, p.concepts, file, requested, 1, known)
    }
}

/// Entrées complètes du début d'une réponse de lot tronquée, rattachées comme `parse_lot`.
pub fn parse_lot_partial(text: &str, items: &[LotItem]) -> LotEnrichment {
    let p = read_partial(text, ENRICH_LOT_SCHEMA_VERSION, true);
    attach_entries(p.files, items)
}

/// Prompt système d'un lot de fichiers (identique pour tous les fournisseurs), langue demandée
/// `language`.
pub fn lot_system_prompt(language: &str) -> String {
    let types = file_types_phrase();
    let rules = language_rules(language);
    format!(
        "Tu enrichis le graphe d'un dépôt à partir de plusieurs fichiers ({types}) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Chaque fichier est introduit par une ligne `=== FICHIER [<f>] : <chemin> ===`, suivie de la liste de ses \
         nœuds puis de son contenu, jusqu'à la ligne `=== FIN DU FICHIER : <chemin> ===`. \
         Les numéros de nœuds continuent d'un fichier au suivant.\n\
         Schéma : {{\"files\": \
         [{{\"f\": <numéro du fichier>, \"summaries\": [{{\"n\": <numéro de la liste de ce fichier>, \
         \"summary\": \"<25 mots au plus>\"}}], \"concepts\": [{{\"name\": \"nom du concept\", \"summary\": \"<25 mots au plus> (optionnel)\", \
         \"sections\": [\"<identifiant de section de la liste de ce fichier>\"]}}]}}]}}.\n\
         {NUMBER_RULES}\n\
         {SUMMARY_RULES}\n\
         {KIND_RULES}\n\
         {rules}\n\
         {CONCEPT_RULES}\n\
         Produis exactement une entrée par fichier fourni. Les résumés ne portent que sur les nœuds de la \
         liste du fichier concerné. Les concepts ne concernent que les fichiers Markdown et ne citent que des \
         sections de la liste du même fichier. Tu ne crées, ne renomme ni ne supprimes aucun nœud et tu ne \
         produis aucun autre lien. Sans rien à signaler pour un fichier, donne-lui une entrée avec \
         \"summaries\": [] et \"concepts\": []."
    )
}

/// Message utilisateur d'un lot : pour chaque fichier, chemin, nœuds et contenu, avec séparateurs.
pub fn lot_user_prompt(items: &[LotItem]) -> String {
    let mut out = String::new();
    let mut next = 1;
    for (k, it) in items.iter().enumerate() {
        let list = list_lines(&it.node_ids, next);
        next += it.node_ids.len();
        out.push_str(&format!(
            "=== FICHIER [{k}] : {f} ===\nNœuds du fichier :\n{list}\nContenu :\n{t}\n=== FIN DU FICHIER : {f} ===\n\n",
            k = k + 1,
            f = it.file,
            t = it.text
        ));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::DEFAULT_SUMMARY_LANGUAGE;
    use crate::llm::FormatFault;

    const EXTRAIT: &str = "Un fichier trop long est envoyé en extraits : sa ligne \"Fichier :\" porte alors \"(extrait k/n, lignes a–b)\" et tu ne vois que ces lignes. À partir du deuxième extrait, le contenu commence par une copie partielle du début du fichier (package, imports, déclaration du type englobant, ou titres des sections englobantes), donnée pour contexte : n'en tire aucun résumé. Un nœud de la liste seulement cité dans un extrait se résume d'après ce que l'extrait en montre ; omets-le si l'extrait n'en montre pas le rôle.";

    const EXCERPT_2_3: Excerpt = Excerpt {
        rank: 2,
        total: 3,
        first_line: 10,
        last_line: 20,
    };

    fn lot_items() -> [LotItem; 2] {
        [
            LotItem {
                file: "a.md".into(),
                text: "texte a".into(),
                node_ids: ids(&["document:a.md", "section:a.md#x"]),
            },
            LotItem {
                file: "B.java".into(),
                text: "texte b".into(),
                node_ids: ids(&["type:p.B"]),
            },
        ]
    }

    fn user_prompt(excerpt: Option<Excerpt>) -> String {
        enrichment_user_prompt(
            "a.md",
            excerpt,
            "texte a",
            &ids(&["document:a.md", "section:a.md#x"]),
        )
    }

    /// Empreinte BLAKE3 d'une liste de textes de prompt.
    pub(crate) fn fingerprint_of(texts: &[String]) -> String {
        blake3::hash(texts.join("\n<<<->>>\n").as_bytes())
            .to_hex()
            .to_string()
    }

    /// Les cinq textes de la 0.26.0 reconstruits depuis les textes actuels (REQ-169) : sans
    /// `EXCERPT_RULES`, requête `None` réécrite en `(morceau 2)`. `f` transforme les prompts
    /// système.
    fn texts_0_26(f: impl Fn(String) -> String) -> Vec<String> {
        let user = texts_v8_user(user_prompt_v9(None)).replacen(
            "Fichier : a.md\n",
            "Fichier : a.md (morceau 2)\n",
            1,
        );
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            f(
                texts_v3_system(enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE))
                    .replace(&format!("{EXTRAIT}\n"), ""),
            ),
            user,
            f(texts_v3_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE))),
            texts_v8_user(lot_user_prompt(&lot_items())),
        ]
    }

    const OLD_TYPES: &str =
        "code Java/Kotlin/Groovy/Scala/JavaScript/TypeScript, build Maven/Gradle/Ivy, ou Markdown";

    /// Prompt système de la version 5 : sans les règles des concepts (REQ-175).
    fn texts_v5_system(p: String) -> String {
        texts_v7_system(p)
            .replacen(EXCERPT_RULES, EXTRAIT, 1)
            .replace(&format!("{CONCEPT_RULES}\n"), "")
    }

    /// Les six textes figés par la version 5 (AC-175-2).
    fn texts_v5() -> Vec<String> {
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            texts_v5_system(enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            texts_v8_user(user_prompt_v9(None)),
            texts_v8_user(user_prompt_v9(Some(EXCERPT_2_3))),
            texts_v5_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            texts_v8_user(lot_user_prompt(&lot_items())),
        ]
    }

    #[test]
    fn req175_ac2_the_six_texts_of_version_5_are_unchanged() {
        assert_eq!(
            fingerprint_of(&texts_v5()),
            "dd9ffc5c3e4673a05ec777430b44aa445cf5f2262e251e62a531f9cf097d70f2"
        );
    }

    /// Prompt système de la version 4 : sans la consigne de langue (REQ-172).
    fn texts_v4_system(p: String) -> String {
        texts_v5_system(p).replace(
            &format!("{}\n", language_rules(DEFAULT_SUMMARY_LANGUAGE)),
            "",
        )
    }

    /// Les six textes figés par la version 4 (AC-172-4).
    fn texts_v4() -> Vec<String> {
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            texts_v4_system(enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            texts_v8_user(user_prompt_v9(None)),
            texts_v8_user(user_prompt_v9(Some(EXCERPT_2_3))),
            texts_v4_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            texts_v8_user(lot_user_prompt(&lot_items())),
        ]
    }

    #[test]
    fn req172_ac4_the_six_texts_of_version_4_are_unchanged() {
        assert_eq!(
            fingerprint_of(&texts_v4()),
            "d35c005b92a13804baf653a19ea7facd94edac5e4f25969b44a5ca59209711de"
        );
    }

    #[test]
    fn req172_ac1_language_rules_in_both_system_prompts() {
        assert_eq!(
            language_rules("français"),
            "Langue : rédige les résumés et les noms de concepts en français, quelle que soit la langue du contenu ; garde tels quels les identifiants et les termes techniques."
        );
        for l in ["français", "anglais", "portugais du Brésil"] {
            let r = language_rules(l);
            let one = format!("\n{KIND_RULES}\n{r}\n");
            assert_eq!(enrichment_system_prompt(l).matches(&one).count(), 1, "{l}");
            let lot = format!("\n{KIND_RULES}\n{r}\n");
            assert_eq!(lot_system_prompt(l).matches(&lot).count(), 1, "{l}");
        }
        assert!(!enrichment_system_prompt("anglais").contains("en français"));
        assert!(!lot_system_prompt("anglais").contains("en français"));
    }

    /// Prompt système de la version 3 : sans `KIND_RULES`, ancienne parenthèse des types.
    fn texts_v3_system(p: String) -> String {
        texts_v4_system(p)
            .replace(&format!("{KIND_RULES}\n"), "")
            .replace(&file_types_phrase(), OLD_TYPES)
    }

    /// Les six textes figés par la version 3 (REQ-171, AC-171-4).
    fn texts_v3() -> Vec<String> {
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            texts_v3_system(enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            texts_v8_user(user_prompt_v9(None)),
            texts_v8_user(user_prompt_v9(Some(EXCERPT_2_3))),
            texts_v3_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            texts_v8_user(lot_user_prompt(&lot_items())),
        ]
    }

    /// Prompt système de la version 8 : sans `NUMBER_RULES` ni la phrase sur les numéros, avec
    /// les fragments `"node"`, `"file"` et `=== FICHIER : <chemin> ===` (REQ-236).
    fn texts_v8_system(p: String) -> String {
        p.replace(&format!("{NUMBER_RULES}\n"), "")
            .replace(
                "{\"n\": <numéro de la liste>, ",
                "{\"node\": \"<identifiant de la liste>\", ",
            )
            .replace(
                "{\"f\": <numéro du fichier>, ",
                "{\"file\": \"<chemin exact du fichier>\", ",
            )
            .replace(
                "{\"n\": <numéro de la liste de ce fichier>, ",
                "{\"node\": \"<identifiant de la liste de ce fichier>\", ",
            )
            .replace(
                "=== FICHIER [<f>] : <chemin> ===",
                "=== FICHIER : <chemin> ===",
            )
            .replace(
                " Les numéros de nœuds continuent d'un fichier au suivant.",
                "",
            )
    }

    /// Texte utilisateur de la version 8 : lignes `- <id>` et `=== FICHIER : <chemin> ===`.
    fn texts_v8_user(u: String) -> String {
        let mut out = String::new();
        for line in u.split_inclusive('\n') {
            if let (true, Some((_, id))) = (line.starts_with('['), line.split_once(" — ")) {
                out.push_str(&format!("- {id}"));
            } else if let Some(rest) = line.strip_prefix("=== FICHIER [") {
                out.push_str(&format!("=== FICHIER {}", rest.split_once("] ").unwrap().1));
            } else {
                out.push_str(line);
            }
        }
        out
    }

    /// Prompt système des versions 2 à 7 : « Schéma (version N) : {"version": N, » et la réponse
    /// vide avec `version` remis (REQ-201 les a retirés en version 8).
    fn texts_v7_system(p: String) -> String {
        texts_v8_system(p).replacen(
            "Schéma : {\"summaries\"",
            &format!(
                "Schéma (version {ENRICH_SCHEMA_VERSION}) : {{\"version\": {ENRICH_SCHEMA_VERSION}, \"summaries\""
            ),
            1,
        )
        .replacen(
            "Schéma : {\"files\"",
            &format!(
                "Schéma (version {ENRICH_LOT_SCHEMA_VERSION}) : {{\"version\": {ENRICH_LOT_SCHEMA_VERSION}, \"files\""
            ),
            1,
        )
        .replacen(
            "réponds {\"summaries\"",
            &format!("réponds {{\"version\": {ENRICH_SCHEMA_VERSION}, \"summaries\""),
            1,
        )
    }

    /// Les six textes de la version 6 (inchangés par la version 7) : prompts actuels avec `version`
    /// remis.
    fn texts_v6() -> Vec<String> {
        let mut t = texts_v9();
        t[1] = texts_v7_system(t[1].clone());
        t[4] = texts_v7_system(t[4].clone());
        for i in [2, 3, 5] {
            t[i] = texts_v8_user(t[i].clone());
        }
        t
    }

    /// Les six textes actuels (version 10 : contenu avant la liste, REQ-267).
    fn texts_v10() -> Vec<String> {
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
            user_prompt(None),
            user_prompt(Some(EXCERPT_2_3)),
            lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
            lot_user_prompt(&lot_items()),
        ]
    }

    /// Requête de la version 9 : liste avant le contenu, extrait sur la ligne `Fichier :`.
    fn user_prompt_v9(excerpt: Option<Excerpt>) -> String {
        let list = list_lines(&ids(&["document:a.md", "section:a.md#x"]), 1);
        let head = match excerpt {
            None => "Fichier : a.md".to_string(),
            Some(e) => format!(
                "Fichier : a.md (extrait {}/{}, lignes {}–{})",
                e.rank, e.total, e.first_line, e.last_line
            ),
        };
        format!("{head}\n\nNœuds du fichier :\n{list}\nContenu :\ntexte a")
    }

    /// Les six textes de la version 9 reconstruits depuis les textes actuels.
    fn texts_v9() -> Vec<String> {
        let mut t = texts_v10();
        t[1] = t[1].replacen(EXCERPT_RULES, EXTRAIT, 1);
        t[2] = user_prompt_v9(None);
        t[3] = user_prompt_v9(Some(EXCERPT_2_3));
        t
    }

    #[test]
    fn req177_ac5_the_six_texts_of_version_6_are_unchanged() {
        assert_eq!(
            fingerprint_of(&texts_v6()),
            "0d53830988dade24bd34df822cd23054749d4d8eb426ed86d817f306620348fb"
        );
    }

    #[test]
    fn req171_ac4_the_six_texts_of_version_3_are_unchanged() {
        assert_eq!(
            fingerprint_of(&texts_v3()),
            "27656c4c231a8abc77a1f39bd2a7c985c33bddaf48c141a06df73557b6b7dc27"
        );
    }

    #[test]
    fn req169_ac3_the_five_texts_of_0_26_0_are_unchanged() {
        assert_eq!(
            fingerprint_of(&texts_0_26(|t| t)),
            "a15bf9ef481fcf8b4d4b23cd4c073a7c341de03e0642b46c77c7c4cb0bd06f36"
        );
    }

    #[test]
    fn req167_ac2_only_the_rules_and_the_example_value_changed() {
        let strip = |t: String| {
            t.replace(&format!("{SUMMARY_RULES}\n"), "")
                .replace("<25 mots au plus>", "une phrase")
        };
        assert_eq!(
            fingerprint_of(&texts_0_26(strip)),
            "9d89eb9166852d117d282e3559895d7e2e5e3caf46da37b53548f491a589f8c5"
        );
    }

    /// Textes communs aux quatre familles (REQ-193) : les sept textes actuels (version 9), la
    /// ligne `CONCEPT_RULES` + `\n` retirée des deux prompts système.
    fn common_texts() -> Vec<String> {
        let strip = |p: String| p.replacen(&format!("{CONCEPT_RULES}\n"), "", 1);
        let mut texts = texts_v10();
        texts[1] = strip(texts[1].clone());
        texts[4] = strip(texts[4].clone());
        texts.push(
            crate::llm::FormatFault::ALL
                .map(crate::llm::FormatFault::note)
                .join("\n"),
        );
        texts
    }

    /// Textes propres à une famille : `CONCEPT_RULES` pour `markdown`, les trois marques d'extrait (X = 7) pour `données`, aucun pour les autres.
    fn family_texts(f: PromptFamily) -> Vec<String> {
        match f {
            PromptFamily::Markdown => vec![CONCEPT_RULES.to_string()],
            PromptFamily::Data => vec![
                crate::excerpt::mark_with_skeleton(7),
                crate::excerpt::mark_head_only(7),
                crate::excerpt::mark_sql(7, 7),
            ],
            _ => Vec::new(),
        }
    }

    fn family_fingerprint(f: PromptFamily) -> String {
        let mut texts = common_texts();
        texts.extend(family_texts(f));
        fingerprint_of(&texts)
    }

    // Règle d'incrément (REQ-193) : changer un texte commun incrémente les quatre constantes de
    // version ; changer un texte de famille n'incrémente que la sienne ; un texte nouveau propre
    // à une famille s'ajoute à `family_texts`. Mettre ensuite à jour le littéral du test.

    #[test]
    fn req193_ac2_code_prompt_texts_are_frozen_next_to_its_version() {
        assert_eq!(CODE_PROMPT_VERSION, 10);
        assert_eq!(
            family_fingerprint(PromptFamily::Code),
            "9e6aea0b850cd7ed5e65bc224a7ff326f53b24a4b3f4f96209d7a27b9eef66f5",
            "les textes du prompt de la famille code ont changé : incrémentez CODE_PROMPT_VERSION"
        );
    }

    #[test]
    fn req193_ac2_build_prompt_texts_are_frozen_next_to_its_version() {
        assert_eq!(BUILD_PROMPT_VERSION, 11);
        assert_eq!(
            family_fingerprint(PromptFamily::Build),
            "9e6aea0b850cd7ed5e65bc224a7ff326f53b24a4b3f4f96209d7a27b9eef66f5",
            "les textes du prompt de la famille build ont changé : incrémentez BUILD_PROMPT_VERSION"
        );
    }

    #[test]
    fn req193_ac2_data_prompt_texts_are_frozen_next_to_its_version() {
        assert_eq!(DATA_PROMPT_VERSION, 12);
        assert_eq!(
            family_fingerprint(PromptFamily::Data),
            "1ee976e3afda38cd6ff4afbc39173fd78f3fa0a80324a4d6740cecf18f210b40",
            "les textes du prompt de la famille données ont changé : incrémentez DATA_PROMPT_VERSION"
        );
    }

    #[test]
    fn req193_ac2_markdown_prompt_texts_are_frozen_next_to_its_version() {
        assert_eq!(MARKDOWN_PROMPT_VERSION, 10);
        assert_eq!(
            family_fingerprint(PromptFamily::Markdown),
            "d08806bf925afbc5378c560164b731c1276013b9291e62120e72f4250d4377fe",
            "les textes du prompt de la famille markdown ont changé : incrémentez MARKDOWN_PROMPT_VERSION"
        );
        assert_ne!(
            family_fingerprint(PromptFamily::Markdown),
            family_fingerprint(PromptFamily::Code)
        );
    }

    #[test]
    fn req193_ac3_concept_rules_are_in_each_system_prompt_and_in_no_common_text() {
        let prompts = [
            enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
            lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
        ];
        for p in &prompts {
            assert_eq!(p.matches(CONCEPT_RULES).count(), 1);
        }
        let common = common_texts();
        for t in &common {
            assert!(!t.contains(CONCEPT_RULES));
        }
        let line = format!("{CONCEPT_RULES}\n");
        for (i, p) in [(1, &prompts[0]), (4, &prompts[1])] {
            // Position de la ligne dans le prompt envoyé, relevée avant le retrait.
            let pos = p.find(&line).unwrap();
            let mut back = common[i].clone();
            back.insert_str(pos, &line);
            assert_eq!(&back, p);
        }
    }

    #[test]
    fn req193_ac1_each_file_type_has_the_version_of_its_family() {
        use FileType::*;
        let expected = [
            (Java, 1),
            (Kotlin, 1),
            (Groovy, 1),
            (Scala, 1),
            (JavaScript, 1),
            (TypeScript, 1),
            (Maven, 2),
            (Ivy, 2),
            (Gradle, 2),
            (Npm, 2),
            (Sql, 3),
            (Xml, 3),
            (Json, 3),
            (Yaml, 3),
            (Markdown, 4),
        ];
        assert_eq!(expected.len(), FileType::ALL.len());
        let v = PromptVersions {
            code: 1,
            build: 2,
            data: 3,
            markdown: 4,
            ..PromptVersions::CURRENT
        };
        for (t, n) in expected {
            assert_eq!(v.of(t), n, "{t:?}");
        }
        for t in FileType::ALL {
            assert!(expected.iter().any(|(e, _)| *e == t), "{t:?}");
            let want = match t {
                Sql | Xml | Json | Yaml => 12,
                Maven | Ivy | Gradle | Npm => 11,
                _ => 10,
            };
            assert_eq!(PromptVersions::CURRENT.of(t), want, "{t:?}");
        }
    }

    #[test]
    fn req193_ac2_rest_of_version_table() {
        let v = PromptVersions {
            code: 1,
            build: 2,
            data: 3,
            markdown: 4,
            ..PromptVersions::CURRENT
        };
        assert_eq!(v.of_path("src/A.java"), 1);
        assert_eq!(v.of_path("pom.xml"), 2);
        assert_eq!(v.of_path("app.yml"), 3);
        assert_eq!(v.of_path("docs/a.md"), 4);
        assert_eq!(v.of_path("inconnu.bin"), 1);
        assert_eq!(
            [
                CODE_PROMPT_VERSION,
                BUILD_PROMPT_VERSION,
                DATA_PROMPT_VERSION,
                MARKDOWN_PROMPT_VERSION
            ],
            [10, 11, 12, 10]
        );
    }

    #[test]
    fn req265_ac2_masking_off_adds_1000_to_targeted_types_only() {
        use FileType::*;
        let off = PromptVersions {
            masking_off: true,
            ..PromptVersions::CURRENT
        };
        let mut targeted = 0;
        for t in FileType::ALL {
            let base = PromptVersions::CURRENT.of(t);
            if matches!(t, Yaml | Json | Xml | Sql | Npm) {
                targeted += 1;
                assert_eq!(off.of(t), base + 1000, "{t:?}");
            } else {
                assert_eq!(off.of(t), base, "{t:?}");
            }
        }
        assert_eq!(targeted, 5);
        assert_eq!(FileType::ALL.len(), 15);
        assert_eq!(
            [
                CODE_PROMPT_VERSION,
                BUILD_PROMPT_VERSION,
                DATA_PROMPT_VERSION,
                MARKDOWN_PROMPT_VERSION
            ],
            [10, 11, 12, 10]
        );
    }

    #[test]
    fn req265_ac2_call_key_differs_with_masking_state() {
        use crate::store::{call_key, CallKeyParts};
        let key = |v: PromptVersions| {
            call_key(&CallKeyParts {
                model: "m",
                language: "français",
                prompt_version: v.of_path("app.yml"),
                path: "app.yml",
                rank: 1,
                total: 1,
                lines: None,
                text: "a: 1",
                requested: &[],
                known: &[],
            })
        };
        let off = PromptVersions {
            masking_off: true,
            ..PromptVersions::CURRENT
        };
        assert_ne!(key(PromptVersions::CURRENT), key(off));
    }

    const TYPES: &str = "code Java/Kotlin/Groovy/Scala/JavaScript/TypeScript, build Maven/Ivy/Gradle/npm, données SQL/XML/JSON/YAML, ou Markdown";

    #[test]
    fn req171_ac1_file_types_phrase_in_both_system_prompts() {
        assert_eq!(file_types_phrase(), TYPES);
        let one = enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        let lot = lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        let a = format!("à partir d'un fichier ({TYPES}) et tu réponds");
        let b = format!("à partir de plusieurs fichiers ({TYPES}) et tu réponds");
        assert_eq!(one.matches(&a).count(), 1);
        assert_eq!(lot.matches(&b).count(), 1);
        assert!(!one.contains("Maven/Gradle/Ivy") && !lot.contains("Maven/Gradle/Ivy"));
    }

    fn sample_path(l: crate::parse::Lang) -> &'static str {
        use crate::parse::Lang;
        match l {
            Lang::Java => "A.java",
            Lang::Kotlin => "A.kt",
            Lang::KotlinGradle => "build.gradle.kts",
            Lang::Scala => "A.scala",
            Lang::Groovy => "A.groovy",
            Lang::GroovyGradle => "build.gradle",
            Lang::Xml => "pom.xml",
            Lang::Markdown => "a.md",
            Lang::Sql => "a.sql",
            Lang::DataXml => "a.xml",
            Lang::Json => "a.json",
            Lang::Yaml => "a.yaml",
            Lang::JavaScript => "a.js",
            Lang::TypeScript => "a.ts",
            Lang::Tsx => "a.tsx",
            Lang::PackageJson => "package.json",
        }
    }

    #[test]
    fn req171_ac2_every_file_type_and_language_is_named() {
        use crate::parse::Lang;
        let phrase = file_types_phrase();
        let words: Vec<&str> = phrase.split([' ', '/', ',']).collect();
        for t in FileType::ALL {
            assert!(words.contains(&file_type_label(t).1), "{t:?}");
        }
        let all = [
            Lang::Java,
            Lang::Kotlin,
            Lang::KotlinGradle,
            Lang::Scala,
            Lang::Groovy,
            Lang::GroovyGradle,
            Lang::Xml,
            Lang::Markdown,
            Lang::Sql,
            Lang::DataXml,
            Lang::Json,
            Lang::Yaml,
            Lang::JavaScript,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::PackageJson,
        ];
        assert_eq!(all.len(), 16);
        for l in all {
            let path = sample_path(l);
            assert_eq!(
                Lang::from_path(std::path::Path::new(path)),
                Some(l),
                "{path}"
            );
            let t = FileType::of(path).unwrap_or_else(|| panic!("{path}"));
            assert!(words.contains(&file_type_label(t).1), "{path}");
        }
    }

    const CONCEPTS: &str = "Concepts : chacun est une notion métier ou technique durable que le document définit ou explique, jamais un titre de section ni un mot générique (introduction, exemple, présentation, sommaire, remarque) ; son nom tient en 1 à 4 mots, au singulier, sans article, et désigne la notion elle-même (« commande », pas « les commandes » ni « gestion des commandes ») ; au plus 8 concepts par fichier dans une même réponse ; sans notion de ce genre, \"concepts\": [].";

    #[test]
    fn req175_ac1_concept_rules_text_and_placement() {
        assert_eq!(CONCEPT_RULES, CONCEPTS);
        for l in ["français", "anglais"] {
            let r = language_rules(l);
            let one = format!("\n{r}\n{CONCEPT_RULES}\nLes résumés ne portent que");
            assert_eq!(enrichment_system_prompt(l).matches(&one).count(), 1, "{l}");
            let lot = format!(
                "\n{r}\n{CONCEPT_RULES}\nProduis exactement une entrée par fichier fourni."
            );
            assert_eq!(lot_system_prompt(l).matches(&lot).count(), 1, "{l}");
        }
        for needle in ["extrait", "morceau"] {
            assert!(!CONCEPT_RULES.contains(needle), "{needle}");
        }
        let items = lot_items();
        for u in [
            user_prompt(None),
            user_prompt(Some(EXCERPT_2_3)),
            lot_user_prompt(&items),
        ] {
            assert!(!u.contains(CONCEPT_RULES) && !u.contains("Concepts :"));
        }
    }

    const GENRES: &str = "Angle du résumé selon le genre du nœud, donné par le préfixe de son identifiant : module → ce que le module produit et à quoi lui servent ses dépendances ; type → sa responsabilité ; method → son effet et ce qu'elle renvoie ; table → son rôle métier et ses colonnes clés ; document → le sujet du document, ou ce que décrit ou configure un fichier de données (SQL, XML, JSON, YAML) ; section → le sujet qu'elle traite.";

    #[test]
    fn req171_ac3_kind_rules_text_placement_and_genres() {
        use crate::model::NodeKind;
        assert_eq!(KIND_RULES, GENRES);
        let one = format!("\n{EXCERPT_RULES}\n{KIND_RULES}\n");
        assert_eq!(
            enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE)
                .matches(&one)
                .count(),
            1
        );
        let lot_p = lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        let lot = format!("\n{SUMMARY_RULES}\n{KIND_RULES}\n");
        assert_eq!(lot_p.matches(&lot).count(), 1);
        assert!(!lot_p.contains(EXCERPT_RULES));
        let excluded = ["package", "key", "dependency", "concept"];
        for k in NodeKind::ALL {
            let needle = format!("{} → ", k.as_str());
            if excluded.contains(&k.as_str()) {
                assert!(!KIND_RULES.contains(&needle), "{needle}");
            } else {
                assert_eq!(KIND_RULES.matches(&needle).count(), 1, "{needle}");
            }
        }
    }

    #[test]
    fn req169_ac1_excerpt_rules_text_and_placement() {
        let needle = format!("\n{SUMMARY_RULES}\n{EXCERPT_RULES}\n");
        assert_eq!(
            enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE)
                .matches(&needle)
                .count(),
            1
        );
        let lot = lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        assert!(!lot.contains(EXCERPT_RULES));
        assert_eq!(lot.matches(&format!("\n{SUMMARY_RULES}\n")).count(), 1);
    }

    #[test]
    fn req169_ac2_user_prompt_first_line() {
        let none = user_prompt(None);
        assert_eq!(
            enrichment_user_prompt("a.md", None, "t", &ids(&["document:a.md"])),
            format!("Fichier : a.md\n\nContenu :\nt\n\nNœuds du fichier :\n[1] a.md — document:a.md\n\n{INSTRUCTION}")
        );
        let some = user_prompt(Some(EXCERPT_2_3));
        assert_eq!(some.lines().next().unwrap(), "Fichier : a.md");
        assert!(!none.contains("morceau") && !some.contains("morceau"));
        assert!(!none.contains("extrait"));
    }

    const INSTRUCTION: &str =
        "Réponds par l'objet JSON demandé, sur les seuls nœuds de cette liste.";

    #[test]
    fn req267_ac1_content_before_list() {
        assert_eq!(
            user_prompt(None),
            format!("Fichier : a.md\n\nContenu :\ntexte a\n\nNœuds du fichier :\n[1] a.md — document:a.md\n[2] x — section:a.md#x\n\n{INSTRUCTION}")
        );
    }

    #[test]
    fn req267_ac2_excerpt_hint_after_list() {
        let some = user_prompt(Some(EXCERPT_2_3));
        assert_eq!(some.lines().next().unwrap(), "Fichier : a.md");
        assert!(some.ends_with(&format!(
            "[2] x — section:a.md#x\n\n(extrait 2/3, lignes 10–20)\n{INSTRUCTION}"
        )));
    }

    #[test]
    fn req267_ac3_excerpt_rules_start() {
        assert!(EXCERPT_RULES.starts_with("Un fichier trop long est envoyé en extraits : une ligne \"(extrait k/n, lignes a–b)\" suit alors la liste des nœuds et tu ne vois que ces lignes. À partir du deuxième extrait,"));
        assert!(EXCERPT_RULES
            .ends_with(&EXTRAIT[EXTRAIT.find(" À partir du deuxième extrait,").unwrap()..]));
    }

    #[test]
    fn req267_ac4_versions() {
        assert_eq!(
            (
                CODE_PROMPT_VERSION,
                BUILD_PROMPT_VERSION,
                DATA_PROMPT_VERSION,
                MARKDOWN_PROMPT_VERSION
            ),
            (10, 11, 12, 10)
        );
    }

    #[test]
    fn req236_ac1_numbered_list_in_user_prompt() {
        assert_eq!(
            user_prompt(None),
            format!("Fichier : a.md\n\nContenu :\ntexte a\n\nNœuds du fichier :\n[1] a.md — document:a.md\n[2] x — section:a.md#x\n\n{INSTRUCTION}")
        );
        let line = |id: &str, rank: usize| {
            let mut v = vec!["document:z".to_string(); rank - 1];
            v.push(id.to_string());
            let p = enrichment_user_prompt("f", None, "t", &v);
            p.lines()
                .find(|l| l.starts_with(&format!("[{rank}]")))
                .unwrap()
                .to_string()
        };
        assert_eq!(
            line("method:shop.Order.place(int)", 3),
            "[3] place — method:shop.Order.place(int)"
        );
        assert_eq!(line("type:p.B", 1), "[1] B — type:p.B");
        assert_eq!(
            line("document:docs/guide.md", 1),
            "[1] guide.md — document:docs/guide.md"
        );
    }

    #[test]
    fn req236_ac2_lot_user_prompt_numbers_files_and_nodes() {
        assert_eq!(
            lot_user_prompt(&lot_items()),
            "=== FICHIER [1] : a.md ===\nNœuds du fichier :\n[1] a.md — document:a.md\n[2] x — section:a.md#x\n\nContenu :\ntexte a\n=== FIN DU FICHIER : a.md ===\n\n=== FICHIER [2] : B.java ===\nNœuds du fichier :\n[3] B — type:p.B\n\nContenu :\ntexte b\n=== FIN DU FICHIER : B.java ===\n\n"
        );
    }

    #[test]
    fn req236_ac3_schema_lines_in_system_prompts() {
        let one = enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        assert!(one.contains("Schéma : {\"summaries\": [{\"n\": <numéro de la liste>, \"summary\": \"<25 mots au plus>\"}], \"concepts\": [{\"name\": \"nom du concept\", \"summary\": \"<25 mots au plus> (optionnel)\", \"sections\": [\"<identifiant de section de la liste>\"]}]}.\n"));
        let lot = lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        assert!(lot.contains("Schéma : {\"files\": [{\"f\": <numéro du fichier>, \"summaries\": [{\"n\": <numéro de la liste de ce fichier>, \"summary\": \"<25 mots au plus>\"}], \"concepts\": [{\"name\": \"nom du concept\", \"summary\": \"<25 mots au plus> (optionnel)\", \"sections\": [\"<identifiant de section de la liste de ce fichier>\"]}]}]}.\n"));
    }

    #[test]
    fn req236_ac4_number_rules_after_schema_and_no_node_or_file_key() {
        assert_eq!(
            NUMBER_RULES,
            "Chaque nœud de la liste s'écrit [numéro] nom — identifiant. Dans \"n\", donne le numéro écrit à côté du nom du nœud résumé, sans recopier son identifiant."
        );
        for p in [
            enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
            lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
        ] {
            let schema = p.lines().position(|l| l.starts_with("Schéma")).unwrap();
            assert_eq!(p.lines().nth(schema + 1).unwrap(), NUMBER_RULES);
            assert!(!p.contains("\"node\"") && !p.contains("\"file\""));
        }
        assert!(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE).contains(
            "Chaque fichier est introduit par une ligne `=== FICHIER [<f>] : <chemin> ===`, suivie de la liste de ses nœuds puis de son contenu, jusqu'à la ligne `=== FIN DU FICHIER : <chemin> ===`. Les numéros de nœuds continuent d'un fichier au suivant."
        ));
    }

    #[test]
    fn req167_ac1_summary_rules_in_both_system_prompts() {
        assert_eq!(
            SUMMARY_RULES,
            "Résumés (de nœud ou de concept) : une phrase de 25 mots au plus, qui dit le rôle ou la responsabilité de l'élément (ce qu'il fait et à quoi il sert), pas sa syntaxe ni sa signature ; ne répète pas son nom ; n'écris que ce que le contenu montre, sans rien supposer ni inventer. Omets (aucune entrée dans \"summaries\") un accesseur ou un mutateur trivial (il lit ou écrit un champ, sans autre effet) et une section vide ou réduite à son titre."
        );
        let needle = format!("\n{SUMMARY_RULES}\n");
        for p in [
            enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
            lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
        ] {
            assert_eq!(p.matches(&needle).count(), 1);
            // « une phrase » figure dans SUMMARY_RULES : on vérifie l'absence de l'ancienne valeur d'exemple.
            assert!(!p.contains("\"summary\": \"une phrase"));
            assert!(p.contains("\"summary\": \"<25 mots au plus>\""));
            assert!(p.contains("\"summary\": \"<25 mots au plus> (optionnel)\""));
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn req024_ac4_filters_out_of_list_items() {
        let nodes = ids(&["type:com.acme.A", "section:docs/g.md#intro"]);
        let json = r#"{"version":2,
            "summaries":[{"node":"type:com.acme.A","summary":" Fait A. "},
                         {"node":"type:com.acme.Inexistant","summary":"x"}],
            "concepts":[{"name":"Facturation  Client","summary":"s","sections":["section:docs/g.md#intro"]},
                        {"name":"Autre","sections":["section:other.md#x"]}]}"#;
        let e = Enrichment::parse(json, "docs/g.md", &nodes).unwrap();
        assert_eq!(
            e.summaries,
            vec![("type:com.acme.A".into(), "Fait A.".into())]
        );
        assert_eq!(e.concepts.len(), 1);
        assert_eq!(e.concepts[0].name, "facturation client");
        assert_eq!(e.rejected.len(), 2, "{:?}", e.rejected);
    }

    #[test]
    fn req024_ac4_concept_in_non_markdown_is_rejected() {
        let nodes = ids(&["type:A"]);
        let json =
            r#"{"version":2,"summaries":[],"concepts":[{"name":"X","sections":["type:A"]}]}"#;
        let e = Enrichment::parse(json, "A.java", &nodes).unwrap();
        assert!(e.concepts.is_empty());
        assert_eq!(e.rejected.len(), 1);
    }

    #[test]
    fn req024_ac5_non_conforming_responses() {
        let n = ids(&[]);
        for bad in [
            "pas du json",
            r#"{"version":1,"summaries":[],"concepts":[]}"#,
            r#"{"version":2,"summaries":[],"concepts":[],"extra":1}"#,
            r#"{"version":2,"summaries":[{"node":"a","summary":"b","x":1}],"concepts":[]}"#,
            r#"{"version":2,"summaries":[]}"#,
        ] {
            assert!(Enrichment::parse(bad, "a.md", &n).is_err(), "{bad}");
        }
        let ok =
            Enrichment::parse(r#"{"version":2,"summaries":[],"concepts":[]}"#, "a.md", &n).unwrap();
        assert_eq!(ok, Enrichment::default());
    }

    #[test]
    fn req201_ac1_version_is_optional_but_checked_when_present() {
        let n = ids(&[]);
        let without = Enrichment::parse(r#"{"summaries":[],"concepts":[]}"#, "a.md", &n).unwrap();
        let with =
            Enrichment::parse(r#"{"version":2,"summaries":[],"concepts":[]}"#, "a.md", &n).unwrap();
        assert_eq!(without, with);
        let e = Enrichment::parse(r#"{"version":1,"summaries":[],"concepts":[]}"#, "a.md", &n)
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("version 1 non prise en charge (attendu : 2)"),
            "{e}"
        );
        assert_eq!(
            crate::llm::FormatFault::of(&e),
            Some(crate::llm::FormatFault::Version)
        );
        let e = Enrichment::parse(r#"{"summaries":[],"concepts":[],"extra":1}"#, "a.md", &n)
            .unwrap_err();
        assert_eq!(
            crate::llm::FormatFault::of(&e),
            Some(crate::llm::FormatFault::UnknownField)
        );
        let items = [item("A.java", &[])];
        let entry = r#"{"file":"A.java","summaries":[],"concepts":[]}"#;
        let a = parse_lot(&format!(r#"{{"files":[{entry}]}}"#), &items).unwrap();
        let b = parse_lot(&format!(r#"{{"version":3,"files":[{entry}]}}"#), &items).unwrap();
        assert_eq!(a.into_results(), b.into_results());
        let e = parse_lot(r#"{"version":2,"files":[]}"#, &items).unwrap_err();
        assert!(
            e.to_string()
                .contains("version 2 non prise en charge pour un lot (attendu : 3)"),
            "{e}"
        );
    }

    #[test]
    fn req201_ac2_system_prompts_do_not_name_version() {
        let one = enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        let lot = lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE);
        assert!(!one.contains("version") && !lot.contains("version"));
        assert!(one.ends_with(r#"réponds {"summaries": [], "concepts": []}."#));
        assert_eq!(
            [
                CODE_PROMPT_VERSION,
                BUILD_PROMPT_VERSION,
                DATA_PROMPT_VERSION,
                MARKDOWN_PROMPT_VERSION
            ],
            [10, 11, 12, 10]
        );
        assert_eq!(crate::llm::ask::ASK_PROMPT_VERSION, 2);
    }

    #[test]
    fn req180_ac1_normalize_concept_name_table() {
        for (input, want) in [
            ("« Facturation ».", "facturation"),
            ("\"Gestion  des Commandes\"", "gestion des commandes"),
            ("‘API’", "api"),
            ("Facturation…", "facturation"),
            ("Qu'est-ce ?", "qu'est-ce"),
            ("l'API", "l'api"),
            ("Node.js", "node.js"),
            ("C#", "c#"),
            ("API (REST)", "api (rest)"),
            ("Facturation  Client", "facturation client"),
            ("\"...\"", ""),
            ("« »", ""),
            (".", ""),
        ] {
            assert_eq!(normalize_concept_name(input), want, "{input}");
        }
    }

    #[test]
    fn req024_ac4_empty_id_or_name_filtered_rest_applied() {
        let n = ids(&["type:A", "section:a.md#s"]);
        let json = r#"{"version":2,
            "summaries":[{"node":" ","summary":"x"},{"node":"type:A","summary":"ok"}],
            "concepts":[{"name":"  ","sections":["section:a.md#s"]},
                        {"name":"Bon","sections":["section:a.md#s"]}]}"#;
        let e = Enrichment::parse(json, "a.md", &n).unwrap();
        assert_eq!(e.summaries, vec![("type:A".into(), "ok".into())]);
        assert_eq!(e.concepts.len(), 1);
        assert_eq!(e.rejected.len(), 2, "{:?}", e.rejected);
    }

    #[test]
    fn req024_ac4_concept_section_needs_section_prefix() {
        let n = ids(&["type:A"]);
        let json =
            r#"{"version":2,"summaries":[],"concepts":[{"name":"X","sections":["type:A"]}]}"#;
        let e = Enrichment::parse(json, "a.md", &n).unwrap();
        assert!(e.concepts.is_empty());
        assert_eq!(e.rejected.len(), 1);
    }

    /// Positions (en octets) de la fin de chaque `}` hors chaîne de `text`, à la profondeur donnée.
    fn closings_at(text: &str, depth: usize) -> Vec<usize> {
        let (mut d, mut in_str, mut esc) = (0usize, false, false);
        let mut out = Vec::new();
        for (i, c) in text.char_indices() {
            if in_str {
                match (esc, c) {
                    (true, _) => esc = false,
                    (false, '\\') => esc = true,
                    (false, '"') => in_str = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => d += 1,
                '}' | ']' => {
                    d -= 1;
                    if c == '}' && d == depth {
                        out.push(i + 1);
                    }
                }
                _ => {}
            }
        }
        out
    }

    const TRICKY: &str = "Rôle « é » avec \\\"guillemets\\\", } et ] dedans.";

    #[test]
    fn req232_ac1_lot_cut_at_every_char_boundary() {
        let items = [
            item("A.java", &["type:A"]),
            item("B.java", &["type:B"]),
            item("C.java", &["type:C"]),
        ];
        let entry = |f: &str, n: &str| {
            format!(
                r#"{{"file":"{f}","summaries":[{{"node":"{n}","summary":"{TRICKY}"}}],"concepts":[]}}"#
            )
        };
        let full = format!(
            r#"{{"version":3,"files":[{},{},{}]}}"#,
            entry("A.java", "type:A"),
            entry("B.java", "type:B"),
            entry("C.java", "type:C")
        );
        assert!(parse_lot(&full, &items).is_ok());
        let closes = closings_at(&full, 2);
        assert_eq!(closes.len(), 3);
        for (cut, _) in full.char_indices().chain([(full.len(), ' ')]) {
            let l = parse_lot_partial(&full[..cut], &items);
            let want = closes.iter().filter(|&&c| c <= cut).count();
            let got = l.files.iter().filter(|(_, r)| r.is_ok()).count();
            assert_eq!(got, want, "coupe à {cut}");
            assert_eq!(l.files.len(), 3);
        }
    }

    #[test]
    fn req232_ac1_call_cut_at_every_char_boundary() {
        let s = |n: &str| format!(r#"{{"node":"type:{n}","summary":"{TRICKY}"}}"#);
        let c = |n: &str| format!(r#"{{"name":"{n}","summary":null,"sections":["section:s"]}}"#);
        let full = format!(
            r#"{{"version":2,"summaries":[{},{},{}],"concepts":[{},{}]}}"#,
            s("A"),
            s("B"),
            s("C"),
            c("un"),
            c("deux")
        );
        let known = ids(&["type:A", "type:B", "type:C", "section:s"]);
        let closes = closings_at(&full, 2);
        assert_eq!(closes.len(), 5);
        for (cut, _) in full.char_indices().chain([(full.len(), ' ')]) {
            let e = Enrichment::parse_partial(&full[..cut], "d.md", &known, &known);
            let n = closes.iter().filter(|&&x| x <= cut).count();
            assert_eq!(e.summaries.len(), n.min(3), "coupe à {cut}");
            assert_eq!(e.concepts.len(), n.saturating_sub(3), "coupe à {cut}");
        }
    }

    #[test]
    fn req232_ac2_partial_reader_edge_cases() {
        let known = ids(&["type:A", "type:B"]);
        let nothing = |t: &str| {
            let e = Enrichment::parse_partial(t, "A.java", &known, &known);
            assert!(e.summaries.is_empty() && e.concepts.is_empty(), "{t:?}");
        };
        nothing("");
        nothing("pas de json");
        nothing("x");
        let one = r#"{"version":2,"summaries":[{"node":"type:A","summary":"Rôle de a."},{"node":"type:B","summary":"Rôle de b."}],"concepts":[]}"#;
        let plain = Enrichment::parse_partial(one, "A.java", &known, &known);
        assert_eq!(plain.summaries.len(), 2);
        let fenced =
            Enrichment::parse_partial(&format!("```json\n{one}"), "A.java", &known, &known);
        assert_eq!(fenced, plain);
        nothing(&one.replace(r#""version":2"#, r#""version":99"#));
        nothing(&one.replace(r#""concepts":[]"#, r#""version":99"#));
        // champ inconnu dans l'élément 2
        let unk = r#"{"summaries":[{"node":"type:A","summary":"Rôle de a."},{"node":"type:B","summary":"Rôle de b.","x":1}],"concepts":[]}"#;
        let e = Enrichment::parse_partial(unk, "A.java", &known, &known);
        assert_eq!(e.summaries.len(), 1);
        // concepts avant summaries
        let md = ids(&["section:s", "type:A"]);
        let rev = r#"{"concepts":[{"name":"notion","summary":null,"sections":["section:s"]}],"summaries":[{"node":"section:s","summary":"Rôle de s."}]}"#;
        let e = Enrichment::parse_partial(rev, "d.md", &md, &md);
        assert_eq!((e.summaries.len(), e.concepts.len()), (1, 1));
        // clés hors schéma : les éléments complets avant elles restent gardés
        let e = Enrichment::parse_partial(
            r#"{"summaries":[{"node":"type:A","summary":"Rôle de a."}],"files":[],"concepts":[]}"#,
            "A.java",
            &known,
            &known,
        );
        assert_eq!(e.summaries.len(), 1);
        let e = Enrichment::parse_partial(
            r#"{"files":[],"summaries":[{"node":"type:A","summary":"Rôle de a."}]}"#,
            "A.java",
            &known,
            &known,
        );
        assert!(e.summaries.is_empty());
        let items = [item("A.java", &["type:A"])];
        let entry = r#"{"file":"A.java","summaries":[{"node":"type:A","summary":"Rôle de a."}],"concepts":[]}"#;
        let l = parse_lot_partial(
            &format!(r#"{{"version":3,"concepts":[],"files":[{entry}]}}"#),
            &items,
        );
        assert!(l.files[0].1.is_err());
        let l = parse_lot_partial(
            &format!(r#"{{"files":[{entry}],"summaries":[],"concepts":[]}}"#),
            &items,
        );
        assert!(l.files[0].1.is_ok());
        // lot : entrée sans concepts
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let lot = r#"{"files":[{"file":"A.java","summaries":[{"node":"type:A","summary":"Rôle de a."}],"concepts":[]},{"file":"B.java","summaries":[{"node":"type:B","summary":"Rôle de b."}]}]}"#;
        let l = parse_lot_partial(lot, &items);
        assert!(l.files[0].1.is_ok() && l.files[1].1.is_err());
        let l = parse_lot_partial(
            &lot.replace(r#"{"files""#, r#"{"version":9,"files""#),
            &items,
        );
        assert!(l.files.iter().all(|(_, r)| r.is_err()));
    }

    fn item(file: &str, nodes: &[&str]) -> LotItem {
        LotItem {
            file: file.into(),
            text: String::new(),
            node_ids: ids(nodes),
        }
    }

    #[test]
    fn req027_ac6_parse_lot_conforming() {
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let json = r#"{"version":3,"files":[
            {"file":"B.java","summaries":[{"node":"type:B","summary":"Rôle de b."},{"node":"type:A","summary":"x"}],"concepts":[]},
            {"file":"A.java","summaries":[{"node":"type:A","summary":"Rôle de a."}],"concepts":[]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        assert!(l.rejected.is_empty());
        assert_eq!(l.files[0].0, "A.java");
        let a = l.files[0].1.as_ref().unwrap();
        assert_eq!(a.summaries, vec![("type:A".into(), "Rôle de a.".into())]);
        let b = l.files[1].1.as_ref().unwrap();
        assert_eq!(b.summaries, vec![("type:B".into(), "Rôle de b.".into())]);
        assert_eq!(b.rejected.len(), 1, "{:?}", b.rejected);
    }

    #[test]
    fn req027_ac8_parse_lot_non_conforming() {
        let items = [item("A.java", &[]), item("B.java", &[])];
        for bad in [
            "pas du json",
            r#"{"version":2,"files":[]}"#,
            r#"{"version":3,"files":[],"extra":1}"#,
            r#"{"version":3,"files":[{"file":"A.java","summaries":[],"concepts":[],"x":1}]}"#,
            r#"{"version":3,"files":[{"file":"A.java","summaries":[]}]}"#,
        ] {
            assert!(parse_lot(bad, &items).is_err(), "{bad}");
        }
    }

    #[test]
    fn req027_ac7_parse_lot_out_of_lot_duplicate_and_missing() {
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let json = r#"{"version":3,"files":[
            {"file":"A.java","summaries":[{"node":"type:A","summary":"Rôle de a."}],"concepts":[]},
            {"file":"autre/Inconnu.java","summaries":[],"concepts":[]},
            {"file":"A.java","summaries":[{"node":"type:A","summary":"bis"}],"concepts":[]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        assert_eq!(l.rejected.len(), 2, "{:?}", l.rejected);
        assert!(l.rejected[0].contains("autre/Inconnu.java"));
        assert!(l.rejected[1].contains("A.java"));
        let a = l.files[0].1.as_ref().unwrap();
        assert_eq!(a.summaries, vec![("type:A".into(), "Rôle de a.".into())]);
        let e = l.files[1].1.as_ref().unwrap_err().to_string();
        assert!(
            e.contains("B.java") && e.contains("absent de la réponse du lot"),
            "{e}"
        );
        let r = l.into_results();
        let a = r[0].1.as_ref().unwrap();
        assert_eq!(a.rejected.len(), 2);
    }

    #[test]
    fn req198_ac1_parse_lot_normalizes_response_paths() {
        let items = [
            item("src/A.java", &["type:A"]),
            item("src/B.java", &["type:B"]),
            item("docs/c.md", &["document:docs/c.md"]),
        ];
        let json = r#"{"version":3,"files":[
            {"file":"./src/A.java","summaries":[{"node":"type:Z","summary":"hors liste"}],"concepts":[]},
            {"file":" src\\B.java ","summaries":[],"concepts":[]},
            {"file":"docs/c.md","summaries":[],"concepts":[]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        assert!(l.rejected.is_empty(), "{:?}", l.rejected);
        assert!(l.files.iter().all(|(_, r)| r.is_ok()), "{:?}", l.files);
        let a = l.files[0].1.as_ref().unwrap();
        assert_eq!(a.rejected.len(), 1, "{:?}", a.rejected);
    }

    #[test]
    fn req198_ac2_parse_lot_exact_path_first_and_ambiguity() {
        let items = [item("a\\b.md", &[]), item("a/b.md", &[])];
        let json = r#"{"version":3,"files":[
            {"file":"a\\b.md","summaries":[],"concepts":[]},
            {"file":"a/b.md","summaries":[],"concepts":[]},
            {"file":"./a/b.md","summaries":[],"concepts":[]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        assert!(l.files.iter().all(|(_, r)| r.is_ok()), "{:?}", l.files);
        assert_eq!(l.rejected, vec!["entrée en double pour ./a/b.md"]);

        let items = [item("x/a.md", &[]), item("y/a.md", &[])];
        let json = r#"{"version":3,"files":[{"file":"a.md","summaries":[],"concepts":[]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        assert_eq!(l.rejected, vec!["entrée pour a.md (fichier hors du lot)"]);
        assert!(l.files.iter().all(|(_, r)| r.is_err()));
    }

    #[test]
    fn req027_ac4_parse_lot_concept_in_java_entry_is_dropped() {
        let items = [
            item("A.java", &["type:A"]),
            item("d.md", &["section:d.md#s"]),
        ];
        let json = r#"{"version":3,"files":[
            {"file":"A.java","summaries":[],"concepts":[{"name":"X","sections":["type:A"]}]},
            {"file":"d.md","summaries":[],"concepts":[{"name":"Y","sections":["section:d.md#s"]}]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        let a = l.files[0].1.as_ref().unwrap();
        assert!(a.concepts.is_empty());
        assert_eq!(a.rejected.len(), 1);
        assert_eq!(l.files[1].1.as_ref().unwrap().concepts.len(), 1);
    }

    #[test]
    fn req027_ac7_parse_lot_all_out_of_lot_reports_rejected_in_errors() {
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let json = r#"{"version":3,"files":[
            {"file":"X.java","summaries":[],"concepts":[]},
            {"file":"Y.java","summaries":[],"concepts":[]}]}"#;
        let r = parse_lot(json, &items).unwrap().into_results();
        assert_eq!(r.len(), 2);
        for (f, res) in &r {
            let e = res.as_ref().unwrap_err().to_string();
            assert!(
                e.contains(f) && e.contains("absent de la réponse du lot"),
                "{e}"
            );
            assert!(e.contains("X.java") && e.contains("Y.java"), "{e}");
        }
    }

    fn one_summary(file: &str, nodes: &[&str], node: &str, summary: &str) -> Enrichment {
        let json = serde_json::json!({"version": 2, "concepts": [],
            "summaries": [{"node": node, "summary": summary}]})
        .to_string();
        Enrichment::parse(&json, file, &ids(nodes)).unwrap()
    }

    fn words90() -> String {
        (0..90)
            .map(|i| format!("mot{i:02}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn req178_ac1_resume_replie_sur_une_ligne() {
        let e = one_summary(
            "A.java",
            &["type:A"],
            "type:A",
            "  Première ligne.\n\n  Deuxième\tligne.  ",
        );
        assert_eq!(
            e.summaries,
            vec![("type:A".into(), "Première ligne. Deuxième ligne.".into())]
        );
        assert!(e.rejected.is_empty());
    }

    #[test]
    fn req234_ac1_resume_vide_signale() {
        for text in ["", "\n \t"] {
            let e = one_summary("A.java", &["type:A"], "type:A", text);
            assert!(e.summaries.is_empty(), "{text:?}");
            assert_eq!(e.rejected, vec!["résumé pour type:A (vide)"], "{text:?}");
        }
    }

    #[test]
    fn req234_ac2_resume_vide_cas_limites() {
        let parse =
            |json: &str, nodes: &[&str]| Enrichment::parse(json, "A.java", &ids(nodes)).unwrap();
        let e = parse(
            r#"{"version":2,"concepts":[],"summaries":[
                {"node":"type:A","summary":""},{"node":"type:A","summary":"Rôle."}]}"#,
            &["type:A"],
        );
        assert_eq!(e.summaries, vec![("type:A".into(), "Rôle.".into())]);
        assert_eq!(e.rejected, vec!["résumé pour type:A (vide)"]);
        let e = parse(
            r#"{"version":2,"concepts":[],"summaries":[{"node":"type:Hors","summary":""}]}"#,
            &["type:A"],
        );
        assert_eq!(e.rejected, vec!["résumé pour type:Hors (nœud hors liste)"]);
        let e = parse(
            r#"{"version":2,"concepts":[],"summaries":[{"node":"type:A","summary":"Rôle."}]}"#,
            &["type:A", "type:B"],
        );
        assert!(e.rejected.is_empty());
        let json = r#"{"version":2,"summaries":[],"concepts":[{"name":"Facturation","summary":"","sections":["section:docs/g.md#s1"]}]}"#;
        let e = Enrichment::parse(json, "docs/g.md", &ids(&["section:docs/g.md#s1"])).unwrap();
        assert_eq!(e.concepts.len(), 1);
        assert_eq!(e.concepts[0].summary, None);
        assert!(e.rejected.is_empty());
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let json = r#"{"version":3,"files":[
            {"file":"A.java","summaries":[{"node":"type:A","summary":""}],"concepts":[]},
            {"file":"B.java","summaries":[{"node":"type:B","summary":"Rôle."}],"concepts":[]}]}"#;
        let lot = parse_lot(json, &items).unwrap();
        let a = lot.files[0].1.as_ref().unwrap();
        let b = lot.files[1].1.as_ref().unwrap();
        assert_eq!(a.rejected, vec!["résumé pour type:A (vide)"]);
        assert!(b.rejected.is_empty());
    }

    #[test]
    fn req178_ac2_resume_coupe_a_400_caracteres() {
        let long = words90();
        assert_eq!(long.chars().count(), 539);
        let e = one_summary("A.java", &["type:A"], "type:A", &long);
        let got = &e.summaries[0].1;
        assert!(got.chars().count() <= 400 && got.ends_with('…'));
        let body = got.trim_end_matches('…');
        assert!(long.starts_with(body) && !body.ends_with(' '));
        assert!(long[body.len()..].starts_with(' '));
        let next = long[body.len() + 1..].split(' ').next().unwrap();
        assert!(body.chars().count() + 1 + next.chars().count() + 1 > 400);
        assert_eq!(
            e.rejected,
            vec!["résumé pour type:A : coupé à 400 caractères, fin écartée (539 caractères reçus)"]
        );
        let exact = "é".repeat(400);
        let e = one_summary("A.java", &["type:A"], "type:A", &exact);
        assert_eq!(e.summaries[0].1, exact);
        assert!(e.rejected.is_empty());
        let e = one_summary("A.java", &["type:A"], "type:A", &"x".repeat(500));
        assert_eq!(e.summaries[0].1, format!("{}…", "x".repeat(399)));
    }

    #[test]
    fn req178_ac3_resumes_vides_de_sens_ecartes() {
        let nodes = ["type:A", "type:com.acme.Order", "method:Big.m1"];
        let bad = [
            ("type:A", "N/A"),
            ("type:A", "n/a."),
            ("type:A", "…"),
            ("type:A", "-"),
            ("type:A", "TODO"),
            ("type:A", "Aucun résumé."),
            ("type:A", "Classe A."),
            ("type:A", "A"),
            ("type:com.acme.Order", "Order."),
            ("type:com.acme.Order", "class Order"),
            ("method:Big.m1", "Méthode m1."),
        ];
        for (id, text) in bad {
            let e = one_summary("A.java", &nodes, id, text);
            assert!(e.summaries.is_empty(), "{text}");
            assert_eq!(e.rejected, vec![format!("résumé pour {id} (vide de sens)")]);
        }
        let good = [
            ("type:A", "Le A."),
            ("type:A", "Fait A."),
            ("type:A", "s"),
            ("type:A", "Classe A centrale."),
            ("type:com.acme.Order", "Commande client."),
        ];
        for (id, text) in good {
            let e = one_summary("A.java", &nodes, id, text);
            assert_eq!(e.summaries, vec![(id.to_string(), text.to_string())]);
            assert!(e.rejected.is_empty(), "{text}");
        }
    }

    fn one_concept(summary: &str) -> Enrichment {
        let json = serde_json::json!({"version": 2, "summaries": [],
            "concepts": [{"name": "Facturation", "summary": summary,
                          "sections": ["section:docs/g.md#intro"]}]})
        .to_string();
        Enrichment::parse(&json, "docs/g.md", &ids(&["section:docs/g.md#intro"])).unwrap()
    }

    #[test]
    fn req178_ac4_resume_de_concept() {
        let e = one_concept("Émission\ndes factures.");
        assert_eq!(
            e.concepts[0].summary.as_deref(),
            Some("Émission des factures.")
        );
        assert!(e.rejected.is_empty());
        for bad in ["N/A", "Facturation."] {
            let e = one_concept(bad);
            assert_eq!(e.concepts.len(), 1);
            assert_eq!(e.concepts[0].summary, None);
            assert_eq!(
                e.rejected,
                vec!["concept facturation : résumé vide de sens écarté"]
            );
        }
        let e = one_concept(&words90());
        let s = e.concepts[0].summary.as_deref().unwrap();
        assert!(s.chars().count() <= 400 && s.ends_with('…'));
        assert_eq!(
            e.rejected,
            vec!["concept facturation : résumé coupé à 400 caractères, fin écartée (539 caractères reçus)"]
        );
    }

    #[test]
    fn req178_ac5_regles_par_fichier_d_un_lot() {
        let items = vec![
            LotItem {
                file: "A.java".into(),
                text: String::new(),
                node_ids: ids(&["type:A"]),
            },
            LotItem {
                file: "B.java".into(),
                text: String::new(),
                node_ids: ids(&["type:B"]),
            },
        ];
        let json = r#"{"version":3,"files":[
            {"file":"A.java","summaries":[{"node":"type:A","summary":"N/A"}],"concepts":[]},
            {"file":"B.java","summaries":[{"node":"type:B","summary":"Rôle\nde B."}],"concepts":[]}]}"#;
        let lot = parse_lot(json, &items).unwrap();
        let a = lot.files[0].1.as_ref().unwrap();
        let b = lot.files[1].1.as_ref().unwrap();
        assert!(a.summaries.is_empty());
        assert_eq!(a.rejected, vec!["résumé pour type:A (vide de sens)"]);
        assert_eq!(b.summaries, vec![("type:B".into(), "Rôle de B.".into())]);
        assert!(b.rejected.is_empty());
    }

    #[test]
    fn req178_ac9_elements_sans_saut_de_ligne_ni_texte_recu() {
        let e = one_summary("A.java", &["type:A"], "type:A", "\n\nN/A\n");
        assert_eq!(e.rejected, vec!["résumé pour type:A (vide de sens)"]);
        for text in ["é".repeat(400), "  a   b  ".to_string()] {
            assert!(one_summary("A.java", &["type:A"], "type:A", &text)
                .rejected
                .is_empty());
        }
        let json = r#"{"version":2,"summaries":[],"concepts":[{"name":"« Facturation ».","sections":["type:A"]}]}"#;
        let e = Enrichment::parse(json, "A.java", &ids(&["type:A"])).unwrap();
        assert_eq!(
            e.rejected,
            vec!["concept facturation (fichier non Markdown)"]
        );
    }

    #[test]
    fn req179_ac1_premier_resume_garde() {
        let two = |a: &str, b: &str| {
            let json = serde_json::json!({"version": 2, "concepts": [], "summaries": [
                {"node": "type:A", "summary": a}, {"node": "type:A", "summary": b}]})
            .to_string();
            Enrichment::parse(&json, "A.java", &ids(&["type:A"])).unwrap()
        };
        let e = two("Premier rôle.", "Second rôle.");
        assert_eq!(e.summaries, vec![("type:A".into(), "Premier rôle.".into())]);
        assert_eq!(
            e.rejected,
            vec!["résumé en double pour type:A (premier gardé)"]
        );
        let e = two("Rôle.", "Rôle.");
        assert_eq!(e.summaries.len(), 1);
        assert!(e.rejected.is_empty());
        let e = two("N/A", "Rôle.");
        assert_eq!(e.summaries, vec![("type:A".into(), "Rôle.".into())]);
        assert_eq!(e.rejected, vec!["résumé pour type:A (vide de sens)"]);
    }

    #[test]
    fn req179_ac2_concepts_de_meme_nom_fusionnes() {
        let json = r#"{"version":2,"summaries":[],"concepts":[
            {"name":"Facturation","sections":["section:docs/g.md#s1"]},
            {"name":"« facturation »","summary":"Émission des factures.",
             "sections":["section:docs/g.md#s2","section:docs/g.md#s1"]}]}"#;
        let e = Enrichment::parse(
            json,
            "docs/g.md",
            &ids(&["section:docs/g.md#s1", "section:docs/g.md#s2"]),
        )
        .unwrap();
        assert_eq!(
            e.concepts,
            vec![Concept {
                name: "facturation".into(),
                summary: Some("Émission des factures.".into()),
                sections: ids(&["section:docs/g.md#s1", "section:docs/g.md#s2"]),
            }]
        );
        assert!(e.rejected.is_empty());
    }

    #[test]
    fn req179_ac3_unicite_et_fusion_idempotente() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let nodes = ids(&[
            "type:N0",
            "type:N1",
            "type:N2",
            "section:g.md#a",
            "section:g.md#b",
        ]);
        let names = [
            "Facturation",
            "« facturation »",
            "\"Facturation\".",
            "Client",
            "client…",
        ];
        let texts = [
            "",
            " \n ",
            "N/A",
            "Rôle.",
            "Autre rôle.",
            "Rôle.\n",
            "N2.",
            "x",
        ];
        for _ in 0..200 {
            let summaries: Vec<_> = (0..next(8))
                .map(|_| {
                    let text = if next(6) == 0 {
                        "mot ".repeat(150)
                    } else {
                        texts[next(texts.len() as u64) as usize].to_string()
                    };
                    serde_json::json!({"node": format!("type:N{}", next(3)), "summary": text})
                })
                .collect();
            let concepts: Vec<_> = (0..next(6))
                .map(|_| {
                    let secs: Vec<String> = (0..1 + next(2))
                        .map(|_| format!("section:g.md#{}", ["a", "b"][next(2) as usize]))
                        .collect();
                    serde_json::json!({"name": names[next(names.len() as u64) as usize],
                        "summary": texts[next(texts.len() as u64) as usize], "sections": secs})
                })
                .collect();
            let json =
                serde_json::json!({"version": 2, "summaries": summaries, "concepts": concepts})
                    .to_string();
            let e = Enrichment::parse(&json, "g.md", &nodes).unwrap();
            let mut seen = HashSet::new();
            assert!(e.summaries.iter().all(|(n, _)| seen.insert(n.clone())));
            let mut seen = HashSet::new();
            assert!(e.concepts.iter().all(|c| seen.insert(c.name.clone())));
            let merged = crate::chunk::merge_enrichments(vec![e.clone()]);
            assert_eq!(merged.summaries, e.summaries);
            assert_eq!(merged.concepts, e.concepts);
        }
    }

    #[test]
    fn req180_ac2_nom_normalise_dans_les_elements_de_concept() {
        let json = r#"{"version":2,"summaries":[],"concepts":[
            {"name":"« Facturation ».","summary":"N/A","sections":["section:g.md#a","section:x#y"]}]}"#;
        let e = Enrichment::parse(json, "g.md", &ids(&["section:g.md#a"])).unwrap();
        assert_eq!(e.concepts[0].name, "facturation");
        assert_eq!(
            e.rejected,
            vec![
                "concept facturation : sections hors liste écartées",
                "concept facturation : résumé vide de sens écarté"
            ]
        );

        let json = r#"{"version":2,"summaries":[],"concepts":[{"name":"\"...\"","sections":["section:docs/g.md#intro"]}]}"#;
        let e = Enrichment::parse(json, "docs/g.md", &ids(&["section:docs/g.md#intro"])).unwrap();
        assert!(e.concepts.is_empty());
        assert_eq!(e.rejected, vec!["concept sans nom"]);
    }

    // --- T-327 : réponses à numéros (REQ-237, REQ-238) ---

    fn list_a() -> Vec<String> {
        ids(&["type:A", "method:A.m"])
    }

    fn known_a() -> Vec<String> {
        ids(&["type:A", "method:A.m", "method:A.x"])
    }

    fn numbered(json: &str) -> Result<Enrichment> {
        Enrichment::parse_numbered(json, "A.java", &list_a(), &known_a())
    }

    fn body(summaries: &str) -> String {
        format!(r#"{{"summaries":[{summaries}],"concepts":[]}}"#)
    }

    fn fault_of(r: Result<impl std::fmt::Debug>) -> (Option<FormatFault>, String) {
        let e = r.unwrap_err();
        (FormatFault::of(&e), e.to_string())
    }

    #[test]
    fn req237_ac1_number_is_translated_and_out_of_bounds_dropped() {
        let e = numbered(&body(r#"{"n":2,"summary":"Rôle."}"#)).unwrap();
        assert_eq!(e.summaries, vec![("method:A.m".into(), "Rôle.".into())]);
        assert!(e.rejected.is_empty());
        for n in [0, -1, 3] {
            let e = numbered(&body(&format!(
                r#"{{"n":{n},"summary":"x"}},{{"n":1,"summary":"Garde."}}"#
            )))
            .unwrap();
            assert_eq!(e.summaries, vec![("type:A".into(), "Garde.".into())], "{n}");
        }
        let e = numbered(&body(r#"{"node":"method:A.x","summary":"Rôle."}"#)).unwrap();
        assert_eq!(e.summaries, vec![("method:A.x".into(), "Rôle.".into())]);
        let e = numbered(&body(r#"{"node":"type:Hors","summary":"x"}"#)).unwrap();
        assert_eq!(e.rejected, vec!["résumé pour type:Hors (nœud hors liste)"]);
    }

    #[test]
    fn req237_ac2_rules_apply_after_translation() {
        let e = numbered(&body(r#"{"n":1,"summary":""}"#)).unwrap();
        assert_eq!(e.rejected, vec!["résumé pour type:A (vide)"]);
        let e = numbered(&body(r#"{"n":1,"summary":"N/A"}"#)).unwrap();
        assert_eq!(e.rejected, vec!["résumé pour type:A (vide de sens)"]);
        for second in [
            r#"{"n":1,"summary":"Autre."}"#,
            r#"{"node":"type:A","summary":"Autre."}"#,
        ] {
            let e = numbered(&body(&format!(
                r#"{{"n":1,"summary":"Premier."}},{second}"#
            )))
            .unwrap();
            assert_eq!(e.summaries, vec![("type:A".into(), "Premier.".into())]);
            assert_eq!(
                e.rejected,
                vec!["résumé en double pour type:A (premier gardé)"]
            );
        }
    }

    #[test]
    fn req237_ac3_shape_errors() {
        let (f, m) = fault_of(numbered(&body(r#"{"summary":"x"}"#)));
        assert_eq!(f, Some(FormatFault::MissingField));
        assert!(m.contains("missing field `n`"), "{m}");
        let (f, m) = fault_of(numbered(&body(r#"{"n":1,"node":"type:A","summary":"x"}"#)));
        assert_eq!(f, Some(FormatFault::Shape));
        assert!(m.contains("duplicate field `n`"), "{m}");
        for bad in [r#"{"n":"1","summary":"x"}"#, r#"{"n":1.5,"summary":"x"}"#] {
            assert_eq!(
                fault_of(numbered(&body(bad))).0,
                Some(FormatFault::Shape),
                "{bad}"
            );
        }
        let (f, _) = fault_of(numbered(&body(r#"{"n":1,"summary":"x","y":1}"#)));
        assert_eq!(f, Some(FormatFault::UnknownField));
        let (f, _) = fault_of(numbered(
            r#"{"summaries":[],"concepts":[{"name":"c","summary":null,"sections":[1]}]}"#,
        ));
        assert_eq!(f, Some(FormatFault::Shape));
    }

    fn two_files() -> [LotItem; 2] {
        lot_items()
    }

    fn lot(json: &str) -> LotEnrichment {
        parse_lot(json, &two_files()).unwrap()
    }

    #[test]
    fn req237_ac4_lot_numbers() {
        let l = lot(
            r#"{"files":[{"f":2,"summaries":[{"n":3,"summary":"Rôle."}],"concepts":[]},{"f":1,"summaries":[{"n":2,"summary":"Sujet."}],"concepts":[]}]}"#,
        );
        assert_eq!(
            l.files[0].1.as_ref().unwrap().summaries,
            vec![("section:a.md#x".into(), "Sujet.".into())]
        );
        assert_eq!(
            l.files[1].1.as_ref().unwrap().summaries,
            vec![("type:p.B".into(), "Rôle.".into())]
        );
        let l = lot(r#"{"files":[{"f":2,"summaries":[{"n":1,"summary":"x"}],"concepts":[]}]}"#);
        let b = l.files[1].1.as_ref().unwrap();
        assert!(b.summaries.is_empty());
        assert_eq!(b.rejected, vec!["résumé pour le numéro 1 (hors liste)"]);
        for f in [0, 3] {
            let l = lot(&format!(
                r#"{{"files":[{{"f":{f},"summaries":[],"concepts":[]}},{{"f":2,"summaries":[{{"n":3,"summary":"Rôle."}}],"concepts":[]}}]}}"#
            ));
            assert_eq!(
                l.rejected,
                vec![format!("entrée pour le fichier numéro {f} (hors du lot)")]
            );
            assert!(l.files[0]
                .1
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("absent de la réponse du lot"));
            assert_eq!(
                l.files[1].1.as_ref().unwrap().summaries,
                vec![("type:p.B".into(), "Rôle.".into())]
            );
        }
        let l = lot(
            r#"{"files":[{"f":1,"summaries":[],"concepts":[]},{"f":1,"summaries":[],"concepts":[]}]}"#,
        );
        assert_eq!(l.rejected, vec!["entrée en double pour a.md"]);
        let l = lot(
            r#"{"files":[{"file":"./a.md","summaries":[{"n":1,"summary":"Doc."}],"concepts":[]}]}"#,
        );
        assert_eq!(
            l.files[0].1.as_ref().unwrap().summaries,
            vec![("document:a.md".into(), "Doc.".into())]
        );
        let (f, m) = fault_of(parse_lot(
            r#"{"files":[{"summaries":[],"concepts":[]}]}"#,
            &two_files(),
        ));
        assert_eq!(f, Some(FormatFault::MissingField));
        assert!(m.contains("missing field `f`"), "{m}");
        let (f, m) = fault_of(parse_lot(
            r#"{"files":[{"f":1,"file":"a.md","summaries":[],"concepts":[]}]}"#,
            &two_files(),
        ));
        assert_eq!(f, Some(FormatFault::Shape));
        assert!(m.contains("duplicate field `f`"), "{m}");
    }

    #[test]
    fn req237_ac5_partial_translates_numbers() {
        let text = r#"{"summaries":[{"n":2,"summary":"Rôle."},{"n":9,"summary":"x"},{"summary":"sans cible"},{"n":1,"summary":"Perdu."}]}"#;
        let e = Enrichment::parse_partial(text, "A.java", &list_a(), &known_a());
        assert_eq!(e.summaries, vec![("method:A.m".into(), "Rôle.".into())]);
        assert_eq!(e.rejected, vec!["résumé pour le numéro 9 (hors liste)"]);
        let t = r#"{"files":[{"f":2,"summaries":[{"n":3,"summary":"Rôle."}],"concepts":[]},{"f":1,"summaries":[{"n":1,"summary":"x"}"#;
        let l = parse_lot_partial(t, &two_files());
        assert_eq!(
            l.files[1].1.as_ref().unwrap().summaries,
            vec![("type:p.B".into(), "Rôle.".into())]
        );
        let t = r#"{"files":[{"f":1,"summaries":[{"n":1,"summary":"Doc."}],"concepts":[]},{"summaries":[{"summary":"x"}],"concepts":[]}]}"#;
        let l = parse_lot_partial(t, &two_files());
        assert_eq!(
            l.files[0].1.as_ref().unwrap().summaries,
            vec![("document:a.md".into(), "Doc.".into())]
        );
        assert!(l.files[1].1.is_err());
    }

    #[test]
    fn req238_ac4_out_of_list_number_reported_once() {
        let r = |n: i64| format!("résumé pour le numéro {n} (hors liste)");
        let e = numbered(&body(r#"{"n":0,"summary":"x"}"#)).unwrap();
        assert_eq!(e.rejected, vec![r(0)]);
        let e = numbered(&body(r#"{"n":-1,"summary":"x"}"#)).unwrap();
        assert_eq!(e.rejected, vec![r(-1)]);
        let e = numbered(&body(r#"{"n":3,"summary":"x"},{"n":3,"summary":"y"}"#)).unwrap();
        assert_eq!(e.rejected, vec![r(3)]);
    }
}
