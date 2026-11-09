//! Masquage lexical des valeurs sensibles et des blocs PEM avant envoi au LLM (REQ-264, DEC-198).
//! Fonction pure : jamais d'erreur, ce qui n'est pas reconnu reste tel quel, et le nombre de `\n`
//! ne change jamais.

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::filter::FileType;

const MARK: &str = "***";
const SENSITIVE: [&str; 12] = [
    "password",
    "secret",
    "token",
    "credential",
    "apikey",
    "api_key",
    "api-key",
    "privatekey",
    "private_key",
    "private-key",
    "passwd",
    "pwd",
];
const DEPENDENCY_KEYS: [&str; 7] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
    "bundledDependencies",
    "overrides",
    "resolutions",
];

/// Types de fichiers dont le texte est masqué.
pub fn is_targeted(t: FileType) -> bool {
    matches!(
        t,
        FileType::Yaml | FileType::Json | FileType::Xml | FileType::Sql | FileType::Npm
    )
}

/// Nom de clé sensible, sans égard à la casse.
pub fn is_sensitive(name: &str) -> bool {
    let lower = name.to_lowercase();
    SENSITIVE.iter().any(|p| lower.contains(p))
}

/// Valeur qui ne peut pas être un secret : elle reste visible.
pub fn is_kept_value(raw: &str) -> bool {
    let mut t = raw.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
            t = t[1..t.len() - 1].trim();
        }
    }
    matches!(
        t.to_lowercase().as_str(),
        "true" | "false" | "null" | "~" | ""
    ) || (t.len() >= 3
        && t.starts_with("${")
        && t.ends_with('}')
        && !t[..t.len() - 1].contains('}'))
        || (t.len() >= 4
            && t.starts_with("{{")
            && t.ends_with("}}")
            && !t[..t.len() - 2].contains("}}"))
}

/// Texte masqué et nombre de valeurs et de blocs PEM masqués.
pub fn redact<'a>(path: &str, text: &'a str) -> (Cow<'a, str>, usize) {
    let Some(t) = FileType::of(path).filter(|t| is_targeted(*t)) else {
        return (Cow::Borrowed(text), 0);
    };
    let (keyed, n1) = match t {
        FileType::Yaml => redact_yaml(text),
        FileType::Json => redact_json(text, false),
        FileType::Npm => redact_json(text, true),
        FileType::Xml => redact_xml(text),
        FileType::Sql => redact_sql(text),
        _ => (text.to_string(), 0),
    };
    let (out, n2) = redact_pem(&keyed);
    let n = n1 + n2;
    if n == 0 {
        (Cow::Borrowed(text), 0)
    } else {
        (Cow::Owned(out), n)
    }
}

fn split_eol(line: &str) -> (&str, &str) {
    let body = line.strip_suffix('\n').unwrap_or(line);
    let body = body.strip_suffix('\r').unwrap_or(body);
    (body, &line[body.len()..])
}

/// Clé en début de ligne : (indentation, clé, position du `:` dans la ligne).
fn yaml_key(body: &str) -> Option<(usize, &str, usize)> {
    let mut pos = body.len() - body.trim_start().len();
    while body[pos..].starts_with("- ") {
        pos += 2;
        pos += body[pos..].len() - body[pos..].trim_start().len();
    }
    let rest = &body[pos..];
    let (key, colon) = match rest.as_bytes().first()? {
        q @ (b'"' | b'\'') => {
            let end = rest[1..].find(*q as char)? + 1;
            let after = &rest[end + 1..];
            let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
            if !after[spaces..].starts_with(':') {
                return None;
            }
            (&rest[1..end], pos + end + 1 + spaces)
        }
        b'#' => return None,
        _ => {
            let mut at = None;
            for (i, _) in rest.match_indices(':') {
                let next = rest[i + 1..].chars().next();
                if next.is_none() || next == Some(' ') || next == Some('\t') {
                    at = Some(i);
                    break;
                }
            }
            let i = at?;
            (rest[..i].trim_end(), pos + i)
        }
    };
    Some((pos, key, colon))
}

/// Valeur d'une ligne YAML sans son commentaire de fin.
fn yaml_value(rest: &str) -> &str {
    let t = rest.trim();
    if let Some(q) = t.chars().next().filter(|c| *c == '"' || *c == '\'') {
        if let Some(end) = t[1..].find(q) {
            return &t[..end + 2];
        }
        return t;
    }
    if t.starts_with('#') {
        return "";
    }
    match t.find(" #") {
        Some(i) => &t[..i],
        None => t,
    }
}

