//! Règles pures de forme des résumés et des noms de concepts (REQ-178, REQ-180, DEC-144).
//!
//! Résumé : repli des blancs (`fold`), plafond de 400 caractères (`cap`), résumé vide de sens
//! (`is_meaningless` : mots absents, formule creuse, ou simple rappel du nom de l'élément).
//! Les appelants (`filter_raw`) viennent avec T-267.
#![allow(dead_code)] // branché sur `filter_raw` par T-267

/// Plafond d'un résumé, en caractères (V-006).
pub const SUMMARY_MAX_CHARS: usize = 400;

/// Formules creuses (règle (b) de REQ-178), sur les mots joints par une espace.
const FILLERS: [&str; 20] = [
    "n a",
    "na",
    "none",
    "null",
    "nil",
    "undefined",
    "todo",
    "tbd",
    "unknown",
    "not applicable",
    "no summary",
    "aucun",
    "aucune",
    "néant",
    "rien",
    "inconnu",
    "non applicable",
    "sans objet",
    "pas de résumé",
    "aucun résumé",
];

/// Mots de genre (règle (c) de REQ-178).
const KIND_WORDS: [&str; 36] = [
    "classe",
    "class",
    "interface",
    "énumération",
    "enum",
    "enregistrement",
    "record",
    "annotation",
    "objet",
    "object",
    "trait",
    "alias",
    "type",
    "méthode",
    "method",
    "fonction",
    "function",
    "constructeur",
    "constructor",
    "champ",
    "field",
    "propriété",
    "property",
    "module",
    "package",
    "paquet",
    "document",
    "fichier",
    "file",
    "section",
    "table",
    "vue",
    "view",
    "clé",
    "key",
    "concept",
];

/// Toute suite de blancs devient une espace, blancs de tête et de fin retirés.
pub fn fold(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `None` si le résumé (déjà replié) tient en 400 caractères, sinon le texte coupé : plus long
/// préfixe de mots entiers d'au plus 399 caractères (ou les 399 premiers si le premier mot les
/// dépasse), puis `…`.
pub fn cap(s: &str) -> Option<String> {
    if s.chars().count() <= SUMMARY_MAX_CHARS {
        return None;
    }
    let limit = SUMMARY_MAX_CHARS - 1;
    let mut end = 0; // longueur en octets du préfixe retenu
    let mut len = 0; // longueur en caractères du préfixe retenu
    let mut pos = 0; // octet courant
    let mut count = 0; // caractères avant `pos`
    for word in s.split(' ') {
        let w = word.chars().count();
        let total = if count == 0 { w } else { count + w };
        if total > limit {
            break;
        }
        end = pos + word.len();
        len = total;
        pos = end + 1;
        count = total + 1;
    }
    let kept = if len == 0 {
        let cut = s.char_indices().nth(limit).map_or(s.len(), |(i, _)| i);
        &s[..cut]
    } else {
        &s[..end]
    };
    Some(format!("{kept}…"))
}

/// Suites maximales de caractères alphanumériques, en minuscules.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Nom court d'un identifiant de nœud : après le premier `:`, avant le premier `(`, dernier
/// segment après `.` `/` `#` `$` `:`.
pub fn node_name(id: &str) -> &str {
    let rest = id.split_once(':').map_or(id, |(_, r)| r);
    let rest = rest.split('(').next().unwrap_or(rest);
    rest.rsplit(['.', '/', '#', '$', ':'])
        .next()
        .unwrap_or(rest)
}

/// Vrai si le résumé (déjà replié) ne dit rien : sans mot, formule creuse, ou simple nom de
/// l'élément éventuellement précédé d'un mot de genre.
pub fn is_meaningless(summary: &str, name: &str) -> bool {
    let sw = words(summary);
    if sw.is_empty() {
        return true;
    }
    let joined = sw.join(" ");
    if FILLERS.contains(&joined.as_str()) {
        return true;
    }
    let nw = words(name);
    if nw.is_empty() {
        return false;
    }
    sw == nw
        || (sw.len() == nw.len() + 1 && KIND_WORDS.contains(&sw[0].as_str()) && sw[1..] == nw[..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_collapses_all_whitespace() {
        assert_eq!(fold("  a\nb\r\nc\td\u{a0}e  "), "a b c d e");
        assert_eq!(fold(" \n\t\u{a0}"), "");
    }

    #[test]
    fn cap_cuts_on_whole_words() {
        let text = (0..90)
            .map(|i| format!("mot{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(text.chars().count(), 539);
        let out = cap(&text).unwrap();
        assert!(out.chars().count() <= SUMMARY_MAX_CHARS);
        let before = out.strip_suffix('…').unwrap();
        assert!(!before.ends_with(' '));
        assert!(text.starts_with(before));
        assert_eq!(text.as_bytes()[before.len()], b' ');
        let next = text[before.len() + 1..].split(' ').next().unwrap();
        assert!(before.chars().count() + 1 + next.chars().count() + 1 > SUMMARY_MAX_CHARS);
    }

    #[test]
    fn cap_keeps_exactly_400_chars() {
        assert_eq!(cap(&"é".repeat(400)), None);
        let mixed = format!("{} {}", "é".repeat(199), "é".repeat(200));
        assert_eq!(mixed.chars().count(), 400);
        assert_eq!(cap(&mixed), None);
    }

    #[test]
    fn cap_cuts_long_first_word_on_char_boundary() {
        let out = cap(&"é".repeat(500)).unwrap();
        assert_eq!(out, format!("{}…", "é".repeat(399)));
        assert_eq!(out.chars().count(), 400);
        let out = cap(&format!("{} suite", "é".repeat(450))).unwrap();
        assert_eq!(out, format!("{}…", "é".repeat(399)));
    }

    #[test]
    fn node_name_examples() {
        assert_eq!(node_name("type:com.acme.A"), "A");
        assert_eq!(node_name("method:Big.m1"), "m1");
        assert_eq!(node_name("section:docs/g.md#intro"), "intro");
        assert_eq!(node_name("type:web/src/api/client.ApiClient"), "ApiClient");
        assert_eq!(
            node_name("method:com.acme.core.OrderService.place"),
            "place"
        );
        assert_eq!(node_name("method:A.m(int,String)"), "m");
    }

    #[test]
    fn meaningless_discarded() {
        for (s, id) in [
            ("N/A", "type:A"),
            ("n/a.", "type:A"),
            ("…", "type:A"),
            ("-", "type:A"),
            ("TODO", "type:A"),
            ("Aucun résumé.", "type:A"),
            ("Classe A.", "type:A"),
            ("A", "type:A"),
            ("Order.", "type:com.acme.Order"),
            ("class Order", "type:com.acme.Order"),
            ("Méthode m1.", "method:Big.m1"),
        ] {
            assert!(is_meaningless(&fold(s), node_name(id)), "{s} / {id}");
        }
        assert!(is_meaningless("Facturation.", "facturation"));
    }

    #[test]
    fn meaningful_kept() {
        for (s, id) in [
            ("Le A.", "type:A"),
            ("Fait A.", "type:A"),
            ("s", "type:A"),
            ("Classe A centrale.", "type:A"),
            ("Commande client.", "type:com.acme.Order"),
            ("Client.", "type:web.ApiClient"),
            ("Service.", "type:com.acme.OrderService"),
        ] {
            assert!(!is_meaningless(&fold(s), node_name(id)), "{s} / {id}");
        }
    }

    #[test]
    fn every_filler_is_meaningless() {
        for f in FILLERS {
            assert!(is_meaningless(f, "x"), "{f}");
        }
    }
}
