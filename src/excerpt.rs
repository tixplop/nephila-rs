//! Texte envoyé au modèle pour un fichier JSON, YAML ou XML de données : le début du fichier et
//! le squelette de ses clés au-delà de L caractères, jamais découpé (REQ-239, DEC-178). Un fichier
//! SQL part sans ses INSERT et COPY massifs, puis reste découpé (REQ-240).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use quick_xml::events::Event as XmlEvent;
use quick_xml::reader::Reader;
use yaml_rust2::parser::{Event, Parser};

use crate::filter::FileType;

/// Plafond de L, en caractères.
const DATA_LIMIT: usize = 6_000;

/// Vrai pour un fichier JSON, YAML ou XML de données : il part en un seul morceau.
pub fn never_split(path: &str) -> bool {
    matches!(
        FileType::of(path),
        Some(FileType::Json | FileType::Yaml | FileType::Xml)
    )
}

/// L = min(6 000, `max_chunk_chars`), en caractères.
pub fn data_limit(max_chunk_chars: usize) -> usize {
    DATA_LIMIT.min(max_chunk_chars)
}

/// Marque d'un extrait avec squelette ; `kib` = octets du fichier / 1024, arrondi au supérieur.
pub fn mark_with_skeleton(kib: usize) -> String {
    format!(
        "[extrait tronqué de {kib} Ko : début du fichier, puis squelette des clés sur 2 niveaux]"
    )
}

/// Marque d'un extrait sans clé à montrer.
pub fn mark_head_only(kib: usize) -> String {
    format!("[extrait tronqué de {kib} Ko : début du fichier]")
}

/// Marque d'un SQL dont `k` instructions INSERT ou COPY sont retirées ; `kib` = octets / 1024,
/// arrondi au supérieur. Termine par un saut de ligne.
pub fn mark_sql(kib: usize, k: usize) -> String {
    format!("-- extrait tronqué de {kib} Ko : {k} instructions INSERT ou COPY retirées\n")
}

/// Titre de la section du squelette.
pub fn skeleton_title() -> &'static str {
    "[squelette des clés]"
}

/// Texte envoyé au modèle pour `path` : JSON, YAML et XML de données réduits au-delà de L, SQL
/// sans ses INSERT et COPY massifs ; tout autre type reste tel quel.
pub fn for_llm<'a>(path: &str, text: &'a str, max_chunk_chars: usize) -> Cow<'a, str> {
    if FileType::of(path) == Some(FileType::Sql) {
        return sql_for_llm(text);
    }
    if !never_split(path) {
        return Cow::Borrowed(text);
    }
    let limit = data_limit(max_chunk_chars);
    if text.len() <= limit || text.chars().count() <= limit {
        return Cow::Borrowed(text);
    }
    let head_chars = limit * 2 / 3;
    let head_end = text
        .char_indices()
        .nth(head_chars)
        .map_or(text.len(), |(i, _)| i);
    let head = &text[..head_end];
    let kib = text.len().div_ceil(1024);
    let skeleton = match FileType::of(path) {
        Some(FileType::Json) => json_skeleton(text),
        Some(FileType::Yaml) => yaml_skeleton(text),
        _ => xml_skeleton(text),
    };
    let lines = render(&skeleton, limit - head_chars);
    if lines.is_empty() {
        return Cow::Owned(format!("{}\n{head}", mark_head_only(kib)));
    }
    Cow::Owned(format!(
        "{}\n{head}\n{}\n{lines}",
        mark_with_skeleton(kib),
        skeleton_title()
    ))
}

/// Plafond, en caractères, des instructions INSERT et COPY gardées d'un fichier SQL.
const SQL_BULK_LIMIT: usize = 2_000;

/// SQL sans ses INSERT et COPY massifs (REQ-240) : on garde les premiers tant que leur total
/// reste dans le plafond, la première qui dépasse et les suivantes sont retirées.
fn sql_for_llm(text: &str) -> Cow<'_, str> {
    let ranges = crate::parse::sql::bulk_ranges(text);
    let mut total = 0;
    let mut cut = ranges.len();
    for (n, r) in ranges.iter().enumerate() {
        total += text[r.clone()].chars().count();
        if total > SQL_BULK_LIMIT {
            cut = n;
            break;
        }
    }
    let removed = &ranges[cut..];
    if removed.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = mark_sql(text.len().div_ceil(1024), removed.len());
    let mut pos = 0;
    for r in removed {
        out.push_str(&text[pos..r.start]);
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    Cow::Owned(out)
}