/// Valeur en bloc (`|`, `>`, indicateurs, étiquette devant).
fn is_block_value(value: &str) -> bool {
    let mut t = value.trim_start();
    while t.starts_with('!') || t.starts_with('&') {
        match t.find(char::is_whitespace) {
            Some(i) => t = t[i..].trim_start(),
            None => return false,
        }
    }
    let Some(r) = t.strip_prefix(['|', '>']) else {
        return false;
    };
    let r = r.trim_start_matches(|c: char| c == '+' || c == '-' || c.is_ascii_digit());
    r.is_empty() || r.starts_with(' ') || r.starts_with('\t') || r.starts_with('#')
}

fn redact_yaml(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut block_indent: Option<usize> = None;
    let mut block_hit = false;
    for line in text.split_inclusive('\n') {
        let (body, eol) = split_eol(line);
        if let Some(ind) = block_indent {
            let cur = body.len() - body.trim_start().len();
            if body.trim().is_empty() {
                out.push_str(line);
                continue;
            }
            if cur > ind {
                out.push_str(&body[..cur]);
                out.push_str(MARK);
                out.push_str(eol);
                if !block_hit {
                    block_hit = true;
                    count += 1;
                }
                continue;
            }
            block_indent = None;
        }
        if let Some((indent, key, colon)) = yaml_key(body) {
            if is_sensitive(key) {
                let rest = &body[colon + 1..];
                let value = yaml_value(rest);
                if value.contains("-----BEGIN ")
                    || value.starts_with('[')
                    || (value.starts_with('{') && !value.starts_with("{{"))
                {
                    out.push_str(line);
                    continue;
                }
                if is_block_value(value) {
                    block_indent = Some(indent);
                    block_hit = false;
                    out.push_str(line);
                    continue;
                }
                if !is_kept_value(value) {
                    out.push_str(&body[..colon + 1]);
                    out.push_str(" \"***\"");
                    out.push_str(eol);
                    count += 1;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    (out, count)
}

enum Frame {
    Object {
        expect_key: bool,
        key: Option<String>,
        deps: bool,
    },
    Array,
}

/// Fin de la chaîne JSON qui commence en `start` (guillemet) : indice du guillemet fermant.
fn json_string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

fn redact_json(text: &str, npm: bool) -> (String, usize) {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut stack: Vec<Frame> = Vec::new();
    let mut count = 0;
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let Some(end) = json_string_end(b, i) else {
                    break;
                };
                let raw = &text[i + 1..end];
                match stack.last_mut() {
                    Some(Frame::Object {
                        expect_key: true,
                        key,
                        ..
                    }) => *key = Some(raw.to_string()),
                    Some(Frame::Object {
                        key: Some(k), deps, ..
                    }) if !*deps && is_sensitive(k) && !is_kept_value(raw) => {
                        out.push_str(&text[copied..i]);
                        out.push_str("\"***\"");
                        out.push_str(&"\n".repeat(raw.matches('\n').count()));
                        copied = end + 1;
                        count += 1;
                    }
                    _ => {}
                }
                i = end + 1;
            }
            b'{' => {
                let deps = npm
                    && matches!(stack.last(), Some(Frame::Object { key: Some(k), .. })
                        if DEPENDENCY_KEYS.contains(&k.as_str()));
                stack.push(Frame::Object {
                    expect_key: true,
                    key: None,
                    deps,
                });
                i += 1;
            }
            b'[' => {
                stack.push(Frame::Array);
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                i += 1;
            }
            b':' => {
                if let Some(Frame::Object { expect_key, .. }) = stack.last_mut() {
                    *expect_key = false;
                }
                i += 1;
            }
            b',' => {
                if let Some(Frame::Object { expect_key, .. }) = stack.last_mut() {
                    *expect_key = true;
                }
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < b.len() && matches!(b[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                if let Some(Frame::Object {
                    expect_key: false,
                    key: Some(k),
                    deps: false,
                }) = stack.last()
                {
                    if is_sensitive(k) {
                        out.push_str(&text[copied..start]);
                        out.push_str("\"***\"");
                        copied = i;
                        count += 1;
                    }
                }
            }
            _ => i += 1,
        }
    }
    out.push_str(&text[copied..]);
    (out, count)
}

/// Remplace chaque ligne non vide de `s` par sa marque, en gardant indentation et sauts de ligne.
fn mask_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (n, seg) in s.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let body = seg.trim();
        if body.is_empty() {
            out.push_str(seg);
        } else {
            let start = seg.len() - seg.trim_start().len();
            out.push_str(&seg[..start]);
            out.push_str(MARK);
            out.push_str(&seg[start + body.len()..]);
        }
    }
    out
}

/// Fin du texte d'un élément qui commence en `from` : première balise, CDATA sautés.
fn xml_text_end(text: &str, from: usize) -> usize {
    let mut i = from;
    while let Some(rel) = text[i..].find('<') {
        let at = i + rel;
        let end = if text[at..].starts_with("<![CDATA[") {
            "]]>"
        } else if text[at..].starts_with("<!--") {
            "-->"
        } else if text[at..].starts_with("<?") {
            "?>"
        } else {
            return at;
        };
        match text[at..].find(end) {
            Some(e) => i = at + e + end.len(),
            None => return text.len(),
        }
    }
    text.len()
}

/// Premier `pat` à partir de `from`, ou `None`.
fn xml_skip(text: &str, from: usize, pat: &str) -> Option<usize> {
    text[from..].find(pat).map(|e| from + e + pat.len())
}

fn redact_xml(text: &str) -> (String, usize) {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut count = 0;
    let mut i = 0;
    while let Some(rel) = text[i..].find('<') {
        let at = i + rel;
        let rest = &text[at..];
        let skip = if rest.starts_with("<!--") {
            xml_skip(text, at, "-->")
        } else if rest.starts_with("<?") {
            xml_skip(text, at, "?>")
        } else if rest.starts_with("<![CDATA[") {
            xml_skip(text, at, "]]>")
        } else if rest.starts_with("<!") || rest.starts_with("</") {
            xml_skip(text, at, ">")
        } else {
            // Balise ouvrante.
            let name_start = at + 1;
            let mut p = name_start;
            while p < b.len() && !b[p].is_ascii_whitespace() && b[p] != b'/' && b[p] != b'>' {
                p += 1;
            }
            let mut sensitive_text = is_sensitive(&text[name_start..p]);
            let mut attrs: Vec<(&str, usize, usize)> = Vec::new();
            let mut self_closing = false;
            let mut closed = false;
            while p < b.len() {
                while p < b.len() && b[p].is_ascii_whitespace() {
                    p += 1;
                }
                if p >= b.len() {
                    break;
                }
                if b[p] == b'>' {
                    p += 1;
                    closed = true;
                    break;
                }
                if b[p] == b'/' {
                    self_closing = true;
                    p += 1;
                    continue;
                }
                let a_start = p;
                while p < b.len()
                    && !b[p].is_ascii_whitespace()
                    && !matches!(b[p], b'=' | b'>' | b'/')
                {
                    p += 1;
                }
                let a_name = &text[a_start..p];
                while p < b.len() && b[p].is_ascii_whitespace() {
                    p += 1;
                }
                if p < b.len() && b[p] == b'=' {
                    p += 1;
                    while p < b.len() && b[p].is_ascii_whitespace() {
                        p += 1;
                    }
                    if p < b.len() && (b[p] == b'"' || b[p] == b'\'') {
                        let q = b[p] as char;
                        let Some(e) = text[p + 1..].find(q) else {
                            p = b.len();
                            break;
                        };
                        attrs.push((a_name, p + 1, p + 1 + e));
                        p += e + 2;
                    } else {
                        let v = p;
                        while p < b.len() && !b[p].is_ascii_whitespace() && b[p] != b'>' {
                            p += 1;
                        }
                        attrs.push((a_name, v, p));
                    }
                }
            }
            if !closed {
                break;
            }
            let keyed = attrs.iter().any(|(n, s, e)| {
                matches!(n.to_lowercase().as_str(), "name" | "key") && is_sensitive(&text[*s..*e])
            });
            for (n, s, e) in &attrs {
                let hit = is_sensitive(n) || (keyed && n.eq_ignore_ascii_case("value"));
                if hit && !is_kept_value(&text[*s..*e]) {
                    out.push_str(&text[copied..*s]);
                    out.push_str(&mask_lines(&text[*s..*e]));
                    copied = *e;
                    count += 1;
                }
            }
            sensitive_text |= keyed;
            let mut next = p;
            if sensitive_text && !self_closing {
                let end = xml_text_end(text, p);
                let t = &text[p..end];
                if !is_kept_value(t) {
                    out.push_str(&text[copied..p]);
                    out.push_str(&mask_lines(t));
                    copied = end;
                    count += 1;
                }
                next = end;
            }
            Some(next)
        };
        match skip {
            Some(n) => i = n,
            None => break,
        }
    }
    out.push_str(&text[copied..]);
    (out, count)
}

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Word,
    Quoted,
    Str,
    Num,
    Punct(u8),
}

