//! Fichiers SQL : tables, vues et clés étrangères, lus par un lecteur lexical dédié, indépendant
//! du dialecte (REQ-030, DEC-035 A). Sur le modèle de `gradle_lex.rs` : aucune grammaire, aucune
//! exécution de SQL.

use std::collections::HashSet;
use std::path::Path;

use super::data::{self, Format};
use super::{Declared, FileStructure, Reference};
use crate::model::{node_id, EdgeKind, NodeKind, SubKind};

/// Jeton SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// Mot non délimité, en minuscules.
    Word(String),
    /// Identifiant délimité (`"…"`, `` `…` ``, `[…]`), délimiteurs retirés, en minuscules.
    Name(String),
    /// Ponctuation parmi `( ) , .`.
    Punct(char),
    /// Fin d'instruction : `;` ou ligne réduite à `GO`.
    Sep,
    /// Tout autre caractère.
    Other,
}

#[derive(Debug, Default)]
struct Lexed {
    tokens: Vec<Tok>,
    /// Description du premier problème lexical (chaîne, commentaire ou bloc non fermé).
    problem: Option<String>,
}

fn is_word_start(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '@')
}

/// Découpe `src` en jetons ; en cas d'erreur lexicale, le flux s'arrête là.
fn lex(src: &str) -> Lexed {
    let chars: Vec<char> = src.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut out = Lexed::default();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '-' if at(i + 1) == Some('-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if at(i + 1) == Some('*') => {
                i += 2;
                loop {
                    match at(i) {
                        None => {
                            out.problem = Some("commentaire /* */ non fermé".into());
                            return out;
                        }
                        Some('*') if at(i + 1) == Some('/') => {
                            i += 2;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            '\'' => {
                i += 1;
                loop {
                    match at(i) {
                        None => {
                            out.problem = Some("chaîne non fermée".into());
                            return out;
                        }
                        Some('\'') if at(i + 1) == Some('\'') => i += 2,
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            '"' | '`' => {
                i += 1;
                let mut name = String::new();
                loop {
                    match at(i) {
                        None => {
                            out.problem = Some("identifiant délimité non fermé".into());
                            return out;
                        }
                        Some(q) if q == c && at(i + 1) == Some(c) => {
                            name.push(c);
                            i += 2;
                        }
                        Some(q) if q == c => {
                            i += 1;
                            break;
                        }
                        Some(ch) => {
                            name.push(ch);
                            i += 1;
                        }
                    }
                }
                out.tokens.push(Tok::Name(name.to_lowercase()));
            }
            '[' => {
                // `[nom]` sur une seule ligne ; sinon crochet ordinaire (`int[]`).
                let mut j = i + 1;
                while j < chars.len() && chars[j] != ']' && chars[j] != '\n' {
                    j += 1;
                }
                if at(j) == Some(']') && j > i + 1 {
                    let name: String = chars[i + 1..j].iter().collect();
                    out.tokens.push(Tok::Name(name.to_lowercase()));
                    i = j + 1;
                } else {
                    out.tokens.push(Tok::Other);
                    i += 1;
                }
            }
            '$' if dollar_tag(&chars, i).is_some() => {
                let tag = dollar_tag(&chars, i).unwrap_or_default();
                let delim: Vec<char> = format!("${tag}$").chars().collect();
                let mut j = i + delim.len();
                loop {
                    if j + delim.len() > chars.len() {
                        out.problem = Some(format!("bloc ${tag}$ non fermé"));
                        return out;
                    }
                    if chars[j..j + delim.len()] == delim[..] {
                        i = j + delim.len();
                        break;
                    }
                    j += 1;
                }
            }
            '(' | ')' | ',' | '.' => {
                out.tokens.push(Tok::Punct(c));
                i += 1;
            }
            ';' => {
                out.tokens.push(Tok::Sep);
                i += 1;
            }
            c if is_word_start(c) => {
                let start = i;
                while at(i).is_some_and(is_word_char) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let word = word.to_lowercase();
                if word == "go" && alone_on_line(&chars, start, i) {
                    out.tokens.push(Tok::Sep);
                } else {
                    out.tokens.push(Tok::Word(word));
                }
            }
            _ => {
                out.tokens.push(Tok::Other);
                i += 1;
            }
        }
    }
    out
}

/// Étiquette du bloc `$tag$` qui commence en `i` (`""` pour `$$`), s'il y en a un.
fn dollar_tag(chars: &[char], i: usize) -> Option<String> {
    let mut j = i + 1;
    if chars.get(j) == Some(&'$') {
        return Some(String::new());
    }
    let first = *chars.get(j)?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    let mut tag = String::new();
    while let Some(&ch) = chars.get(j) {
        if ch == '$' {
            return Some(tag);
        }
        if !(ch.is_alphanumeric() || ch == '_') {
            return None;
        }
        tag.push(ch);
        j += 1;
    }
    None
}

/// Le mot `chars[start..end]` est seul sur sa ligne (blancs autour).
fn alone_on_line(chars: &[char], start: usize, end: usize) -> bool {
    let before = chars[..start]
        .iter()
        .rev()
        .take_while(|&&c| c != '\n')
        .all(|c| c.is_whitespace());
    let after = chars[end..]
        .iter()
        .take_while(|&&c| c != '\n')
        .all(|c| c.is_whitespace());
    before && after
}

fn is_word(t: Option<&Tok>, kw: &str) -> bool {
    matches!(t, Some(Tok::Word(w)) if w == kw)
}

fn segment(t: Option<&Tok>) -> Option<&str> {
    match t {
        Some(Tok::Word(w)) | Some(Tok::Name(w)) => Some(w),
        _ => None,
    }
}

/// Nom composé `a.b.c` à partir de `i` : le nom et l'indice du jeton suivant.
fn read_name(toks: &[Tok], mut i: usize) -> Option<(String, usize)> {
    let mut name = segment(toks.get(i))?.to_string();
    i += 1;
    while toks.get(i) == Some(&Tok::Punct('.')) {
        match segment(toks.get(i + 1)) {
            Some(seg) => {
                name.push('.');
                name.push_str(seg);
                i += 2;
            }
            None => break,
        }
    }
    Some((name, i))
}

const MODIFIERS: [&str; 11] = [
    "temp",
    "temporary",
    "global",
    "local",
    "unlogged",
    "external",
    "transient",
    "virtual",
    "foreign",
    "materialized",
    "recursive",
];

/// Fin de liste `FROM` : mots qui ouvrent une autre clause.
const LIST_END: [&str; 17] = [
    "where",
    "group",
    "order",
    "having",
    "union",
    "intersect",
    "except",
    "limit",
    "offset",
    "window",
    "fetch",
    "for",
    "join",
    "inner",
    "left",
    "right",
    "full",
];

const LIST_END_MORE: [&str; 4] = ["cross", "natural", "returning", "qualify"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Table,
    View,
}

/// Déclaration reconnue au début d'une instruction : genre, nom et indice du jeton suivant.
fn declaration(toks: &[Tok]) -> Option<(Kind, String, usize)> {
    let mut i;
    if is_word(toks.first(), "create") {
        i = 1;
        loop {
            if is_word(toks.get(i), "or") && is_word(toks.get(i + 1), "replace") {
                i += 2;
            } else if matches!(toks.get(i), Some(Tok::Word(w)) if MODIFIERS.contains(&w.as_str())) {
                i += 1;
            } else {
                break;
            }
        }
        let kind = if is_word(toks.get(i), "table") {
            Kind::Table
        } else if is_word(toks.get(i), "view") {
            Kind::View
        } else {
            return None;
        };
        i += 1;
        if is_word(toks.get(i), "if")
            && is_word(toks.get(i + 1), "not")
            && is_word(toks.get(i + 2), "exists")
        {
            i += 3;
        }
        let (name, next) = read_name(toks, i)?;
        return Some((kind, name, next));
    }
    if is_word(toks.first(), "alter") && is_word(toks.get(1), "table") {
        i = 2;
        if is_word(toks.get(i), "if") && is_word(toks.get(i + 1), "exists") {
            i += 2;
        }
        if is_word(toks.get(i), "only") {
            i += 1;
        }
        let (name, next) = read_name(toks, i)?;
        return Some((Kind::Table, name, next));
    }
    None
}

/// `REFERENCES nom` dans le corps d'une table ; rien pour `CREATE TABLE … AS SELECT`.
fn table_references(toks: &[Tok], from: usize, create: bool) -> Vec<String> {
    let mut depth = 0i32;
    let mut out = Vec::new();
    for (k, t) in toks.iter().enumerate().skip(from) {
        match t {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            Tok::Word(w) if create && depth <= 0 && w == "as" => return Vec::new(),
            Tok::Word(w) if w == "references" => {
                if let Some((name, _)) = read_name(toks, k + 1) {
                    out.push(name);
                }
            }
            _ => {}
        }
    }
    out
}

/// Noms de CTE de l'instruction : `WITH [RECURSIVE] n [(cols)] AS (` et `, m AS (`.
fn cte_names(toks: &[Tok]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 1..toks.len() {
        let prev_ok = matches!(&toks[i - 1], Tok::Punct(','))
            || is_word(toks.get(i - 1), "with")
            || is_word(toks.get(i - 1), "recursive");
        let Some(name) = segment(toks.get(i)) else {
            continue;
        };
        if !prev_ok {
            continue;
        }
        let mut j = i + 1;
        if toks.get(j) == Some(&Tok::Punct('(')) {
            let mut depth = 0;
            while let Some(t) = toks.get(j) {
                match t {
                    Tok::Punct('(') => depth += 1,
                    Tok::Punct(')') => {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        if !is_word(toks.get(j), "as") {
            continue;
        }
        j += 1;
        if is_word(toks.get(j), "not") {
            j += 1;
        }
        if is_word(toks.get(j), "materialized") {
            j += 1;
        }
        if toks.get(j) == Some(&Tok::Punct('(')) {
            names.insert(name.to_string());
        }
    }
    names
}

/// Noms de tables lus par une vue : éléments des listes qui suivent `FROM` et `JOIN`.
fn view_references(toks: &[Tok], from: usize) -> Vec<String> {
    let ctes = cte_names(toks);
    let mut out = Vec::new();
    // Mot qui précède chaque parenthèse ouverte (`EXTRACT(… FROM x)` n'est pas une lecture de table).
    let mut parens: Vec<bool> = Vec::new();
    let mut i = from;
    while i < toks.len() {
        match &toks[i] {
            Tok::Punct('(') => {
                let special = matches!(
                    i.checked_sub(1).and_then(|p| toks.get(p)),
                    Some(Tok::Word(w)) if ["extract", "substring", "trim", "overlay", "position"].contains(&w.as_str())
                );
                parens.push(special);
            }
            Tok::Punct(')') => {
                parens.pop();
            }
            Tok::Word(w)
                if (w == "from" || w == "join")
                    && !parens.last().copied().unwrap_or(false)
                    && !(w == "from" && is_word(toks.get(i.wrapping_sub(1)), "distinct")) =>
            {
                read_list(toks, i + 1, &ctes, &mut out);
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Éléments d'une liste de tables à partir de `i` ; les sous-requêtes sont parcourues par l'appelant.
fn read_list(toks: &[Tok], mut i: usize, ctes: &HashSet<String>, out: &mut Vec<String>) {
    loop {
        while is_word(toks.get(i), "lateral") || is_word(toks.get(i), "only") {
            i += 1;
        }
        if let Some((name, next)) = read_name(toks, i) {
            let is_function = toks.get(next) == Some(&Tok::Punct('('));
            let is_cte = !name.contains('.') && ctes.contains(&name);
            if !is_function && !is_cte {
                out.push(name);
            }
            i = next;
        }
        // Alias, AS, ON … jusqu'à la virgule ou au mot-clé suivant, au même niveau.
        let mut depth = 0i32;
        loop {
            match toks.get(i) {
                None | Some(Tok::Sep) => return,
                Some(Tok::Punct('(')) => depth += 1,
                Some(Tok::Punct(')')) => {
                    depth -= 1;
                    if depth < 0 {
                        return;
                    }
                }
                Some(Tok::Punct(',')) if depth == 0 => {
                    i += 1;
                    break;
                }
                Some(Tok::Word(w))
                    if depth == 0
                        && (LIST_END.contains(&w.as_str())
                            || LIST_END_MORE.contains(&w.as_str())) =>
                {
                    return;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Extrait la structure d'un fichier SQL.
pub fn extract(path: &Path, text: &str) -> FileStructure {
    let lexed = lex(text);
    let mut declares = vec![data::document(path, Format::Sql)];
    let mut references: Vec<Reference> = Vec::new();
    let mut seen_nodes: HashSet<String> = HashSet::new();
    let mut seen_refs: HashSet<(String, String)> = HashSet::new();
    let mut warnings = Vec::new();

    for stmt in lexed.tokens.split(|t| *t == Tok::Sep) {
        let Some((kind, name, next)) = declaration(stmt) else {
            continue;
        };
        let id = node_id(NodeKind::Table, &name);
        if seen_nodes.insert(id.clone()) {
            declares.push(Declared {
                kind: NodeKind::Table,
                short: Some(name.rsplit('.').next().unwrap_or(&name).to_string()),
                qualified: name.clone(),
                subkind: Some(match kind {
                    Kind::Table => SubKind::Table,
                    Kind::View => SubKind::View,
                }),
                summary: None,
            });
        }
        let targets = match kind {
            Kind::Table => table_references(stmt, next, is_word(stmt.first(), "create")),
            Kind::View => view_references(stmt, next),
        };
        for target in targets {
            if seen_refs.insert((id.clone(), target.clone())) {
                references.push(Reference {
                    source: id.clone(),
                    kind: EdgeKind::References,
                    target,
                });
            }
        }
    }

    let mut problems: Vec<String> = lexed.problem.into_iter().collect();
    let mut depth = 0i64;
    let mut negative = false;
    for t in &lexed.tokens {
        match t {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => {
                depth -= 1;
                negative |= depth < 0;
            }
            _ => {}
        }
    }
    if depth != 0 || negative {
        problems.push("parenthèses non équilibrées".into());
    }
    if !problems.is_empty() {
        warnings.push(data::warning(path, Format::Sql, &problems.join(", ")));
    }
    FileStructure {
        context: None,
        declares,
        references,
        warnings,
        ..FileStructure::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> FileStructure {
        extract(Path::new("db/t.sql"), text)
    }

    fn tables(f: &FileStructure) -> Vec<(String, String)> {
        f.declares
            .iter()
            .filter(|d| d.kind == NodeKind::Table)
            .map(|d| (d.qualified.clone(), d.subkind.unwrap().as_str().to_string()))
            .collect()
    }

    fn refs(f: &FileStructure) -> Vec<(String, String)> {
        f.references
            .iter()
            .map(|r| {
                assert_eq!(r.kind, EdgeKind::References);
                (r.source.clone(), r.target.clone())
            })
            .collect()
    }

    fn t(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    /// AC-030-1
    #[test]
    fn req030_ac1_tables_et_cle_etrangere_sans_point_virgule_final() {
        let f = run("CREATE TABLE customers (id INT PRIMARY KEY);\nCREATE TABLE IF NOT EXISTS \"Orders\" (id INT, customer_id INT REFERENCES customers(id))");
        assert_eq!(tables(&f), [t("customers", "table"), t("orders", "table")]);
        assert_eq!(refs(&f), [t("table:orders", "customers")]);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert_eq!(f.declares[0].id(), "document:db/t.sql");
        assert_eq!(f.declares[0].subkind, Some(SubKind::Sql));
    }

    #[test]
    fn nom_court_est_le_dernier_segment() {
        let f = run("CREATE TABLE shop.products (id INT);");
        assert_eq!(f.declares[1].qualified, "shop.products");
        assert_eq!(f.declares[1].short.as_deref(), Some("products"));
    }

    /// AC-030-2 (unité)
    #[test]
    fn req030_ac2_alter_table_declare_et_reference() {
        let f = run("ALTER TABLE orders ADD CONSTRAINT fk_p FOREIGN KEY (product_id) REFERENCES shop.products (id);");
        assert_eq!(tables(&f), [t("orders", "table")]);
        assert_eq!(refs(&f), [t("table:orders", "shop.products")]);
    }

    #[test]
    fn alter_table_if_exists_only() {
        let f =
            run("alter table if exists only Public.\"Orders\" add column x int references b(id);");
        assert_eq!(tables(&f), [t("public.orders", "table")]);
        assert_eq!(refs(&f), [t("table:public.orders", "b")]);
    }

    /// AC-030-3
    #[test]
    fn req030_ac3_vue_avec_cte_jointures_et_fonction() {
        let f = run("CREATE OR REPLACE VIEW order_view AS WITH recent AS (SELECT * FROM orders) SELECT * FROM recent r JOIN customers c ON r.customer_id = c.id, shop.products p, generate_series(1, 3) g");
        assert_eq!(tables(&f), [t("order_view", "view")]);
        assert_eq!(
            refs(&f),
            [
                t("table:order_view", "orders"),
                t("table:order_view", "customers"),
                t("table:order_view", "shop.products"),
            ]
        );
    }

    #[test]
    fn vue_liste_from_sous_requete_et_extract() {
        let f = run("CREATE VIEW v AS SELECT extract(year FROM d), a IS DISTINCT FROM b FROM a x, (SELECT 1 FROM b) s, c AS y WHERE z IN (SELECT q FROM d);");
        let mut targets: Vec<String> = refs(&f).into_iter().map(|r| r.1).collect();
        targets.sort();
        assert_eq!(targets, ["a", "b", "c", "d"]);
    }

    /// AC-030-4
    #[test]
    fn req030_ac4_ignores_et_dialectes() {
        let sql = "-- CREATE TABLE faux1\n/* CREATE TABLE faux2 */\nINSERT INTO t VALUES ('CREATE TABLE faux3');\nCREATE FUNCTION f() RETURNS void AS $$ BEGIN CREATE TABLE faux4 (x INT); END $$ LANGUAGE plpgsql;\nCREATE INDEX i ON t (x);\nDROP TABLE t;\nCREATE TABLE `db`.`items` (id INT);\nCREATE TABLE [dbo].[Users] (id INT)\n  GO  \nCREATE MATERIALIZED VIEW mv AS SELECT * FROM [dbo].[Users]";
        let f = run(sql);
        assert_eq!(
            tables(&f),
            [
                t("db.items", "table"),
                t("dbo.users", "table"),
                t("mv", "view")
            ]
        );
        assert_eq!(refs(&f), [t("table:mv", "dbo.users")]);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    }

    /// AC-030-5 et AC-029-5
    #[test]
    fn req030_ac5_parenthese_non_fermee() {
        let f = run("CREATE TABLE a (id INT, b_id INT REFERENCES b(id)");
        assert_eq!(tables(&f), [t("a", "table")]);
        assert_eq!(refs(&f), [t("table:a", "b")]);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("db/t.sql"));
        let f = run("CREATE TABLE a (");
        assert_eq!(tables(&f), [t("a", "table")]);
        assert_eq!(f.warnings.len(), 1);
    }

    #[test]
    fn chaine_non_fermee_retient_ce_qui_precede() {
        let f = run("CREATE TABLE a (id INT);\nINSERT INTO a VALUES ('abc");
        assert_eq!(tables(&f), [t("a", "table")]);
        assert_eq!(f.warnings.len(), 1);
        let f = run("CREATE TABLE a (id INT); /* x");
        assert_eq!(f.warnings.len(), 1);
        let f = run("CREATE TABLE a (id INT); SELECT $$ x");
        assert_eq!(f.warnings.len(), 1);
    }

    #[test]
    fn bloc_dollar_avec_etiquette_imbrique() {
        let f = run("CREATE PROCEDURE p() AS $body$ BEGIN EXECUTE $q$ CREATE TABLE faux (x INT); $q$; END $body$;\nCREATE TABLE ok (x INT);");
        assert_eq!(tables(&f), [t("ok", "table")]);
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn modificateurs() {
        let f = run("CREATE GLOBAL TEMPORARY TABLE g (x INT);\nCREATE OR REPLACE RECURSIVE VIEW r AS SELECT 1;\nCREATE UNLOGGED TABLE u (x INT);\nCREATE UNIQUE INDEX i ON g (x);");
        assert_eq!(
            tables(&f),
            [t("g", "table"), t("r", "view"), t("u", "table")]
        );
    }

    #[test]
    fn create_table_as_select_sans_reference_et_auto_reference_gardee() {
        let f = run("CREATE TABLE c AS SELECT * FROM orders;\nCREATE TABLE tree (id INT, parent INT REFERENCES tree(id));");
        assert_eq!(refs(&f), [t("table:tree", "tree")]);
        assert_eq!(tables(&f).len(), 2);
    }

    #[test]
    fn declaration_en_double_dans_le_fichier() {
        let f = run("CREATE TABLE a (x INT); ALTER TABLE a ADD COLUMN y INT;");
        assert_eq!(tables(&f), [t("a", "table")]);
    }

    #[test]
    fn fichier_vide_ou_sans_declaration() {
        let f = run("");
        assert_eq!(f.declares.len(), 1);
        assert!(f.warnings.is_empty());
        let f = run("SELECT 1;\n-- fin");
        assert_eq!(f.declares.len(), 1);
        assert!(f.warnings.is_empty());
    }
}