/// Clés de niveau 1 dans l'ordre de lecture, chacune avec ses clés de niveau 2, sans doublon.
#[derive(Default)]
struct Skeleton {
    top: Vec<(String, Vec<String>)>,
    index: HashMap<String, usize>,
    seen: HashSet<(usize, String)>,
}

impl Skeleton {
    /// Ajoute une clé de niveau 1 et rend son rang.
    fn level1(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        self.top.push((name.to_string(), Vec::new()));
        self.index.insert(name.to_string(), self.top.len() - 1);
        self.top.len() - 1
    }

    fn level2(&mut self, parent: usize, name: &str) {
        if self.seen.insert((parent, name.to_string())) {
            self.top[parent].1.push(name.to_string());
        }
    }
}

/// Lignes du squelette dans `budget` caractères (saut de ligne compris) ; la première ligne qui
/// ne tient pas est remplacée par `…`. Sans saut de ligne final.
fn render(skeleton: &Skeleton, budget: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    let all = skeleton.top.iter().flat_map(|(k, kids)| {
        std::iter::once(k.clone()).chain(kids.iter().map(|c| format!("  {c}")))
    });
    for line in all {
        let cost = line.chars().count() + 1;
        if used + cost > budget {
            lines.push("…".to_string());
            break;
        }
        used += cost;
        lines.push(line);
    }
    lines.join("\n")
}

/// Objets rencontrés en traversant les tableaux comme s'ils n'existaient pas.
fn objects<'a>(
    v: &'a serde_json::Value,
    out: &mut Vec<&'a serde_json::Map<String, serde_json::Value>>,
) {
    match v {
        serde_json::Value::Object(m) => out.push(m),
        serde_json::Value::Array(a) => a.iter().for_each(|x| objects(x, out)),
        _ => {}
    }
}

fn json_skeleton(text: &str) -> Skeleton {
    let mut sk = Skeleton::default();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return sk;
    };
    let mut roots = Vec::new();
    objects(&value, &mut roots);
    for root in roots {
        for (k, v) in root {
            let i = sk.level1(k);
            let mut inner = Vec::new();
            objects(v, &mut inner);
            for m in inner {
                for k2 in m.keys() {
                    sk.level2(i, k2);
                }
            }
        }
    }
    sk
}

/// Mapping YAML en cours de lecture.
struct YamlMap {
    /// Nombre de mappings englobants ; `usize::MAX` pour une clé complexe, ignorée.
    depth: usize,
    key_turn: bool,
    /// Rang de niveau 1 de la clé dont la valeur est lue (mapping de profondeur 0).
    current: Option<usize>,
    /// Rang de niveau 1 de la clé du mapping parent (mapping de profondeur 1).
    parent: Option<usize>,
}

/// Frame de la pile : `None` = séquence, transparente.
type YamlFrame = Option<YamlMap>;

fn yaml_done(stack: &mut [YamlFrame]) {
    if let Some(Some(m)) = stack.last_mut() {
        m.key_turn = !m.key_turn;
    }
}

fn yaml_skeleton(text: &str) -> Skeleton {
    let mut sk = Skeleton::default();
    let mut stack: Vec<YamlFrame> = Vec::new();
    let mut parser = Parser::new_from_str(text);
    while let Ok((ev, _)) = parser.next_token() {
        match ev {
            Event::StreamEnd => break,
            Event::DocumentStart => stack.clear(),
            Event::Scalar(name, ..) => {
                if let Some(Some(m)) = stack.last_mut() {
                    if m.key_turn {
                        match (m.depth, m.parent) {
                            (0, _) => m.current = Some(sk.level1(&name)),
                            (1, Some(p)) => sk.level2(p, &name),
                            _ => {}
                        }
                    }
                }
                yaml_done(&mut stack);
            }
            Event::Alias(_) => yaml_done(&mut stack),
            Event::SequenceStart(..) => {
                if matches!(stack.last(), Some(Some(m)) if m.key_turn) {
                    // Clé complexe : ignorée avec son contenu.
                    stack.push(Some(YamlMap {
                        depth: usize::MAX,
                        key_turn: false,
                        current: None,
                        parent: None,
                    }));
                } else {
                    stack.push(None);
                }
            }
            Event::MappingStart(..) => {
                let complex = matches!(stack.last(), Some(Some(m)) if m.key_turn);
                let depth = if complex {
                    usize::MAX
                } else {
                    stack
                        .iter()
                        .flatten()
                        .filter(|m| m.depth != usize::MAX)
                        .count()
                };
                let parent = stack.iter().rev().flatten().next().and_then(|m| m.current);
                stack.push(Some(YamlMap {
                    depth,
                    key_turn: true,
                    current: None,
                    parent,
                }));
            }
            Event::SequenceEnd | Event::MappingEnd => {
                stack.pop();
                yaml_done(&mut stack);
            }
            _ => {}
        }
    }
    sk
}

