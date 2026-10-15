//! Lecteur lexical des scripts Gradle, commun à Groovy et à Kotlin (DEC-025 A). Il ne comprend aucune grammaire :
//! il découpe le texte en jetons en sautant les commentaires, ce qui suffit à lire `dependencies { }` et
//! `rootProject.name`.

/// Nature d'un jeton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// Identifiant (ou nombre).
    Ident(String),
    /// Chaîne littérale : texte brut sans les guillemets (`$` conservé, échappement `\x` → `x`).
    Str(String),
    /// Ponctuation parmi `( ) { } [ ] , : = . ;`.
    Punct(char),
    /// Fin de ligne.
    Newline,
    /// Tout autre caractère.
    Other(char),
}

/// Jeton et sa position (décalage en octets, ligne à partir de 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub tok: Tok,
    pub pos: usize,
    pub line: usize,
}

/// Résultat de la lecture : les jetons lus et le signe d'une fin de fichier prématurée.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    /// Chaîne ou commentaire `/* */` non fermé en fin de fichier.
    pub unterminated: bool,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Découpe `src` en jetons.
pub fn lex(src: &str) -> Lexed {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let mut out = Lexed::default();
    let mut i = 0;
    let mut line = 1;
    while i < chars.len() {
        let (pos, c) = chars[i];
        let start_line = line;
        let push = |tok: Tok, out: &mut Lexed| {
            out.tokens.push(Token {
                tok,
                pos,
                line: start_line,
            })
        };
        match c {
            '\n' => {
                push(Tok::Newline, &mut out);
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '/' if at(i + 1) == Some('/') => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
            }
            '/' if at(i + 1) == Some('*') => {
                i += 2;
                loop {
                    match at(i) {
                        None => {
                            out.unterminated = true;
                            break;
                        }
                        Some('*') if at(i + 1) == Some('/') => {
                            i += 2;
                            break;
                        }
                        Some(ch) => {
                            if ch == '\n' {
                                line += 1;
                            }
                            i += 1;
                        }
                    }
                }
            }
            '\'' | '"' => {
                let triple = at(i + 1) == Some(c) && at(i + 2) == Some(c);
                i += if triple { 3 } else { 1 };
                let mut text = String::new();
                let mut closed = false;
                while let Some(ch) = at(i) {
                    if ch == '\\' {
                        if let Some(next) = at(i + 1) {
                            if next == '\n' {
                                line += 1;
                            }
                            text.push(next);
                            i += 2;
                        } else {
                            i += 1;
                        }
                        continue;
                    }
                    if ch == c {
                        if !triple {
                            i += 1;
                            closed = true;
                            break;
                        }
                        if at(i + 1) == Some(c) && at(i + 2) == Some(c) {
                            i += 3;
                            closed = true;
                            break;
                        }
                    }
                    if ch == '\n' {
                        line += 1;
                    }
                    text.push(ch);
                    i += 1;
                }
                if closed {
                    push(Tok::Str(text), &mut out);
                } else {
                    out.unterminated = true;
                }
            }
            '(' | ')' | '{' | '}' | '[' | ']' | ',' | ':' | '=' | '.' | ';' => {
                push(Tok::Punct(c), &mut out);
                i += 1;
            }
            c if is_ident_start(c) => {
                let mut s = String::new();
                while let Some(ch) = at(i).filter(|&ch| is_ident_start(ch)) {
                    s.push(ch);
                    i += 1;
                }
                push(Tok::Ident(s), &mut out);
            }
            c => {
                push(Tok::Other(c), &mut out);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        lex(src).tokens.into_iter().map(|t| t.tok).collect()
    }

    fn s(x: &str) -> Tok {
        Tok::Str(x.to_string())
    }

    fn id(x: &str) -> Tok {
        Tok::Ident(x.to_string())
    }

    #[test]
    fn identifiants_ponctuation_et_fin_de_ligne() {
        assert_eq!(
            toks("a.b = c;\nd"),
            vec![
                id("a"),
                Tok::Punct('.'),
                id("b"),
                Tok::Punct('='),
                id("c"),
                Tok::Punct(';'),
                Tok::Newline,
                id("d")
            ]
        );
        assert_eq!(toks("@ +"), vec![Tok::Other('@'), Tok::Other('+')]);
    }

    #[test]
    fn les_quatre_formes_de_chaine() {
        assert_eq!(
            toks(r#"'a' "b$c" '''d'e''' """f"g}""""#),
            vec![s("a"), s("b$c"), s("d'e"), s("f\"g}")]
        );
    }

    #[test]
    fn echappements() {
        assert_eq!(toks(r#"'it\'s' "q\"x""#), vec![s("it's"), s("q\"x")]);
    }

    #[test]
    fn commentaires_sautes() {
        let l = lex("a // x }\n/* y } \n z */ b");
        assert!(!l.unterminated);
        assert_eq!(
            l.tokens.into_iter().map(|t| t.tok).collect::<Vec<_>>(),
            vec![id("a"), Tok::Newline, id("b")]
        );
    }

    #[test]
    fn positions() {
        let l = lex("ab\n  cd");
        assert_eq!((l.tokens[0].pos, l.tokens[0].line), (0, 1));
        assert_eq!((l.tokens[2].pos, l.tokens[2].line), (5, 2));
    }

    #[test]
    fn fin_de_fichier_dans_une_chaine_ou_un_commentaire() {
        let l = lex("a 'abc");
        assert!(l.unterminated);
        assert_eq!(l.tokens.len(), 1);
        assert!(lex("a \"\"\"abc\"\"").unterminated);
        assert!(lex("a /* abc").unterminated);
        assert!(!lex("a '' b").unterminated);
    }
}