/// Jetons lexicaux d'un texte SQL : (genre, début, fin). Commentaires et blocs `$tag$` sautés ;
/// pour `Str` et `Quoted`, la plage est celle de l'intérieur. S'arrête à une construction non fermée.
fn sql_tokens(text: &str) -> Vec<(Tok, usize, usize)> {
    let b = text.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if text[i..].starts_with("--") {
            i = text[i..].find('\n').map_or(b.len(), |e| i + e + 1);
        } else if text[i..].starts_with("/*") {
            match text[i + 2..].find("*/") {
                Some(e) => i += e + 4,
                None => break,
            }
        } else if c == b'\'' || c == b'"' || c == b'`' || (c == b'[' && text[i..].contains(']')) {
            let close = if c == b'[' { b']' } else { c };
            let mut j = i + 1;
            let end = loop {
                match text[j..].find(close as char) {
                    None => break None,
                    Some(e) if c != b'[' && b.get(j + e + 1) == Some(&close) => j += e + 2,
                    Some(e) => break Some(j + e),
                }
            };
            let Some(end) = end else { break };
            let kind = if c == b'\'' { Tok::Str } else { Tok::Quoted };
            toks.push((kind, i + 1, end));
            i = end + 1;
        } else if c == b'$' {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if b.get(j) == Some(&b'$') {
                let tag = &text[i..=j];
                match text[j + 1..].find(tag) {
                    Some(e) => i = j + 1 + e + tag.len(),
                    None => break,
                }
            } else {
                // Paramètre `$1` : pas un nombre.
                i = j.max(i + 1);
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let s = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$') || b[i] >= 0x80)
            {
                i += 1;
            }
            // Préfixe `N'…'` : la chaîne reste une chaîne.
            if !(i == s + 1 && (b[s] == b'N' || b[s] == b'n') && b.get(i) == Some(&b'\'')) {
                toks.push((Tok::Word, s, i));
            }
        } else if c.is_ascii_digit() {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            toks.push((Tok::Num, s, i));
        } else {
            toks.push((Tok::Punct(c), i, i + 1));
            i += 1;
        }
    }
    toks
}

