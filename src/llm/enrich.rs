//! Prompt d'enrichissement et schéma JSON version 2 (DEC-019, REQ-024).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::filter::FileType;
use crate::llm::tidy;

/// Version du schéma de réponse d'enrichissement.
pub const ENRICH_SCHEMA_VERSION: u32 = 2;

/// Version du prompt d'enrichissement (DEC-062). À incrémenter à chaque changement des consignes
/// système, des gabarits de requête (fichier seul et lot) ou des schémas de réponse version 2 et
/// 3 : tout enrichissement conservé avec une autre version est refait. Un test fige l'empreinte
/// BLAKE3 de ces textes à côté de cette valeur. 1 de la 0.18.0 à la 0.25.x ; 7 depuis la 0.26.0
/// (consignes de forme des résumés, REQ-167 ; extraits, REQ-169 ; genres et types de fichiers,
/// REQ-171 ; langue, REQ-172 ; concepts, REQ-175 ; notes de correction des nouveaux essais,
/// REQ-177 ; les valeurs 2 à 6 n'ont pas été livrées).
pub const ENRICH_PROMPT_VERSION: u32 = 7;

/// Consignes de forme des résumés : ligne commune aux deux prompts système (DEC-135).
pub const SUMMARY_RULES: &str = "Résumés (de nœud ou de concept) : une phrase de 25 mots au plus, qui dit le rôle ou la responsabilité de l'élément (ce qu'il fait et à quoi il sert), pas sa syntaxe ni sa signature ; ne répète pas son nom ; n'écris que ce que le contenu montre, sans rien supposer ni inventer. Omets (aucune entrée dans \"summaries\") un accesseur ou un mutateur trivial (il lit ou écrit un champ, sans autre effet) et une section vide ou réduite à son titre.";

/// Consignes d'extrait : ligne du prompt système du fichier envoyé seul, après `SUMMARY_RULES`
/// (REQ-169, DEC-136).
pub const EXCERPT_RULES: &str = "Un fichier trop long est envoyé en extraits : sa ligne \"Fichier :\" porte alors \"(extrait k/n, lignes a–b)\" et tu ne vois que ces lignes. À partir du deuxième extrait, le contenu commence par une copie partielle du début du fichier (package, imports, déclaration du type englobant, ou titres des sections englobantes), donnée pour contexte : n'en tire aucun résumé. Un nœud de la liste seulement cité dans un extrait se résume d'après ce que l'extrait en montre ; omets-le si l'extrait n'en montre pas le rôle.";

/// Angle du résumé selon le genre du nœud : ligne commune aux deux prompts système (DEC-137).
pub const KIND_RULES: &str = "Angle du résumé selon le genre du nœud, donné par le préfixe de son identifiant : module → ce que le module produit et à quoi lui servent ses dépendances ; type → sa responsabilité ; method → son effet et ce qu'elle renvoie ; table → son rôle métier et ses colonnes clés ; document → le sujet du document, ou ce que décrit ou configure un fichier de données (SQL, XML, JSON, YAML) ; section → le sujet qu'elle traite.";

/// Famille d'un type de fichier dans la phrase des types du prompt (REQ-171).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Code,
    Build,
    Data,
    Doc,
}

/// Famille et libellé d'un type de fichier dans le prompt ; `match` exhaustif (REQ-171).
fn file_type_label(t: FileType) -> (Family, &'static str) {
    match t {
        FileType::Java => (Family::Code, "Java"),
        FileType::Kotlin => (Family::Code, "Kotlin"),
        FileType::Groovy => (Family::Code, "Groovy"),
        FileType::Scala => (Family::Code, "Scala"),
        FileType::JavaScript => (Family::Code, "JavaScript"),
        FileType::TypeScript => (Family::Code, "TypeScript"),
        FileType::Maven => (Family::Build, "Maven"),
        FileType::Ivy => (Family::Build, "Ivy"),
        FileType::Gradle => (Family::Build, "Gradle"),
        FileType::Npm => (Family::Build, "npm"),
        FileType::Sql => (Family::Data, "SQL"),
        FileType::Xml => (Family::Data, "XML"),
        FileType::Json => (Family::Data, "JSON"),
        FileType::Yaml => (Family::Data, "YAML"),
        FileType::Markdown => (Family::Doc, "Markdown"),
    }
}

