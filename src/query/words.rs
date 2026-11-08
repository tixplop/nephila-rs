//! Normalisation des mots de la question de `query ask` (REQ-183, DEC-147) : repli des accents,
//! racine par suffixes et mots vides des mots ordinaires. Trois fonctions pures, sans état.

use std::borrow::Cow;

use crate::model::EdgeKind;

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
/// Un identifiant cité de cette liste reste cherché (REQ-183, DEC-147). Les 45 derniers mots sont
/// des formes de verbe de la table de `link_kinds` qui nomment rarement un nœud (REQ-259, DEC-192).
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
    "invoke",
    "invokes",
    "invoked",
    "invoking",
    "appele",
    "appelee",
    "appeles",
    "appelees",
    "appeler",
    "appelant",
    "appelants",
    "invoque",
    "invoquent",
    "utilisee",
    "utilisees",
    "utiliser",
    "utilisant",
    "extending",
    "etend",
    "etendent",
    "inherit",
    "inherits",
    "inherited",
    "inheriting",
    "herite",
    "heritent",
    "heriter",
    "implementing",
    "implemente",
    "implementent",
    "implementee",
    "implementer",
    "referenced",
    "referencing",
    "referencent",
    "mentioned",
    "mentionne",
    "mentionnent",
    "linked",
    "contain",
    "contains",
    "contained",
    "containing",
    "contient",
    "contiennent",
];

/// Genres de lien visés par un mot de relation de la question (forme repliée exacte, sans racine),
/// vide pour tout autre mot (REQ-259, DEC-192). Une branche par famille de la table.
pub(crate) fn link_kinds(word: &str) -> &'static [EdgeKind] {
    use EdgeKind::*;
    match word {
        "call" | "calls" | "called" | "calling" | "caller" | "callers" | "invoke" | "invokes"
        | "invoked" | "invoking" | "appel" | "appels" | "appelle" | "appellent" | "appele"
        | "appelee" | "appeles" | "appelees" | "appeler" | "appelant" | "appelants" | "invoque"
        | "invoquent" => &[Calls],
        "use" | "uses" | "used" | "using" | "usage" | "utilise" | "utilisent" | "utilisee"
        | "utilisees" | "utiliser" | "utilisant" => &[Calls, References, Imports, DependsOn],
        "extend" | "extends" | "extended" | "extending" | "subclass" | "subclasses"
        | "superclass" | "superclasses" | "etend" | "etendent" => &[Extends],
        "inherit" | "inherits" | "inherited" | "inheriting" | "herite" | "heritent" | "heriter" => {
            &[Extends, Implements]
        }
        "implement" | "implements" | "implemented" | "implementing" | "implementation"
        | "implementations" | "implemente" | "implementent" | "implementee" | "implementer" => {
            &[Implements]
        }
        "import" | "imports" | "imported" | "importing" | "importe" | "importent" | "importer" => {
            &[Imports]
        }
        "depend" | "depends" | "depending" | "dependent" | "dependency" | "dependencies"
        | "dependance" | "dependances" => &[DependsOn],
        "reference" | "references" | "referenced" | "referencing" | "referencent" => &[References],
        "mention" | "mentions" | "mentioned" | "mentionne" | "mentionnent" => &[Mentions],
        "link" | "links" | "linked" | "lien" | "liens" => &[LinksTo],
        "contain" | "contains" | "contained" | "containing" | "contient" | "contiennent" => {
            &[Contains]
        }
        _ => &[],
    }
}

/// Mots d'architecture (REQ-260, DEC-193) : formes repliées, comparées mot à mot, sans racine. Ils
/// restent cherchés comme des noms : ils ne sont pas des mots vides.
const ARCHITECTURE_WORDS: &[&str] = &[
    "architecture",
    "architectures",
    "architectural",
    "architecturale",
    "architecturales",
    "architecturaux",
    "overview",
    "overviews",
    "apercu",
    "structure",
    "structures",
    "organisation",
    "organisations",
    "organization",
    "organizations",
    "organized",
    "organise",
    "organisee",
    "organises",
    "organisees",
    "subsystem",
    "subsystems",
    "theme",
    "themes",
    "community",
    "communities",
    "communaute",
    "communautes",
];

