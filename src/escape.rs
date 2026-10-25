//! Échappement à l'affichage (REQ-100, REQ-101, DEC-103) : une seule table de caractères, partagée
//! par la forme échappée des noms réels (`scan::escaped`) et par celle des chemins convertis.

use std::borrow::Cow;

/// Table unique : U+0000 à U+001F et U+007F en `\xHH`, U+0080 à U+009F, U+202A à U+202E et U+2066
/// à U+2069 en `\u{XXXX}` (chiffres hexadécimaux majuscules). `None` : le caractère reste tel quel.
pub fn control(c: char) -> Option<String> {
    match c as u32 {
        0x00..=0x1F | 0x7F => Some(format!("\\x{:02X}", c as u32)),
        0x80..=0x9F | 0x202A..=0x202E | 0x2066..=0x2069 => Some(format!("\\u{{{:04X}}}", c as u32)),
        _ => None,
    }
}

/// Variante chemin : la table seule ; `\`, `'` et U+FFFD sont écrits tels quels. Une valeur sans
/// caractère à échapper est rendue sans copie.
pub fn path(s: &str) -> Cow<'_, str> {
    if !s.chars().any(|c| control(c).is_some()) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match control(c) {
            Some(e) => out.push_str(&e),
            None => out.push(c),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req100_ac4_chemin_echappe_les_caracteres_de_la_table() {
        assert_eq!(path("a\u{1b}b.java"), "a\\x1Bb.java");
        assert_eq!(path("x\ty.java"), "x\\x09y.java");
        assert_eq!(path("a\u{7f}"), "a\\x7F");
        assert_eq!(path("a\u{9b}.java"), "a\\u{009B}.java");
        assert_eq!(path("a\u{202E}.java"), "a\\u{202E}.java");
        assert_eq!(path("a\u{2066}.java"), "a\\u{2066}.java");
    }

    #[test]
    fn req100_ac4_chemin_inchange_sans_copie() {
        for s in [
            "l'a\\b.java",
            "A\u{FFFD}.java",
            "é.java",
            "u\u{200F}\u{2028}",
        ] {
            let r = path(s);
            assert!(matches!(r, Cow::Borrowed(_)), "{s:?}");
            assert_eq!(r, s);
        }
    }

    #[test]
    fn req101_ac1_table_bornes() {
        for cp in [0x2029u32, 0x202F, 0x2065, 0x206A, 0xA0, 0x20, 0x7E] {
            assert!(control(char::from_u32(cp).unwrap()).is_none(), "{cp:X}");
        }
        for cp in 0x202Au32..=0x202E {
            assert_eq!(
                control(char::from_u32(cp).unwrap()).unwrap(),
                format!("\\u{{{cp:04X}}}")
            );
        }
        for cp in 0x2066u32..=0x2069 {
            assert!(control(char::from_u32(cp).unwrap()).is_some());
        }
    }
}