/// Jetons masqués par l'`INSERT … (colonnes) VALUES (…), (…)` dont le mot est en `k`.
fn sql_insert(text: &str, toks: &[(Tok, usize, usize)], k: usize, hits: &mut BTreeSet<usize>) {
    let mut j = k + 1;
    while j < toks.len() && toks[j].0 != Tok::Punct(b'(') {
        if toks[j].0 == Tok::Punct(b';') {
            return;
        }
        j += 1;
    }
    let mut cols = Vec::new();
    j += 1;
    while j < toks.len() && toks[j].0 != Tok::Punct(b')') {
        if matches!(toks[j].0, Tok::Word | Tok::Quoted) {
            cols.push(is_sensitive(&text[toks[j].1..toks[j].2]));
        }
        j += 1;
    }
    j += 1;
    let values = toks
        .get(j)
        .is_some_and(|t| t.0 == Tok::Word && text[t.1..t.2].eq_ignore_ascii_case("values"));
    if !values || !cols.iter().any(|c| *c) {
        return;
    }
    j += 1;
    while toks.get(j).map(|t| t.0) == Some(Tok::Punct(b'(')) {
        j += 1;
        let (mut depth, mut idx, mut len, mut first) = (1, 0, 0, j);
        while j < toks.len() && depth > 0 {
            match toks[j].0 {
                Tok::Punct(b'(') => {
                    depth += 1;
                    len += 1;
                }
                Tok::Punct(b')') | Tok::Punct(b',') if depth == 1 => {
                    if len == 1
                        && matches!(toks[first].0, Tok::Str | Tok::Num)
                        && cols.get(idx) == Some(&true)
                    {
                        hits.insert(first);
                    }
                    idx += 1;
                    len = 0;
                    first = j + 1;
                    if toks[j].0 == Tok::Punct(b')') {
                        depth = 0;
                    }
                }
                Tok::Punct(b')') => {
                    depth -= 1;
                    len += 1;
                }
                _ => len += 1,
            }
            j += 1;
        }
        if toks.get(j).map(|t| t.0) == Some(Tok::Punct(b',')) {
            j += 1;
        }
    }
}