/// Vrai si le mot (forme repliée exacte) est un mot d'architecture (REQ-260, DEC-193).
pub(crate) fn is_architecture_word(word: &str) -> bool {
    ARCHITECTURE_WORDS.contains(&word)
}

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
        assert_eq!(PROSE_STOP_WORDS.len(), 181);
    }

    /// Les 45 derniers mots vides ajoutés par REQ-259, dans l'ordre.
    #[test]
    fn req259_ac4_last_45_stop_words() {
        let want: Vec<&str> = "invoke invokes invoked invoking appele appelee appeles appelees appeler appelant appelants invoque invoquent utilisee utilisees utiliser utilisant extending etend etendent inherit inherits inherited inheriting herite heritent heriter implementing implemente implementent implementee implementer referenced referencing referencent mentioned mentionne mentionnent linked contain contains contained containing contient contiennent".split(' ').collect();
        assert_eq!(want.len(), 45);
        assert_eq!(&PROSE_STOP_WORDS[PROSE_STOP_WORDS.len() - 45..], &want[..]);
    }

    #[test]
    fn req259_ac1_link_kinds_table() {
        use EdgeKind::*;
        assert_eq!(link_kinds("calls"), &[Calls]);
        assert_eq!(
            link_kinds("utilise"),
            &[Calls, References, Imports, DependsOn]
        );
        assert_eq!(link_kinds("inherits"), &[Extends, Implements]);
        assert_eq!(link_kinds("contient"), &[Contains]);
        assert_eq!(link_kinds("lien"), &[LinksTo]);
        assert!(link_kinds("alpha").is_empty());
        assert!(link_kinds("Calls").is_empty());
    }

    /// Formes de la table : repliées, d'au moins 3 caractères, sans doublon, hors `STOP_WORDS`.
    #[test]
    fn req259_ac1_link_words_are_folded_and_distinct() {
        let all = "call calls called calling caller callers invoke invokes invoked invoking appel appels appelle appellent appele appelee appeles appelees appeler appelant appelants invoque invoquent use uses used using usage utilise utilisent utilisee utilisees utiliser utilisant extend extends extended extending subclass subclasses superclass superclasses etend etendent inherit inherits inherited inheriting herite heritent heriter implement implements implemented implementing implementation implementations implemente implementent implementee implementer import imports imported importing importe importent importer depend depends depending dependent dependency dependencies dependance dependances reference references referenced referencing referencent mention mentions mentioned mentionne mentionnent link links linked lien liens contain contains contained containing contient contiennent";
        let mut seen = std::collections::HashSet::new();
        for w in all.split(' ') {
            assert!(seen.insert(w), "doublon {w}");
            assert!(w.chars().count() >= 3, "trop court {w}");
            assert_eq!(fold(w), w, "non replié {w}");
            assert!(!STOP_WORDS.contains(&w), "dans STOP_WORDS {w}");
            assert!(!link_kinds(w).is_empty(), "absent de la table {w}");
        }
    }

    #[test]
    fn req260_ac1_architecture_words_table() {
        assert_eq!(ARCHITECTURE_WORDS.len(), 28);
        let mut seen = std::collections::HashSet::new();
        for w in ARCHITECTURE_WORDS {
            assert_eq!(fold(w), *w, "{w}");
            assert!(w.chars().count() >= 3, "{w}");
            assert!(seen.insert(*w), "doublon {w}");
            assert!(!STOP_WORDS.contains(w), "{w}");
            assert!(is_architecture_word(w));
        }
        assert!(!is_architecture_word("module"));
        assert!(!is_architecture_word("modules"));
    }
}
