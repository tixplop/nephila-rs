//! Extraction d'un `pom.xml` Maven par lecture XML en flux (DEC-018, DEC-020).

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::FileStructure;
use crate::model::{EdgeKind, NodeKind};
use crate::parse::{Declared, OrderedSet, Reference};

/// Événement XML visité : le chemin d'éléments courant (noms locaux, depuis la racine) est emprunté,
/// jamais copié (DEC-106).
pub(crate) enum Ev<'a> {
    Open(&'a [String], Vec<(String, String)>),
    Text(&'a [String], String),
}

/// Lit `text` en flux et appelle `visit` pour chaque événement. Renvoie le message de l'erreur
/// éventuelle (XML mal formé ou document tronqué) ; les événements lus avant elle ont été visités.
pub(crate) fn read_events(text: &str, mut visit: impl FnMut(Ev<'_>)) -> Option<String> {
    let mut reader = Reader::from_str(text);
    let mut path: Vec<String> = Vec::new();
    let mut problem = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                path.push(local(e.name().as_ref()));
                visit(Ev::Open(&path, attrs(&e)));
            }
            Ok(Event::Empty(e)) => {
                path.push(local(e.name().as_ref()));
                visit(Ev::Open(&path, attrs(&e)));
                path.pop();
            }
            Ok(Event::End(_)) => {
                path.pop();
            }
            Ok(Event::Text(t)) => visit(Ev::Text(&path, t.xml10_content().into_owned())),
            Ok(Event::CData(t)) => visit(Ev::Text(&path, t.xml10_content().into_owned())),
            Ok(Event::GeneralRef(r)) => {
                let s = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => match r.xml10_content().as_ref() {
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "amp" => "&".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        other => format!("&{other};"),
                    },
                };
                visit(Ev::Text(&path, s));
            }
            Ok(Event::Eof) => {
                if let Some(open) = path.last() {
                    problem = Some(format!("document tronqué (élément <{open}> non fermé)"));
                }
                break;
            }
            Ok(_) => {}
            Err(e) => {
                problem = Some(format!("{e} (position {})", reader.error_position()));
                break;
            }
        }
    }
    problem
}

fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or("").to_string()
}

fn attrs(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .filter_map(|a| {
            let v = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()?
                .into_owned();
            Some((local(a.key.as_ref()), v))
        })
        .collect()
}

pub(crate) fn warning(path: &Path, problem: &str) -> String {
    format!(
        "{} : XML mal formé, lu au mieux : {problem}",
        path.display()
    )
}

fn is(p: &[String], expected: &[&str]) -> bool {
    p.len() == expected.len() && p.iter().zip(expected).all(|(a, b)| a == b)
}

#[derive(Default)]
struct Dep {
    group: Option<String>,
    artifact: Option<String>,
}