/// Types de fichiers de la première phrase des prompts système, tirés de `FileType::ALL`
/// (REQ-171) : familles `code`, `build`, `données`, puis les types sans famille nommée, jointes
/// par `, ` avec `ou ` devant la dernière.
pub fn file_types_phrase() -> String {
    let groups: Vec<String> = [
        (Family::Code, "code "),
        (Family::Build, "build "),
        (Family::Data, "données "),
        (Family::Doc, ""),
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
    version: u32,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSummary {
    node: String,
    summary: String,
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
/// (REQ-178), premier résumé d'un nœud gardé et concepts de même nom fusionnés (REQ-179), noms
/// de concepts normalisés (REQ-180) ; DEC-144.
fn filter_raw(
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
    file: &str,
    node_ids: &[String],
) -> Enrichment {
    let known: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
    let mut out = Enrichment::default();
    let mut kept: HashMap<String, usize> = HashMap::new();

    for s in summaries {
        if s.node.trim().is_empty() {
            out.rejected.push("résumé sans identifiant de nœud".into());
            continue;
        }
        if !known.contains(s.node.as_str()) {
            out.rejected
                .push(format!("résumé pour {} (nœud hors liste)", s.node));
            continue;
        }
        let Some((folded, received, text)) = fold_and_cap(&s.summary) else {
            continue;
        };
        if tidy::is_meaningless(&folded, tidy::node_name(&s.node)) {
            out.rejected.push(node_empty_of_sense(&s.node));
            continue;
        }
        if let Some(&i) = kept.get(&s.node) {
            if out.summaries[i].1 != text {
                out.rejected.push(node_duplicate(&s.node));
            }
            continue;
        }
        if let Some(n) = received {
            out.rejected.push(node_capped(&s.node, n));
        }
        kept.insert(s.node.clone(), out.summaries.len());
        out.summaries.push((s.node, text));
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
        let raw: RawEnrichment = serde_json::from_str(json).map_err(invalid)?;
        if raw.version != ENRICH_SCHEMA_VERSION {
            return Err(invalid(format!(
                "version {} non prise en charge (attendu : {ENRICH_SCHEMA_VERSION})",
                raw.version
            )));
        }
        Ok(filter_raw(raw.summaries, raw.concepts, file, node_ids))
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

/// Prompt système d'enrichissement (identique pour tous les fournisseurs), langue demandée
/// `language`.
pub fn enrichment_system_prompt(language: &str) -> String {
    let types = file_types_phrase();
    let rules = language_rules(language);
    format!(
        "Tu enrichis le graphe d'un dépôt à partir d'un fichier ({types}) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Schéma (version {ENRICH_SCHEMA_VERSION}) : {{\"version\": {ENRICH_SCHEMA_VERSION}, \
         \"summaries\": [{{\"node\": \"<identifiant de la liste>\", \"summary\": \"<25 mots au plus>\"}}], \
         \"concepts\": [{{\"name\": \"nom du concept\", \"summary\": \"<25 mots au plus> (optionnel)\", \
         \"sections\": [\"<identifiant de section de la liste>\"]}}]}}.\n\
         {SUMMARY_RULES}\n\
         {EXCERPT_RULES}\n\
         {KIND_RULES}\n\
         {rules}\n\
         {CONCEPT_RULES}\n\
         Les résumés ne portent que sur les nœuds de la liste fournie. Les concepts ne concernent \
         que les fichiers Markdown et ne citent que des sections de la liste. Tu ne crées, ne \
         renomme ni ne supprimes aucun nœud et tu ne produis aucun autre lien. \
         Sans rien à signaler, réponds {{\"version\": {ENRICH_SCHEMA_VERSION}, \"summaries\": [], \"concepts\": []}}."
    )
}

/// Message utilisateur d'enrichissement : liste des nœuds puis contenu du morceau.
pub fn enrichment_user_prompt(
    file: &str,
    excerpt: Option<Excerpt>,
    text: &str,
    requested: &[String],
) -> String {
    let list: String = requested.iter().map(|n| format!("- {n}\n")).collect();
    let head = match excerpt {
        None => format!("Fichier : {file}"),
        Some(Excerpt {
            rank,
            total,
            first_line,
            last_line,
        }) => format!("Fichier : {file} (extrait {rank}/{total}, lignes {first_line}–{last_line})"),
    };
    format!("{head}\n\nNœuds du fichier :\n{list}\nContenu :\n{text}")
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
    version: u32,
    files: Vec<RawLotEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLotEntry {
    file: String,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
}

/// Analyse strictement la réponse version 3 d'un lot puis filtre chaque entrée selon les nœuds
/// du fichier qu'elle nomme. Une réponse illisible ou d'une autre version rend tout le lot
/// non conforme (`Err`) ; un fichier du lot sans entrée échoue seul.
pub fn parse_lot(json: &str, items: &[LotItem]) -> Result<LotEnrichment> {
    let raw: RawLot = serde_json::from_str(json).map_err(invalid)?;
    if raw.version != ENRICH_LOT_SCHEMA_VERSION {
        return Err(invalid(format!(
            "version {} non prise en charge pour un lot (attendu : {ENRICH_LOT_SCHEMA_VERSION})",
            raw.version
        )));
    }
    let mut rejected = Vec::new();
    let mut found: Vec<Option<Enrichment>> = vec![None; items.len()];
    for entry in raw.files {
        let Some(i) = items.iter().position(|it| it.file == entry.file) else {
            rejected.push(format!("entrée pour {} (fichier hors du lot)", entry.file));
            continue;
        };
        if found[i].is_some() {
            rejected.push(format!("entrée en double pour {}", entry.file));
            continue;
        }
        found[i] = Some(filter_raw(
            entry.summaries,
            entry.concepts,
            &entry.file,
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
    Ok(LotEnrichment { files, rejected })
}

/// Prompt système d'un lot de fichiers (identique pour tous les fournisseurs), langue demandée
/// `language`.
pub fn lot_system_prompt(language: &str) -> String {
    let types = file_types_phrase();
    let rules = language_rules(language);
    format!(
        "Tu enrichis le graphe d'un dépôt à partir de plusieurs fichiers ({types}) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Chaque fichier est introduit par une ligne `=== FICHIER : <chemin> ===`, suivie de la liste de ses \
         nœuds puis de son contenu, jusqu'à la ligne `=== FIN DU FICHIER : <chemin> ===`.\n\
         Schéma (version {ENRICH_LOT_SCHEMA_VERSION}) : {{\"version\": {ENRICH_LOT_SCHEMA_VERSION}, \"files\": \
         [{{\"file\": \"<chemin exact du fichier>\", \"summaries\": [{{\"node\": \"<identifiant de la liste de ce fichier>\", \
         \"summary\": \"<25 mots au plus>\"}}], \"concepts\": [{{\"name\": \"nom du concept\", \"summary\": \"<25 mots au plus> (optionnel)\", \
         \"sections\": [\"<identifiant de section de la liste de ce fichier>\"]}}]}}]}}.\n\
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
    for it in items {
        let list: String = it.node_ids.iter().map(|n| format!("- {n}\n")).collect();
        out.push_str(&format!(
            "=== FICHIER : {f} ===\nNœuds du fichier :\n{list}\nContenu :\n{t}\n=== FIN DU FICHIER : {f} ===\n\n",
            f = it.file,
            t = it.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_SUMMARY_LANGUAGE;

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
    fn fingerprint_of(texts: &[String]) -> String {
        blake3::hash(texts.join("\n<<<->>>\n").as_bytes())
            .to_hex()
            .to_string()
    }

    /// Les cinq textes de la 0.26.0 reconstruits depuis les textes actuels (REQ-169) : sans
    /// `EXCERPT_RULES`, requête `None` réécrite en `(morceau 2)`. `f` transforme les prompts
    /// système.
    fn texts_0_26(f: impl Fn(String) -> String) -> Vec<String> {
        let user =
            user_prompt(None).replacen("Fichier : a.md\n", "Fichier : a.md (morceau 2)\n", 1);
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            f(
                texts_v3_system(enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE))
                    .replace(&format!("{EXCERPT_RULES}\n"), ""),
            ),
            user,
            f(texts_v3_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE))),
            lot_user_prompt(&lot_items()),
        ]
    }

    const OLD_TYPES: &str =
        "code Java/Kotlin/Groovy/Scala/JavaScript/TypeScript, build Maven/Gradle/Ivy, ou Markdown";

    /// Prompt système de la version 5 : sans les règles des concepts (REQ-175).
    fn texts_v5_system(p: String) -> String {
        p.replace(&format!("{CONCEPT_RULES}\n"), "")
    }

    /// Les six textes figés par la version 5 (AC-175-2).
    fn texts_v5() -> Vec<String> {
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            texts_v5_system(enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            user_prompt(None),
            user_prompt(Some(EXCERPT_2_3)),
            texts_v5_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            lot_user_prompt(&lot_items()),
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
            user_prompt(None),
            user_prompt(Some(EXCERPT_2_3)),
            texts_v4_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            lot_user_prompt(&lot_items()),
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
            user_prompt(None),
            user_prompt(Some(EXCERPT_2_3)),
            texts_v3_system(lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE)),
            lot_user_prompt(&lot_items()),
        ]
    }

    /// Les six textes de la version 6 (inchangés par la version 7).
    fn texts_v6() -> Vec<String> {
        vec![
            format!("{ENRICH_SCHEMA_VERSION}/{ENRICH_LOT_SCHEMA_VERSION}"),
            enrichment_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
            user_prompt(None),
            user_prompt(Some(EXCERPT_2_3)),
            lot_system_prompt(DEFAULT_SUMMARY_LANGUAGE),
            lot_user_prompt(&lot_items()),
        ]
    }

    #[test]
    fn req049_ac4_prompt_texts_are_frozen_next_to_the_prompt_version() {
        // Changer une consigne, un gabarit, un schéma ou une note : incrémenter
        // `ENRICH_PROMPT_VERSION`, puis mettre à jour le littéral ci-dessous.
        assert_eq!(ENRICH_PROMPT_VERSION, 7);
        let mut texts = texts_v6();
        texts.push(
            crate::llm::FormatFault::ALL
                .map(crate::llm::FormatFault::note)
                .join("\n"),
        );
        assert_eq!(
            fingerprint_of(&texts),
            "772f5652e95481e85eea6478ea9ce8b1736b5c63ca4facdf633a76e6ac678ff4",
            "les textes du prompt ont changé : incrémentez ENRICH_PROMPT_VERSION"
        );
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
    fn req169_ac3_the_five_texts_of_0_26_0_are_unchanged() {
        assert_eq!(
            fingerprint_of(&texts_0_26(|t| t)),
            "a15bf9ef481fcf8b4d4b23cd4c073a7c341de03e0642b46c77c7c4cb0bd06f36"
        );
    }

    #[test]
    fn req169_ac1_excerpt_rules_text_and_placement() {
        assert_eq!(EXCERPT_RULES, EXTRAIT);
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
            none,
            "Fichier : a.md\n\nNœuds du fichier :\n- document:a.md\n- section:a.md#x\n\nContenu :\ntexte a"
        );
        assert_eq!(
            enrichment_user_prompt("a.md", None, "t", &ids(&["document:a.md"])),
            "Fichier : a.md\n\nNœuds du fichier :\n- document:a.md\n\nContenu :\nt"
        );
        let some = user_prompt(Some(EXCERPT_2_3));
        let (first, rest) = some.split_once('\n').unwrap();
        assert_eq!(first, "Fichier : a.md (extrait 2/3, lignes 10–20)");
        assert_eq!(rest, none.split_once('\n').unwrap().1);
        assert!(!none.contains("morceau") && !some.contains("morceau"));
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
        let e = one_summary("A.java", &["type:A"], "type:A", "\n \t");
        assert!(e.summaries.is_empty() && e.rejected.is_empty());
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
        for text in ["é".repeat(400), "  a   b  ".to_string(), " \n ".to_string()] {
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
}
