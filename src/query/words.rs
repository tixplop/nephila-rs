//! Normalisation des mots de la question de `query ask` (REQ-183, DEC-147) : repli des accents,
//! racine par suffixes et mots vides des mots ordinaires. Trois fonctions pures, sans état.

use std::borrow::Cow;

/// Lettre de base d'une lettre latine accentuée (blocs Latin-1 et Latin étendu A), minuscule.
fn base_letter(c: char) -> Option<char> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        _ => return None,
    })
}

/// Replie les accents d'une chaîne déjà en minuscules, caractère par caractère : les
/// diacritiques combinants U+0300 à U+036F sont retirés, les lettres de la table deviennent leur
/// lettre de base, tout autre caractère est gardé. Une chaîne ASCII est rendue empruntée.
/// Deux chaînes égales, l'une préfixe ou contenue dans l'autre, le restent après repli
/// (REQ-183, DEC-147).
pub(crate) fn fold(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        return Cow::Borrowed(s);
    }
    Cow::Owned(
        s.chars()
            .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
            .map(|c| base_letter(c).unwrap_or(c))
            .collect(),
    )
}

/// Suffixes retirés pour obtenir la racine, dans l'ordre d'essai.
const SUFFIXES: &[&str] = &[
    "encies", "ancies", "ations", "ences", "ances", "ation", "ence", "ance", "ency", "ancy",
    "ings", "ing", "ies", "ied", "es", "ed", "e", "s",
];

/// Longueur minimale (en caractères) gardée par la racine.
const MIN_STEM_CHARS: usize = 4;

/// Racine d'un mot replié : le mot privé du premier suffixe de la liste dont il se termine et
/// qui laisse au moins 4 caractères ; le `s` final n'est pas retiré après `s`, `i` ou `u`. Un seul
/// suffixe est retiré ; la racine est toujours un préfixe du mot (REQ-183, DEC-147).
pub(crate) fn stem(word: &str) -> &str {
    for suffix in SUFFIXES {
        let Some(head) = word.strip_suffix(suffix) else {
            continue;
        };
        if head.chars().count() < MIN_STEM_CHARS {
            continue;
        }
        if *suffix == "s" && head.ends_with(['s', 'i', 'u']) {
            continue;
        }
        return head;
    }
    word
}

/// Mots vides des mots ordinaires de la question (formes repliées) : liaison et relation, anglais
/// et français. Aucun mot de genre (`class`, `type`, `file`...) : ils désignent souvent un nom.
/// Un identifiant cité de cette liste reste cherché (REQ-183, DEC-147).
pub(crate) const PROSE_STOP_WORDS: &[&str] = &[
    "this",
    "that",
    "these",
    "those",
    "there",
    "their",
    "them",
    "they",
    "then",
    "than",
    "its",
    "his",
    "her",
    "our",
    "your",
    "you",
    "was",
    "were",
    "been",
    "being",
    "has",
    "have",
    "had",
    "can",
    "could",
    "should",
    "would",
    "will",
    "shall",
    "may",
    "might",
    "must",
    "did",
    "done",
    "doing",
    "all",
    "each",
    "every",
    "not",
    "but",
    "nor",
    "from",
    "into",
    "onto",
    "over",
    "under",
    "about",
    "also",
    "only",
    "very",
    "more",
    "most",
    "such",
    "same",
    "other",
    "when",
    "why",
    "whose",
    "whom",
    "while",
    "here",
    "elle",
    "elles",
    "ils",
    "nous",
    "vous",
    "leur",
    "leurs",
    "lui",
    "mon",
    "ton",
    "son",
    "mes",
    "tes",
    "ses",
    "nos",
    "vos",
    "cette",
    "ces",
    "cet",
    "aux",
    "etre",
    "ete",
    "ont",
    "etait",
    "sera",
    "peut",
    "peuvent",
    "doit",
    "faut",
    "fait",
    "font",
    "faire",
    "quand",
    "dont",
    "ainsi",
    "aussi",
    "tout",
    "tous",
    "toute",
    "toutes",
    "plus",
    "moins",
    "tres",
    "entre",
    "vers",
    "chez",
    "sans",
    "sous",
    "depuis",
    "pendant",
    "avant",
    "apres",
    "comme",
    "lequel",
    "laquelle",
    "lesquels",
    "lesquelles",
    "call",
    "calls",
    "called",
    "calling",
    "use",
    "uses",
    "used",
    "using",
    "extend",
    "extends",
    "extended",
    "implement",
    "implements",
    "implemented",
    "appelle",
    "appellent",
    "utilise",
    "utilisent",
];