fn xml_skeleton(text: &str) -> Skeleton {
    let mut sk = Skeleton::default();
    let mut reader = Reader::from_str(text);
    let mut depth = 0usize;
    let mut current: Option<usize> = None;
    loop {
        let (name, opens) = match reader.read_event() {
            Ok(XmlEvent::Start(e)) => (e.name().as_ref().to_string(), true),
            Ok(XmlEvent::Empty(e)) => (e.name().as_ref().to_string(), false),
            Ok(XmlEvent::End(_)) => {
                depth = depth.saturating_sub(1);
                continue;
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            Ok(_) => continue,
        };
        match depth {
            1 => current = Some(sk.level1(&name)),
            2 => {
                if let Some(p) = current {
                    sk.level2(p, &name);
                }
            }
            _ => {}
        }
        if opens {
            depth += 1;
        }
    }
    sk
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(n: usize) -> String {
        "x".repeat(n)
    }

    /// Squelette d'un texte qui dépasse L grâce à `fill`.
    fn skeleton_of(path: &str, text: &str, max: usize) -> String {
        let out = for_llm(path, text, max).into_owned();
        out.split_once("\n[squelette des clés]\n")
            .unwrap_or_else(|| panic!("{out}"))
            .1
            .to_string()
    }

    /// AC-239-1
    #[test]
    fn req239_ac1_exemple_exact() {
        let text = format!(
            r#"{{"a":[{{"k":1}},{{"l":2}}],"b":{{"x":1,"y":{{"deep":1}}}},"pad":"{}"}}"#,
            pad(32)
        );
        assert_eq!(text.chars().count(), 91);
        assert_eq!(
            for_llm("config/a.json", &text, 60),
            "[extrait tronqué de 1 Ko : début du fichier, puis squelette des clés sur 2 niveaux]\n{\"a\":[{\"k\":1},{\"l\":2}],\"b\":{\"x\":1,\"y\":{\"\n[squelette des clés]\na\n  k\n  l\nb\n  x\n  y\n…"
        );
    }

    /// AC-239-2
    #[test]
    fn req239_ac2_seuils() {
        let json = |n: usize| format!("{{\"a\":\"{}\"}}", pad(n - 8));
        let t = json(6000);
        assert!(matches!(for_llm("a.json", &t, 60_000), Cow::Borrowed(s) if s == t));
        let t = json(6001);
        let out = for_llm("a.json", &t, 60_000).into_owned();
        assert!(out.starts_with("[extrait tronqué de 6 Ko"));
        let head = out.split('\n').nth(1).unwrap();
        assert_eq!(head.chars().count(), 4000);

        let t = format!(
            "{{\"k\":1,{}}}",
            (0..800)
                .map(|i| format!("\"c{i}\":1"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let t = t[..5000.min(t.len())].to_string();
        assert_eq!(t.chars().count(), 5000);
        let out = for_llm("a.json", &t, 1000).into_owned();
        let head = out.split('\n').nth(1).unwrap();
        assert_eq!(head.chars().count(), 666);
        // JSON tronqué : pas de clé, tête seule.
        assert!(!out.contains("[squelette"));

        let big = format!(
            "{{{}}}",
            (0..300)
                .map(|i| format!("\"cle{i}\":1"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let out = for_llm("a.json", &big, 1000).into_owned();
        let sk = out.split_once("[squelette des clés]\n").unwrap().1;
        assert!(sk.ends_with("\n…"), "{sk}");
        let body = sk.strip_suffix("\n…").unwrap();
        assert!(body.chars().count() < 334);

        let accents = format!("{{\"a\":\"{}\"}}", "é".repeat(7000));
        let out = for_llm("a.json", &accents, 60_000).into_owned();
        assert_eq!(out.split('\n').nth(1).unwrap().chars().count(), 4000);
    }

    /// AC-239-3
    #[test]
    fn req239_ac3_squelettes() {
        let t = format!(
            r#"[{{"id":1,"who":{{"name":"a"}}}},{{"id":2,"pad":"{}"}}]"#,
            pad(6100)
        );
        assert_eq!(skeleton_of("a.json", &t, 60_000), "id\nwho\n  name\npad");

        let t = format!(
            "kind: A\nmetadata:\n  name: a\n---\nkind: B\nmetadata:\n  labels:\n    x: 1\n# {}\n",
            pad(200)
        );
        assert_eq!(
            skeleton_of("a.yaml", &t, 90),
            "kind\nmetadata\n  name\n  labels"
        );

        let t = format!("items:\n  - id: 1\n  - name: 2\n# {}\n", pad(200));
        assert_eq!(skeleton_of("a.yml", &t, 90), "items\n  id\n  name");

        let t = format!(
            "<beans><bean id=\"a\"><property/></bean><bean id=\"b\"><constructor-arg/></bean><alias/></beans><!-- {} -->",
            pad(200)
        );
        assert_eq!(
            skeleton_of("beans.xml", &t, 120),
            "bean\n  property\n  constructor-arg\nalias"
        );
    }

    /// AC-239-4
    #[test]
    fn req239_ac4_erreurs_et_budget() {
        let long = pad(200);
        for t in [format!("{{\"a\": {long}"), format!("\"{long}\"")] {
            let out = for_llm("a.json", &t, 90).into_owned();
            assert!(
                out.starts_with("[extrait tronqué de 1 Ko : début du fichier]\n"),
                "{out}"
            );
            assert!(!out.contains("squelette"));
            assert_eq!(out.split('\n').nth(1).unwrap().chars().count(), 60);
        }
        let t = format!("<a><b/><!-- {long} -->");
        assert_eq!(skeleton_of("a.xml", &t, 90), "b");
        let t = format!("k: 1\nm: [\n# {long}\n  : : :\n");
        let out = skeleton_of("a.yaml", &t, 90);
        assert!(out.starts_with("k\nm"), "{out}");

        let many = format!(
            "{{{}}}",
            (0..10_000)
                .map(|i| format!("\"k{i}\":1"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let out = for_llm("a.json", &many, 6000).into_owned();
        let sk = out.split_once("[squelette des clés]\n").unwrap().1;
        assert!(sk.ends_with("\n…"));
        assert!(sk.chars().count() <= 2000 + 2);
    }

    /// AC-239-5
    #[test]
    fn req239_ac5_types() {
        let t = format!("{{\"a\":\"{}\"}}", pad(7000));
        for p in ["pom.xml", "ivy.xml", "package.json", "a.md", "A.java"] {
            assert!(matches!(for_llm(p, &t, 60_000), Cow::Borrowed(_)), "{p}");
            assert!(!never_split(p) || p == "x", "{p}");
        }
        for p in ["beans.xml", "app.yml", "app.yaml", "app.json"] {
            assert!(
                for_llm(p, &t, 60_000).starts_with("[extrait tronqué"),
                "{p}"
            );
            assert!(never_split(p));
        }
    }

    fn ins(n: usize) -> String {
        // INSERT de `n` caractères exactement, saut de ligne final compris.
        let head = "INSERT INTO t VALUES ('";
        format!("{head}{}');\n", "v".repeat(n - head.len() - 4))
    }

    /// Marque SQL pour `k` instructions retirées d'un texte `t`.
    fn mark(t: &str, k: usize) -> String {
        mark_sql(t.len().div_ceil(1024), k)
    }

    /// AC-240-1
    #[test]
    fn req240_ac1_exemple_exact() {
        let big = format!("INSERT INTO big VALUES ('{}');", "v".repeat(2970));
        let text = format!(
            "CREATE TABLE a (id INT);\nINSERT INTO a VALUES (1);\n{big}\nCREATE TABLE b (id INT);\ninsert into b values (2);\n"
        );
        assert_eq!(
            for_llm("db/a.sql", &text, 60_000),
            "-- extrait tronqué de 4 Ko : 2 instructions INSERT ou COPY retirées\nCREATE TABLE a (id INT);\nINSERT INTO a VALUES (1);\nCREATE TABLE b (id INT);\n"
        );
    }

    /// AC-240-2
    #[test]
    fn req240_ac2_seuil() {
        let t = format!("CREATE TABLE a (id INT);\n{}", ins(2000));
        assert!(matches!(for_llm("a.sql", &t, 60_000), Cow::Borrowed(s) if s == t));
        let t = format!("CREATE TABLE a (id INT);\n{}{}", ins(1000), ins(1001));
        let out = for_llm("a.sql", &t, 60_000);
        assert_eq!(
            out,
            format!("{}CREATE TABLE a (id INT);\n{}", mark(&t, 1), ins(1000))
        );
        let ddl = "CREATE TABLE a (id INT);\n".repeat(5000);
        assert!(matches!(for_llm("a.sql", &ddl, 60_000), Cow::Borrowed(_)));
    }

    /// AC-240-3
    #[test]
    fn req240_ac3_copy_stdin() {
        let copy = "COPY a (id, nom) FROM stdin;\n1\tO'Brien\n2\t;INSERT\n\\.\n";
        let t = format!("{}{copy}CREATE TABLE b (id INT);\n", ins(2000));
        assert_eq!(
            for_llm("a.sql", &t, 60_000),
            format!("{}{}CREATE TABLE b (id INT);\n", mark(&t, 1), ins(2000))
        );
        let t = format!("{}{}", ins(2000), copy.replace("\\.\n", ""));
        assert_eq!(
            for_llm("a.sql", &t, 60_000),
            format!("{}{}", mark(&t, 1), ins(2000))
        );
    }

    /// AC-240-4
    #[test]
    fn req240_ac4_jamais_retire() {
        let cases = [
            "CREATE FUNCTION f() RETURNS void AS $$ BEGIN INSERT INTO a VALUES (1); END; $$ LANGUAGE plpgsql;\n",
            "SELECT 'x; INSERT INTO a VALUES (1);';\n",
            "-- INSERT INTO a VALUES (1);\nSELECT 1;\n",
            "/* ; INSERT INTO a VALUES (1); */ SELECT 1;\n",
            "INSERTS INTO a VALUES (1);\n",
        ];
        for case in cases {
            let t = format!("{}{case}", ins(2000));
            assert!(
                matches!(for_llm("a.sql", &t, 60_000), Cow::Borrowed(_)),
                "{case}"
            );
            let t = format!("{}{case}{}", ins(2000), ins(100));
            let out = for_llm("a.sql", &t, 60_000);
            assert!(out.contains(case), "{case}");
            assert!(out.starts_with(&mark(&t, 1)), "{case}");
        }
        let t = format!(
            "{}INSERT INTO a VALUES (1)\nGO\nCREATE TABLE z (id INT);\n",
            ins(2000)
        );
        assert_eq!(
            for_llm("a.sql", &t, 60_000),
            format!("{}{}CREATE TABLE z (id INT);\n", mark(&t, 1), ins(2000))
        );
    }

    /// AC-240-5
    #[test]
    fn req240_ac5_chaine_non_fermee() {
        let t = format!(
            "{}{}INSERT INTO a VALUES ('non fermé\nCREATE TABLE b (id INT);\n",
            ins(2000),
            ins(50)
        );
        assert_eq!(
            for_llm("a.sql", &t, 60_000),
            format!(
                "{}{}INSERT INTO a VALUES ('non fermé\nCREATE TABLE b (id INT);\n",
                mark(&t, 1),
                ins(2000)
            )
        );
    }

    /// AC-240-3 : `stdin` hors d'un COPY n'étend rien.
    #[test]
    fn req240_ac3_stdin_hors_copy() {
        let t = format!(
            "{}INSERT INTO t (stdin) VALUES (1);\nCREATE TABLE b (id INT);\n",
            ins(2000)
        );
        assert_eq!(
            for_llm("a.sql", &t, 60_000),
            format!("{}{}CREATE TABLE b (id INT);\n", mark(&t, 1), ins(2000))
        );
    }

    /// AC-240-1 : saut de ligne CRLF retiré avec l'instruction.
    #[test]
    fn req240_ac1_crlf() {
        let big = ins(1999).replace('\n', "\r\n");
        let t = format!("{big}INSERT INTO a VALUES (1);\r\nCREATE TABLE b (id INT);\r\n");
        assert_eq!(
            for_llm("a.sql", &t, 60_000),
            format!("{}{big}CREATE TABLE b (id INT);\r\n", mark(&t, 1))
        );
    }

    /// AC-240-5 : instruction massive sans terminateur, le reste est gardé.
    #[test]
    fn req240_ac5_sans_terminateur() {
        let t = format!(
            "{}INSERT INTO a VALUES (1)\nCREATE TABLE b (id INT)\n",
            ins(2000)
        );
        assert!(matches!(for_llm("a.sql", &t, 60_000), Cow::Borrowed(_)));
    }
}
