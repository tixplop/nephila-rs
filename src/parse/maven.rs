//! Extraction d'un `pom.xml` Maven par lecture XML en flux (DEC-018, DEC-020).

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::FileStructure;
use crate::model::{EdgeKind, NodeKind};
use crate::parse::{Declared, Reference};

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
    (events, problem)
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
        if !out.references.contains(&r) {
            out.references.push(r);
        }
    }
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
}