fn redact_sql(text: &str) -> (String, usize) {
    let toks = sql_tokens(text);
    let mut hits = BTreeSet::new();
    for (k, (kind, s, e)) in toks.iter().enumerate() {
        if *kind != Tok::Word {
            continue;
        }
        let word = &text[*s..*e];
        if word.eq_ignore_ascii_case("insert") {
            sql_insert(text, &toks, k, &mut hits);
        } else if is_sensitive(word) {
            let mut n = k + 1;
            if toks.get(n).map(|t| t.0) == Some(Tok::Punct(b'=')) {
                n += 1;
            }
            if matches!(toks.get(n).map(|t| t.0), Some(Tok::Str | Tok::Num)) {
                hits.insert(n);
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut count = 0;
    for k in hits {
        let (kind, s, e) = toks[k];
        let inner = &text[s..e];
        let (a, z, body) = if kind == Tok::Str {
            if is_kept_value(inner) {
                continue;
            }
            (s - 1, e + 1, mask_lines(inner))
        } else {
            (s, e, MARK.to_string())
        };
        out.push_str(&text[copied..a]);
        out.push('\'');
        out.push_str(&body);
        out.push('\'');
        copied = z;
        count += 1;
    }
    out.push_str(&text[copied..]);
    (out, count)
}

fn redact_pem(text: &str) -> (String, usize) {
    const BEGIN: &str = "-----BEGIN ";
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut pos = 0;
    while let Some(rel) = text[pos..].find(BEGIN) {
        let label_start = pos + rel + BEGIN.len();
        let Some(label_len) = text[label_start..].find("-----") else {
            break;
        };
        let label = &text[label_start..label_start + label_len];
        let body_start = label_start + label_len + 5;
        if label.contains('\n') {
            out.push_str(&text[pos..label_start]);
            pos = label_start;
            continue;
        }
        let end_marker = format!("-----END {label}-----");
        let (body_end, tail_start) = match text[body_start..].find(&end_marker) {
            Some(e) => (body_start + e, body_start + e),
            None => (text.len(), text.len()),
        };
        let body = &text[body_start..body_end];
        out.push_str(&text[pos..body_start]);
        if body.trim().is_empty()
            || body
                .lines()
                .all(|l| l.trim().is_empty() || l.trim() == MARK)
        {
            out.push_str(body);
        } else if !body.contains('\n') {
            out.push_str(MARK);
            count += 1;
        } else {
            for (n, seg) in body.split('\n').enumerate() {
                if n > 0 {
                    out.push('\n');
                }
                let keep = seg.trim().is_empty();
                if keep {
                    out.push_str(seg);
                } else {
                    let (s, eol) = split_eol(seg);
                    out.push_str(&s[..s.len() - s.trim_start().len()]);
                    out.push_str(MARK);
                    out.push_str(&seg[s.len()..eol.len() + s.len()]);
                }
            }
            count += 1;
        }
        pos = tail_start;
    }
    out.push_str(&text[pos..]);
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str, text: &str) -> (String, usize) {
        let (o, n) = redact(path, text);
        (o.into_owned(), n)
    }

    #[test]
    fn req264_ac1_yaml() {
        assert_eq!(
            r("a.yml", "db:\n  password: abc # vieux\n  user: bob\n"),
            ("db:\n  password: \"***\"\n  user: bob\n".into(), 1)
        );
        assert_eq!(r("a.yml", "- api-key: abc").0, "- api-key: \"***\"");
        assert_eq!(r("a.yml", "\"Token\": 'x'\n").0, "\"Token\": \"***\"\n");
        assert_eq!(
            r("a.yml", "private_key: |\n  l1\n  l2\nautre: 1\n"),
            ("private_key: |\n  ***\n  ***\nautre: 1\n".into(), 1)
        );
        assert_eq!(
            r("a.yml", "password: !vault |\n  $ANSIBLE\n").0,
            "password: !vault |\n  ***\n"
        );
        assert_eq!(r("a.yml", "passwd: abc\nDB_PWD: abc\n").1, 2);
        assert_eq!(r("a.yml", "cwd: /x\n"), ("cwd: /x\n".into(), 0));
        assert_eq!(
            r("a.yml", "a: 1\r\npassword: x\r\n").0,
            "a: 1\r\npassword: \"***\"\r\n"
        );
    }

    #[test]
    fn req264_ac2_json() {
        let (o, n) = r("a.json", r#"{"a":{"apiKey":"x","n":1},"password":123}"#);
        assert_eq!(o, r#"{"a":{"apiKey":"***","n":1},"password":"***"}"#);
        assert_eq!(n, 2);
        serde_json::from_str::<serde_json::Value>(&o).unwrap();
        let same = r#"{"k":"a \" b","token":{"id":"x"},"secrets":["x"]}"#;
        serde_json::from_str::<serde_json::Value>(same).unwrap();
        assert_eq!(r("a.json", same), (same.to_string(), 0));
        let (o, _) = r("a.json", "{\"a\": \"q\\\"\",\n \"secret\": \"y\"}\n");
        assert_eq!(o, "{\"a\": \"q\\\"\",\n \"secret\": \"***\"}\n");
        serde_json::from_str::<serde_json::Value>(&o).unwrap();
        // coupé : pas de panique
        r("a.json", "{\"password\": \"abc");
    }

    #[test]
    fn req264_ac3_package_json() {
        let t = r#"{"dependencies":{"jsonwebtoken":"^9.0.0"},"devDependencies":{"secret-lib":"1.0.0"},"config":{"token":"x"}}"#;
        let (o, n) = r("package.json", t);
        assert_eq!(n, 1);
        assert_eq!(
            o,
            r#"{"dependencies":{"jsonwebtoken":"^9.0.0"},"devDependencies":{"secret-lib":"1.0.0"},"config":{"token":"***"}}"#
        );
        let (o, n) = r("a.json", t);
        assert_eq!(n, 3);
        assert!(o.contains(r#""jsonwebtoken":"***""#) && o.contains(r#""secret-lib":"***""#));
    }

    #[test]
    fn req264_ac6_pem() {
        let t = "k: |\n  x\ncert:\n-----BEGIN CERT-----\nAAA\nBBB\n-----END CERT-----\nz: 1\n";
        assert_eq!(
            r("a.yml", t),
            (
                "k: |\n  x\ncert:\n-----BEGIN CERT-----\n***\n***\n-----END CERT-----\nz: 1\n"
                    .into(),
                1
            )
        );
        assert_eq!(
            r(
                "a.json",
                r#"{"c":"-----BEGIN X-----\nAB\n-----END X-----"}"#
            )
            .0,
            r#"{"c":"-----BEGIN X-----***-----END X-----"}"#
        );
        let (o, n) = r(
            "a.json",
            r#"{"private_key":"-----BEGIN X-----\nAB\n-----END X-----"}"#,
        );
        assert_eq!((o.as_str(), n), (r#"{"private_key":"***"}"#, 1));
        let (o, n) = r("a.yml", "-----BEGIN X-----\nAA\nBB\n");
        assert_eq!((o.as_str(), n), ("-----BEGIN X-----\n***\n***\n", 1));
        let (o, _) = r("a.yml", "q: 1\n-----BEGIN X-----\nAA");
        assert_eq!(o.matches('\n').count(), 2);
        assert!(!o.contains("AA"));
    }

    #[test]
    fn req264_ac7_faux_positifs() {
        for v in [
            "true",
            "FALSE",
            "null",
            "~",
            "\"\"",
            "${DB_PASSWORD}",
            "{{ .Values.x }}",
        ] {
            let t = format!("password: {v}\n");
            assert_eq!(r("a.yml", &t), (t.clone(), 0), "{v}");
        }
        let t = "user: bob\ndescription: un token de session\n";
        assert_eq!(r("a.yml", t), (t.to_string(), 0));
        assert_eq!(r("a.yml", "max_tokens: 4096\n").0, "max_tokens: \"***\"\n");
        let j = r#"{"password":"${X}","token":null,"secret":true,"pwd":""}"#;
        assert_eq!(r("a.json", j), (j.to_string(), 0));
    }

    #[test]
    fn req264_corrections() {
        assert_eq!(
            r(
                "a.yml",
                "private_key: -----BEGIN X-----\n  MIIESECRET\n  -----END X-----\n"
            ),
            (
                "private_key: -----BEGIN X-----\n  ***\n  -----END X-----\n".into(),
                1
            )
        );
        assert_eq!(
            r(
                "a.yml",
                "k: \"-----BEGIN X-----\n  MIIESECRET\n  -----END X-----\"\n"
            )
            .1,
            1
        );
        assert_eq!(
            r(
                "a.yml",
                "-----BEGIN X-----FIRST\nMID\nLAST-----END X-----\n"
            ),
            ("-----BEGIN X-----***\n***\n***-----END X-----\n".into(), 1)
        );
        assert_eq!(
            r("a.yml", "-----BEGIN X-----FIRST\nMID").0,
            "-----BEGIN X-----***\n***"
        );
        assert_eq!(
            r("a.yml", "\"password\" : hunter2\n").0,
            "\"password\" : \"***\"\n"
        );
        for v in ["${A}hunter2${B}", "{{a}}hunter2{{b}}"] {
            assert_eq!(r("a.yml", &format!("password: {v}\n")).1, 1, "{v}");
        }
        assert_eq!(
            r("a.yml", "- password: |\n    l1\n  user: bob\n").0,
            "- password: |\n    ***\n  user: bob\n"
        );
        let t = "secrets: [a, b]\ncredentials: {user: a}\n";
        assert_eq!(r("a.yml", t), (t.to_string(), 0));
    }

    #[test]
    fn req264_ac4_xml() {
        assert_eq!(
            r("a.xml", r#"<a password="x" b="y"/>"#),
            (r#"<a password="***" b="y"/>"#.into(), 1)
        );
        assert_eq!(
            r("a.xml", "<password>x</password>"),
            ("<password>***</password>".into(), 1)
        );
        assert_eq!(
            r("a.xml", r#"<add key="ApiKey" value="x"/>"#).0,
            r#"<add key="ApiKey" value="***"/>"#
        );
        assert_eq!(
            r("a.xml", "<property name=\"db.password\">x\ny</property>").0,
            "<property name=\"db.password\">***\n***</property>"
        );
        let same = r#"<a name="titre" value="x"/>"#;
        assert_eq!(r("a.xml", same), (same.into(), 0));
        let same = "<!-- <password>x</password> --><![CDATA[<password>x</password>]]><?pi password=\"x\"?>";
        assert_eq!(r("a.xml", same), (same.into(), 0));
        assert_eq!(
            r("a.xml", "<token><![CDATA[a<b]]></token>").0,
            "<token>***</token>"
        );
        let same = "<password>${X}</password><token></token>";
        assert_eq!(r("a.xml", same), (same.into(), 0));
    }

    #[test]
    fn req264_ac5_sql() {
        assert_eq!(r("a.sql", "password = 'x'"), ("password = '***'".into(), 1));
        assert_eq!(
            r("a.sql", "ALTER USER u WITH PASSWORD 'x'").0,
            "ALTER USER u WITH PASSWORD '***'"
        );
        assert_eq!(
            r(
                "a.sql",
                "INSERT INTO u (id, api_key, nom) VALUES (1, 'k1', 'a'), (2, 'k2', 'b');"
            ),
            (
                "INSERT INTO u (id, api_key, nom) VALUES (1, '***', 'a'), (2, '***', 'b');".into(),
                2
            )
        );
        assert_eq!(r("a.sql", "token = 42").0, "token = '***'");
        let same = "-- password = 'x'\n/* token 'y' */ SELECT 'password = ''z''', \"secret\" 'q', $b$ pwd 'w' $b$;";
        assert_eq!(r("a.sql", same), (same.into(), 0));
        // chaîne non fermée : le reste est laissé
        assert_eq!(
            r("a.sql", "x = 1; pwd = 'abc\nfin"),
            ("x = 1; pwd = 'abc\nfin".into(), 0)
        );
        assert_eq!(r("a.sql", "pwd = 'a\nb'").0, "pwd = '***\n***'");
    }

    #[test]
    fn req264_ac8_types_et_mal_formes() {
        let t = "password: x\n-----BEGIN X-----\nAA\n-----END X-----\n";
        let paths = [
            "a.md",
            "A.java",
            "a.kt",
            "a.groovy",
            "a.scala",
            "a.js",
            "a.ts",
            "pom.xml",
            "ivy.xml",
            "build.gradle",
        ];
        assert_eq!(FileType::ALL.len(), 15);
        let mut visés = 0;
        for ft in FileType::ALL {
            visés += usize::from(is_targeted(ft));
        }
        assert_eq!(visés, 5);
        for p in paths {
            assert!(!is_targeted(FileType::of(p).unwrap()), "{p}");
            let (o, n) = redact(p, t);
            assert!(matches!(o, Cow::Borrowed(_)) && n == 0, "{p}");
        }
        let wellformed = [
            ("a.yml", "k: 1\npassword: x\n"),
            ("a.json", "{\n \"password\": \"x\"\n}\n"),
            ("package.json", "{\n \"token\": \"x\"\n}\n"),
            ("a.xml", "<a>\n <password>x</password>\n</a>\n"),
            ("a.sql", "UPDATE u\nSET pwd = 'x';\n"),
        ];
        for (p, w) in wellformed {
            let (o, n) = redact(p, w);
            assert!(n > 0 && !o.contains("x</") && !o.contains("'x'"), "{p}");
            assert_eq!(o.matches('\n').count(), w.matches('\n').count(), "{p}");
            let (o, n) = redact(p, t);
            assert!(n > 0 && !o.contains("AA"), "{p}");
            assert_eq!(o.matches('\n').count(), t.matches('\n').count(), "{p}");
        }
        let bad = [
            ("a.json", "{\"password\": \"abc\n"),
            ("a.json", "{\"é\": [\"token\": 1, é\n\"secret\":"),
            ("a.xml", "<a password=\"x\n<b><token>é\n"),
            ("a.xml", "<a password=\"x\" b='\n"),
            ("a.xml", "<token>é\n<![CDATA[x\n"),
            (
                "a.sql",
                "password = 'é\nINSERT INTO t (api_key) VALUES ('x\n",
            ),
            ("a.sql", "INSERT INTO t (api_key, é) VALUES (1, 'é'),\n(2"),
            ("a.sql", "/* é\n pwd = 'x'"),
            ("a.sql", "$a$ pwd 'x'\n"),
            ("a.yml", "password: é\n  - [\n"),
            ("package.json", "{\"dependencies\": {\"é\n"),
        ];
        for (p, s) in bad {
            let (o, _) = redact(p, s);
            assert_eq!(
                o.matches('\n').count(),
                s.matches('\n').count(),
                "{p} {s:?}"
            );
        }
        for (p, s) in [
            ("a.xml", "<password>é\n</password>\n<a token=\"\n\">"),
            ("a.xml", "<password>é\nx</password>\n<a token=\"y\n\">"),
            (
                "a.sql",
                "pwd = 'é\n\n'\nINSERT INTO t (token) VALUES ('a\nb');",
            ),
        ] {
            let (o, n) = redact(p, s);
            assert!(n > 0, "{p}");
            assert_eq!(o.matches('\n').count(), s.matches('\n').count(), "{p}");
        }
    }

    #[test]
    fn req264_ac5_sql_delimiteurs() {
        let h = |sql: &str| r("a.sql", sql);
        for sql in [
            "INSERT INTO t ([first name], api_key) VALUES ('a', 'SECRET');",
            "SELECT [it's] FROM t;\npassword = 'SECRET';",
            "INSERT INTO t (\"a\"\"b\", api_key) VALUES (1, 'SECRET');",
            "INSERT INTO t (`a``b`, api_key) VALUES (1, 'SECRET');",
            "pwd = N'SECRET'",
            "INSERT INTO t (api_key) VALUES (N'SECRET');",
        ] {
            let (o, n) = h(sql);
            assert_eq!((n, o.contains("SECRET")), (1, false), "{sql}");
        }
        let same = "UPDATE t SET password = $1 WHERE id = $2";
        assert_eq!(h(same), (same.into(), 0));
        assert_eq!(h("pwd = N'x'").0, "pwd = N'***'");
    }

    #[test]
    fn req264_ac4_xml_commentaire_dans_element() {
        for t in [
            "<password>abc<!-- c -->SECRET</password>",
            "<password>abc<?pi x?>SECRET</password>",
        ] {
            let (o, n) = r("a.xml", t);
            assert_eq!((n, o.contains("SECRET")), (1, false), "{t}");
        }
    }

    #[test]
    fn non_vise_emprunte() {
        let (o, n) = redact("a.md", "password: x\n");
        assert!(matches!(o, Cow::Borrowed(_)));
        assert_eq!(n, 0);
        assert!(is_sensitive("DB_PWD") && !is_sensitive("cwd"));
    }
}