/// Extrait le module et les dépendances d'un pom.
pub fn extract(path: &Path, text: &str) -> FileStructure {
    let mut group = None;
    let mut parent_group = None;
    let mut artifact = None;
    let mut props: HashMap<String, String> = HashMap::new();
    let mut deps: Vec<Dep> = Vec::new();
    let problem = read_events(text, |ev| match ev {
        Ev::Open(p, _) if is(p, &["project", "dependencies", "dependency"]) => {
            deps.push(Dep::default())
        }
        Ev::Text(p, t) => {
            let t = t.trim();
            if t.is_empty() && !p.is_empty() {
                return;
            }
            if is(p, &["project", "groupId"]) {
                append(&mut group, t);
            } else if is(p, &["project", "artifactId"]) {
                append(&mut artifact, t);
            } else if is(p, &["project", "parent", "groupId"]) {
                append(&mut parent_group, t);
            } else if p.len() == 3 && p[0] == "project" && p[1] == "properties" {
                props.entry(p[2].clone()).or_default().push_str(t);
            } else if p.len() == 4
                && p[0] == "project"
                && p[1] == "dependencies"
                && p[2] == "dependency"
            {
                if let Some(d) = deps.last_mut() {
                    match p[3].as_str() {
                        "groupId" => append(&mut d.group, t),
                        "artifactId" => append(&mut d.artifact, t),
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    });
    let mut out = FileStructure::default();
    if let Some(problem) = problem {
        out.warnings.push(warning(path, &problem));
    }
    let Some(artifact) = artifact else {
        return out;
    };
    let vars = |s: &str, group: &Option<String>| -> String {
        let mut cur = s.to_string();
        for _ in 0..5 {
            let next = substitute(
                &cur,
                &props,
                group.as_deref(),
                parent_group.as_deref(),
                &artifact,
            );
            if next == cur {
                break;
            }
            cur = next;
        }
        cur
    };
    let own_group = group
        .as_deref()
        .or(parent_group.as_deref())
        .map(|g| g.to_string());
    let own_group = own_group.map(|g| vars(&g, &None));
    let short = vars(&artifact, &own_group);
    let qualified = match &own_group {
        Some(g) if !g.is_empty() => format!("{g}:{short}"),
        _ => short.clone(),
    };
    let source = format!("module:{qualified}");
    out.declares.push(Declared {
        kind: NodeKind::Module,
        qualified,
        subkind: None,
        summary: None,
        short: Some(short),
    });
    let mut refs = OrderedSet::new();
    for d in &deps {
        let (Some(g), Some(a)) = (&d.group, &d.artifact) else {
            continue;
        };
        let target = format!("{}:{}", vars(g, &own_group), vars(a, &own_group));
        let r = Reference {
            source: source.clone(),
            kind: EdgeKind::DependsOn,
            target,
        };
        refs.insert(r);
    }
    out.references = refs.into_vec();
    out
}

fn append(slot: &mut Option<String>, t: &str) {
    slot.get_or_insert_with(String::new).push_str(t);
}

/// Remplace `${project.groupId}`, `${project.parent.groupId}`, `${project.artifactId}` et les
/// propriétés du pom ; les autres `${…}` restent tels quels.
fn substitute(
    s: &str,
    props: &HashMap<String, String>,
    group: Option<&str>,
    parent_group: Option<&str>,
    artifact: &str,
) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let key = &after[..j];
        let value = match key {
            "project.groupId" => group.or(parent_group).map(str::to_string),
            "project.parent.groupId" => parent_group.map(str::to_string),
            "project.artifactId" => Some(artifact.to_string()),
            k => props.get(k).cloned(),
        };
        match value {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[i..i + 2 + j + 1]),
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> FileStructure {
        extract(Path::new("api/pom.xml"), text)
    }

    fn targets(f: &FileStructure) -> Vec<&str> {
        f.references.iter().map(|r| r.target.as_str()).collect()
    }

    #[test]
    fn req022_ac1_module_et_dependance_interne() {
        let f = run("<project><groupId>com.acme</groupId><artifactId>api</artifactId>\
            <dependencies><dependency><groupId>com.acme</groupId><artifactId>core</artifactId></dependency></dependencies></project>");
        assert_eq!(f.declares.len(), 1);
        assert_eq!(f.declares[0].id(), "module:com.acme:api");
        assert_eq!(f.declares[0].short.as_deref(), Some("api"));
        assert_eq!(f.references[0].source, "module:com.acme:api");
        assert_eq!(f.references[0].kind, EdgeKind::DependsOn);
        assert_eq!(targets(&f), ["com.acme:core"]);
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn req022_ac2_dependance_externe_tous_scopes() {
        let f = run("<project><groupId>g</groupId><artifactId>a</artifactId><dependencies>\
            <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency>\
            <dependency><groupId>junit</groupId><artifactId>junit</artifactId><scope>test</scope></dependency>\
            </dependencies></project>");
        assert_eq!(targets(&f), ["org.slf4j:slf4j-api", "junit:junit"]);
    }

    #[test]
    fn req022_ac3_groupe_herite_substitutions_et_exclusions() {
        let f = run("<project><parent><groupId>com.acme</groupId><artifactId>parent</artifactId></parent>\
            <artifactId>api</artifactId>\
            <dependencyManagement><dependencies><dependency><groupId>x</groupId><artifactId>managed</artifactId></dependency></dependencies></dependencyManagement>\
            <build><plugins><plugin><groupId>p</groupId><artifactId>plug</artifactId>\
            <dependencies><dependency><groupId>q</groupId><artifactId>pdep</artifactId></dependency></dependencies></plugin></plugins></build>\
            <properties><lib.group>org.lib</lib.group></properties>\
            <dependencies>\
            <dependency><groupId>${project.groupId}</groupId><artifactId>core</artifactId></dependency>\
            <dependency><groupId>${project.parent.groupId}</groupId><artifactId>${project.artifactId}-x</artifactId></dependency>\
            <dependency><groupId>${lib.group}</groupId><artifactId>lib</artifactId></dependency>\
            <dependency><groupId>${unknown}</groupId><artifactId>u</artifactId></dependency>\
            </dependencies></project>");
        assert_eq!(f.declares[0].id(), "module:com.acme:api");
        assert_eq!(f.declares[0].short.as_deref(), Some("api"));
        assert_eq!(
            targets(&f),
            [
                "com.acme:core",
                "com.acme:api-x",
                "org.lib:lib",
                "${unknown}:u"
            ]
        );
    }

    #[test]
    fn req022_ac7_pom_tronque_garde_ce_qui_precede() {
        let f = run("<project><groupId>com.acme</groupId><artifactId>api</artifactId>\
            <dependencies><dependency><groupId>a</groupId><artifactId>b</artifactId></dependency></dependencies>");
        assert_eq!(f.declares[0].id(), "module:com.acme:api");
        assert_eq!(targets(&f), ["a:b"]);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("api/pom.xml"), "{:?}", f.warnings);
    }

    #[test]
    fn xml_mal_forme_garde_le_debut() {
        let f = run("<project><artifactId>a</artifactId></wrong>");
        assert_eq!(f.declares[0].id(), "module:a");
        assert_eq!(f.warnings.len(), 1);
    }

    #[test]
    fn sans_artifact_id_pas_de_module() {
        let f = run("<project><groupId>g</groupId></project>");
        assert!(f.declares.is_empty() && f.references.is_empty() && f.warnings.is_empty());
    }

    #[test]
    fn entites_et_cdata() {
        let f =
            run("<project><groupId>g</groupId><artifactId><![CDATA[a]]></artifactId></project>");
        assert_eq!(f.declares[0].id(), "module:g:a");
    }

    /// Lecture et extracteurs de la 0.24.0 (HEAD avant T-193), oracle d'AC-105-2.
    mod oracle {
        #![allow(dead_code)]
        use super::super::{append, attrs, is, local, substitute, warning, Dep};
        use crate::model::{EdgeKind, NodeKind};
        use crate::parse::{Declared, FileStructure, OrderedSet, Reference};
        use quick_xml::events::Event;
        use quick_xml::reader::Reader;
        use std::collections::HashMap;
        use std::path::Path;

        /// Événement XML simplifié : chemin d'éléments (noms locaux) depuis la racine.
        pub(crate) enum XmlEv {
            Open(Vec<String>, Vec<(String, String)>),
            Text(Vec<String>, String),
        }

        /// Lit `text` en flux. Renvoie les événements lus avant une éventuelle erreur, et le message de
        /// l'erreur (XML mal formé ou document tronqué).
        pub(crate) fn read_events(text: &str) -> (Vec<XmlEv>, Option<String>) {
            let mut reader = Reader::from_str(text);
            let mut events = Vec::new();
            let mut path: Vec<String> = Vec::new();
            let mut problem = None;
            loop {
                match reader.read_event() {
                    Ok(Event::Start(e)) => {
                        path.push(local(e.name().as_ref()));
                        events.push(XmlEv::Open(path.clone(), attrs(&e)));
                    }
                    Ok(Event::Empty(e)) => {
                        let mut p = path.clone();
                        p.push(local(e.name().as_ref()));
                        events.push(XmlEv::Open(p, attrs(&e)));
                    }
                    Ok(Event::End(_)) => {
                        path.pop();
                    }
                    Ok(Event::Text(t)) => {
                        events.push(XmlEv::Text(path.clone(), t.xml10_content().into_owned()))
                    }
                    Ok(Event::CData(t)) => {
                        events.push(XmlEv::Text(path.clone(), t.xml10_content().into_owned()))
                    }
                    Ok(Event::GeneralRef(r)) => {
                        let s = match r.resolve_char_ref() {
                            Ok(Some(c)) => c.to_string(),
                            _ => match r.xml10_content().as_ref() {
                                "lt" => "<".into(),
                                "gt" => ">".into(),
                                "amp" => "&".into(),
                                "quot" => "\"".into(),
                                "apos" => "'".into(),
                                other => format!("&{other};"),
                            },
                        };
                        events.push(XmlEv::Text(path.clone(), s));
                    }
                    Ok(Event::Eof) => {
                        if let Some(open) = path.last() {
                            problem =
                                Some(format!("document tronqué (élément <{open}> non fermé)"));
                        }
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        problem = Some(format!("{e} (position {})", reader.error_position()));
                        break;
                    }
                }
            }
            (events, problem)
        }

        /// Extrait le module et les dépendances d'un pom.
        pub fn maven_extract(path: &Path, text: &str) -> FileStructure {
            let (events, problem) = read_events(text);
            let mut group = None;
            let mut parent_group = None;
            let mut artifact = None;
            let mut props: HashMap<String, String> = HashMap::new();
            let mut deps: Vec<Dep> = Vec::new();
            for ev in &events {
                match ev {
                    XmlEv::Open(p, _) if is(p, &["project", "dependencies", "dependency"]) => {
                        deps.push(Dep::default())
                    }
                    XmlEv::Text(p, t) => {
                        let t = t.trim();
                        if t.is_empty() && !p.is_empty() {
                            continue;
                        }
                        if is(p, &["project", "groupId"]) {
                            append(&mut group, t);
                        } else if is(p, &["project", "artifactId"]) {
                            append(&mut artifact, t);
                        } else if is(p, &["project", "parent", "groupId"]) {
                            append(&mut parent_group, t);
                        } else if p.len() == 3 && p[0] == "project" && p[1] == "properties" {
                            props.entry(p[2].clone()).or_default().push_str(t);
                        } else if p.len() == 4
                            && p[0] == "project"
                            && p[1] == "dependencies"
                            && p[2] == "dependency"
                        {
                            if let Some(d) = deps.last_mut() {
                                match p[3].as_str() {
                                    "groupId" => append(&mut d.group, t),
                                    "artifactId" => append(&mut d.artifact, t),
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut out = FileStructure::default();
            if let Some(problem) = problem {
                out.warnings.push(warning(path, &problem));
            }
            let Some(artifact) = artifact else {
                return out;
            };
            let vars = |s: &str, group: &Option<String>| -> String {
                let mut cur = s.to_string();
                for _ in 0..5 {
                    let next = substitute(
                        &cur,
                        &props,
                        group.as_deref(),
                        parent_group.as_deref(),
                        &artifact,
                    );
                    if next == cur {
                        break;
                    }
                    cur = next;
                }
                cur
            };
            let own_group = group
                .as_deref()
                .or(parent_group.as_deref())
                .map(|g| g.to_string());
            let own_group = own_group.map(|g| vars(&g, &None));
            let short = vars(&artifact, &own_group);
            let qualified = match &own_group {
                Some(g) if !g.is_empty() => format!("{g}:{short}"),
                _ => short.clone(),
            };
            let source = format!("module:{qualified}");
            out.declares.push(Declared {
                kind: NodeKind::Module,
                qualified,
                subkind: None,
                summary: None,
                short: Some(short),
            });
            let mut refs = OrderedSet::new();
            for d in &deps {
                let (Some(g), Some(a)) = (&d.group, &d.artifact) else {
                    continue;
                };
                let target = format!("{}:{}", vars(g, &own_group), vars(a, &own_group));
                let r = Reference {
                    source: source.clone(),
                    kind: EdgeKind::DependsOn,
                    target,
                };
                refs.insert(r);
            }
            out.references = refs.into_vec();
            out
        }

        fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
            attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .filter(|v| !v.is_empty())
        }
        /// Extrait le module (`<info>`) et ses dépendances.
        pub fn ivy_extract(path: &Path, text: &str) -> FileStructure {
            let (events, problem) = read_events(text);
            let mut out = FileStructure::default();
            let mut module: Option<String> = None;
            let mut org: Option<String> = None;
            let mut deps: Vec<(Option<String>, String)> = Vec::new();
            for ev in &events {
                let XmlEv::Open(p, a) = ev else { continue };
                let p: Vec<&str> = p.iter().map(String::as_str).collect();
                match p.as_slice() {
                    ["ivy-module", "info"] => {
                        org = attr(a, "organisation").map(str::to_string);
                        module = attr(a, "module").map(str::to_string);
                    }
                    ["ivy-module", "dependencies", "dependency"] => {
                        if let Some(name) = attr(a, "name") {
                            deps.push((attr(a, "org").map(str::to_string), name.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            if let Some(problem) = problem {
                out.warnings.push(warning(path, &problem));
            }
            let Some(module) = module else { return out };
            let qualified = match &org {
                Some(o) => format!("{o}:{module}"),
                None => module.clone(),
            };
            let source = format!("module:{qualified}");
            out.declares.push(Declared {
                kind: NodeKind::Module,
                qualified,
                subkind: None,
                summary: None,
                short: Some(module),
            });
            let mut refs = OrderedSet::new();
            for (o, name) in deps {
                let target = match o.as_deref().or(org.as_deref()) {
                    Some(o) => format!("{o}:{name}"),
                    None => name,
                };
                let r = Reference {
                    source: source.clone(),
                    kind: EdgeKind::DependsOn,
                    target,
                };
                refs.insert(r);
            }
            out.references = refs.into_vec();
            out
        }
    }

    fn ivy_run(text: &str) -> FileStructure {
        crate::parse::ivy::extract(Path::new("ivy.xml"), text)
    }

    fn assert_same(label: &str, text: &str) {
        let p = Path::new("api/pom.xml");
        assert_eq!(
            extract(p, text),
            oracle::maven_extract(p, text),
            "maven {label}"
        );
        let p = Path::new("ivy.xml");
        assert_eq!(
            crate::parse::ivy::extract(p, text),
            oracle::ivy_extract(p, text),
            "ivy {label}"
        );
    }

    fn nested(open: &str, close: &str, n: usize, pre: &str, post: &str) -> String {
        format!("{pre}{}{}{post}", open.repeat(n), close.repeat(n))
    }

    #[test]
    fn req105_ac1_pom_20000_niveaux_sous_une_seconde() {
        let pom = nested(
            "<x>\n",
            "</x>\n",
            20_000,
            "<project><groupId>g</groupId><artifactId>a</artifactId>",
            "<dependencies><dependency><groupId>d</groupId><artifactId>e</artifactId></dependency></dependencies></project>",
        );
        let t = std::time::Instant::now();
        let f = run(&pom);
        let d = t.elapsed();
        assert!(d < std::time::Duration::from_secs(1), "{d:?}");
        assert_eq!(f.declares[0].id(), "module:g:a");
        assert_eq!(targets(&f), ["d:e"]);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    }

    #[test]
    fn req105_ac1_ivy_20000_niveaux_sous_une_seconde() {
        let ivy = nested(
            "<x>\n",
            "</x>\n",
            20_000,
            "<ivy-module><info organisation=\"o\" module=\"m\"/>",
            "<dependencies><dependency org=\"p\" name=\"q\"/></dependencies></ivy-module>",
        );
        let t = std::time::Instant::now();
        let f = ivy_run(&ivy);
        let d = t.elapsed();
        assert!(d < std::time::Duration::from_secs(1), "{d:?}");
        assert_eq!(f.declares[0].id(), "module:o:m");
        assert_eq!(targets(&f), ["p:q"]);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    }

    #[test]
    fn req105_ac2_documents_ecrits_egaux_a_l_oracle() {
        let docs: Vec<(&str, String)> = vec![
            ("vide", String::new()),
            ("texte seul", "bonjour".into()),
            (
                "prefixes",
                "<m:project xmlns:m=\"u\"><m:groupId>g</m:groupId><m:artifactId>a</m:artifactId>\
                 <m:dependencies><m:dependency><m:groupId>d</m:groupId><m:artifactId>e</m:artifactId></m:dependency></m:dependencies></m:project>".into(),
            ),
            (
                "elements vides",
                "<project><groupId/><artifactId>a</artifactId><dependencies><dependency/><dependency><groupId>d</groupId><artifactId/></dependency></dependencies></project>".into(),
            ),
            (
                "cdata",
                "<project><groupId><![CDATA[g]]>x</groupId><artifactId>a<![CDATA[b]]></artifactId></project>".into(),
            ),
            (
                "entites",
                "<project><groupId>g&amp;h&lt;&gt;&quot;&apos;&#65;&#x42;&inconnue;</groupId><artifactId>a</artifactId></project>".into(),
            ),
            (
                "tronque",
                "<project><groupId>g</groupId><artifactId>a</artifactId><dependencies><dependency><groupId>d".into(),
            ),
            (
                "mal forme",
                "<project><groupId>g</groupId><artifactId>a</wrong></project>".into(),
            ),
            (
                "commentaire",
                "<project><groupId>g<!-- c -->h</groupId><artifactId>a<!--x-->b</artifactId></project>".into(),
            ),
            (
                "proprietes",
                "<project><parent><groupId>pg</groupId></parent><artifactId>${n}-x</artifactId>\
                 <properties><n>nom</n><lib.g>org.lib</lib.g><v> 1 </v></properties>\
                 <dependencies><dependency><groupId>${lib.g}</groupId><artifactId>${project.artifactId}</artifactId></dependency>\
                 <dependency><groupId>${project.groupId}</groupId><artifactId>${zz}</artifactId></dependency></dependencies></project>".into(),
            ),
            (
                "ivy",
                "<ivy-module><info organisation=\"o\" module=\"m\"/><dependencies>\
                 <dependency org=\"p\" name=\"q\"/><dependency name=\"r\"/><dependency org=\"z\"/>\
                 <dependency org=\"p\" name=\"q\"/></dependencies></ivy-module>".into(),
            ),
            (
                "ivy sans org, tronque",
                "<ivy-module><info module=\"m\"/><dependencies><dependency name=\"r\" xml:x=\"1\"/>".into(),
            ),
            (
                "ivy attributs mal formes",
                "<ivy-module><info organisation=\"o\" module=\"m\" bad/><dependencies></ivy-module>".into(),
            ),
            (
                "profond",
                nested("<x>\n", "</x>\n", 300, "<project><groupId>g</groupId><artifactId>a</artifactId>", "</project>"),
            ),
            (
                "profond tronque",
                nested("<x>", "", 300, "<project><artifactId>a</artifactId>", ""),
            ),
        ];
        for (label, doc) in &docs {
            assert_same(label, doc);
        }
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut seen = 0;
        for rel in ["ivy/ivy.xml", "maven2/api/pom.xml", "maven2/core/pom.xml"] {
            if let Ok(doc) = std::fs::read_to_string(fixtures.join(rel)) {
                assert_same(rel, &doc);
                seen += 1;
            }
        }
        assert!(seen >= 1, "aucune fixture lue");
    }
}