#[cfg(test)]
mod tests {
    use super::super::ask::STOP_WORDS;
    use super::super::search::oracle::*;
    use super::*;

    #[test]
    fn req183_ac1_fold_table() {
        assert_eq!(fold("éèêëàâäçîïôöùûüÿñ"), "eeeeaaaciioouuuyn");
        assert_eq!(fold("ąćęłńśźż"), "acelnszz");
        assert_eq!(fold(&"école".to_lowercase()), "ecole");
        assert_eq!(fold(&"ÉCOLE".to_lowercase()), "ecole");
        assert_eq!(fold(&"İstanbul".to_lowercase()), "istanbul");
        assert_eq!(fold("e\u{301}"), "e");
        for same in ["æ", "œ", "ß", "ð", "þ", "日本語"] {
            assert_eq!(fold(same), same);
        }
        assert!(matches!(fold("plain_ascii.Name"), Cow::Borrowed(_)));
    }

    #[test]
    fn req183_ac1_fold_keeps_equality_prefix_and_containment() {
        let mut rng = Lcg(183);
        let (mut prefix, mut contained, mut equal) = (false, false, false);
        for _ in 0..200 {
            let a = phrase(&mut rng, 3).to_lowercase();
            let b = if rng.next(3) == 0 {
                a.clone()
            } else {
                phrase(&mut rng, 2).to_lowercase()
            };
            let (fa, fb) = (fold(&a), fold(&b));
            if a == b {
                equal = true;
                assert_eq!(fa, fb);
            }
            if a.starts_with(&b) {
                prefix = true;
                assert!(fa.starts_with(&*fb), "{a:?} {b:?}");
            }
            if a.contains(&b) {
                contained = true;
                assert!(fa.contains(&*fb), "{a:?} {b:?}");
            }
        }
        assert!(equal && prefix && contained);
    }

    #[test]
    fn req183_ac2_stem() {
        let cases = [
            ("annotations", "annot"),
            ("dependencies", "depend"),
            ("dependency", "depend"),
            ("dependances", "depend"),
            ("lines", "line"),
            ("types", "type"),
            ("classes", "class"),
            ("checked", "check"),
            ("writing", "writ"),
            ("properties", "propert"),
            ("builders", "builder"),
            ("create", "creat"),
            ("class", "class"),
            ("status", "status"),
            ("type", "type"),
            ("code", "code"),
            ("added", "added"),
            ("return", "return"),
            ("map", "map"),
            ("references", "refer"),
            ("implementation", "implement"),
            ("facturation", "factur"),
            ("emises", "emis"),
            ("declarees", "declare"),
            ("fichiers", "fichier"),
            ("classe", "class"),
            ("statements", "statement"),
            ("emitted", "emitt"),
            ("copied", "copi"),
            ("lin", "lin"),
            ("gere", "gere"),
            ("日本語", "日本語"),
        ];
        for (word, want) in cases {
            let got = stem(word);
            assert_eq!(got, want, "mot {word}");
            assert!(word.starts_with(got));
        }
    }

    #[test]
    fn req183_prose_stop_words_are_a_closed_folded_list() {
        let mut seen = std::collections::HashSet::new();
        for w in PROSE_STOP_WORDS {
            assert!(seen.insert(*w), "doublon {w}");
            assert!(w.chars().count() >= 3, "trop court {w}");
            assert_eq!(fold(w), *w, "non replié {w}");
            assert!(!STOP_WORDS.contains(w), "déjà dans STOP_WORDS {w}");
        }
        assert_eq!(PROSE_STOP_WORDS.len(), 136);
    }
}
