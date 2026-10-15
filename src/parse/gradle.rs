//! Scripts Gradle (REQ-022, DEC-025 A) : chemin de projet des `build.gradle(.kts)`, dépendances du bloc
//! `dependencies { }` de premier niveau et `rootProject.name` des `settings.gradle(.kts)`, lus par le lecteur
//! lexical commun à Groovy et à Kotlin.

use std::path::Path;

use super::gradle_lex::{lex, Tok, Token};
use super::{FileStructure, ParseCtx};
use crate::model::{EdgeKind, NodeKind};
use crate::parse::{Declared, Reference};

/// Dossier (séparateur `/`, `""` = racine du dépôt) du fichier `path`.
fn dir_of(path: &Path) -> String {
    let parts: Vec<String> = path
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    parts.join("/")
}

fn warning(path: &Path, problem: &str) -> String {
    format!(
        "{} : script Gradle incomplet, lu au mieux : {problem}",
        path.display()
    )
}

fn is_punct(t: &Token, c: char) -> bool {
    t.tok == Tok::Punct(c)
}

/// Indice du jeton qui ferme la parenthèse ouverte en `open`.
fn matching_paren(toks: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate().skip(open) {
        if is_punct(t, '(') {
            depth += 1;
        } else if is_punct(t, ')') {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Découpe des jetons aux virgules de premier niveau (parenthèses, crochets et accolades), fins de ligne écartées.
fn split_args(toks: &[Token]) -> Vec<Vec<Token>> {
    let mut args = vec![Vec::new()];
    let mut depth = 0i32;
    for t in toks {
        match &t.tok {
            Tok::Newline => continue,
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth -= 1,
            Tok::Punct(',') if depth == 0 => {
                args.push(Vec::new());
                continue;
            }
            _ => {}
        }
        if let Some(a) = args.last_mut() {
            a.push(t.clone());
        }
    }
    args.retain(|a| !a.is_empty());
    args
}

/// `g:a` si `s` a au moins deux segments dont les deux premiers sont littéraux.
fn coordinate(s: &str) -> Option<String> {
    let mut parts = s.split(':');
    let (g, a) = (parts.next()?, parts.next()?);
    if g.is_empty() || a.is_empty() || g.contains('$') || a.contains('$') {
        return None;
    }
    Some(format!("{g}:{a}"))
}

/// Chemin de projet Gradle absolu (`:a:b`) ; les chemins relatifs sont ignorés.
fn project_path(s: &str) -> Option<String> {
    if !s.starts_with(':') || s.contains('$') {
        return None;
    }
    Some(s.to_string())
}

/// Argument nommé `nom (:|=) CHAÎNE` : renvoie le nom et la valeur.
fn named_string(arg: &[Token]) -> Option<(&str, &str)> {
    match arg {
        [Token {
            tok: Tok::Ident(n), ..
        }, Token {
            tok: Tok::Punct(':' | '='),
            ..
        }, Token {
            tok: Tok::Str(v), ..
        }] => Some((n, v)),
        _ => None,
    }
}

/// Cible d'un argument de dépendance ; `named` collecte les arguments `group` et `name`.
fn argument_target(arg: &[Token], named: &mut (Option<String>, Option<String>)) -> Option<String> {
    if let [Token {
        tok: Tok::Str(s), ..
    }] = arg
    {
        return coordinate(s);
    }
    if let Some((n, v)) = named_string(arg) {
        match n {
            "group" => named.0 = Some(v.to_string()),
            "name" => named.1 = Some(v.to_string()),
            _ => {}
        }
        return None;
    }
    let [Token {
        tok: Tok::Ident(f), ..
    }, open, inner @ .., close] = arg
    else {
        return None;
    };
    if !is_punct(open, '(')
        || !is_punct(close, ')')
        || matching_paren(arg, 1) != Some(arg.len() - 1)
    {
        return None;
    }
    match f.as_str() {
        "project" => {
            let args = split_args(inner);
            args.iter()
                .find_map(|a| match a.as_slice() {
                    [Token {
                        tok: Tok::Str(s), ..
                    }] => Some(s.as_str()),
                    _ => named_string(a)
                        .filter(|(n, _)| *n == "path")
                        .map(|(_, v)| v),
                })
                .and_then(project_path)
        }
        "platform" | "enforcedPlatform" => {
            let args = split_args(inner);
            match args.as_slice() {
                [one] => argument_target(one, &mut (None, None)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Cibles d'une instruction du bloc `dependencies { }` : `IDENT ( args ) [{ … }]` ou `IDENT args`.
fn statement_targets(stmt: &[Token]) -> Vec<String> {
    let [Token {
        tok: Tok::Ident(_), ..
    }, rest @ ..] = stmt
    else {
        return Vec::new();
    };
    let args_toks: &[Token] = match rest.first() {
        Some(t) if is_punct(t, '(') => match matching_paren(rest, 0) {
            Some(close) => &rest[1..close],
            None => &rest[1..],
        },
        _ => rest,
    };
    let mut named = (None, None);
    let mut out: Vec<String> = split_args(args_toks)
        .iter()
        .filter_map(|a| argument_target(a, &mut named))
        .collect();
    if let (Some(g), Some(a)) = named {
        if !g.is_empty() && !a.is_empty() && !g.contains('$') && !a.contains('$') {
            out.push(format!("{g}:{a}"));
        }
    }
    out
}

/// Cibles des dépendances du bloc `dependencies { }` de premier niveau ; `false` si un bloc n'est pas refermé.
fn dependencies(toks: &[Token]) -> (Vec<String>, bool) {
    let mut targets = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if depth == 0 && t.tok == Tok::Ident("dependencies".into()) {
            let mut j = i + 1;
            while toks.get(j).is_some_and(|n| n.tok == Tok::Newline) {
                j += 1;
            }
            if toks.get(j).is_some_and(|n| is_punct(n, '{')) {
                // Corps : jetons de profondeur 1 relative, les accolades imbriquées (fermetures) étant sautées.
                let mut stmt: Vec<Token> = Vec::new();
                let mut inner = 1usize;
                let mut parens = 0i32;
                let mut closed = false;
                let mut k = j + 1;
                while k < toks.len() {
                    let t = &toks[k];
                    k += 1;
                    match &t.tok {
                        Tok::Punct('{') => {
                            inner += 1;
                            continue;
                        }
                        Tok::Punct('}') => {
                            inner -= 1;
                            if inner == 0 {
                                closed = true;
                                break;
                            }
                            continue;
                        }
                        _ if inner > 1 => continue,
                        Tok::Punct('(' | '[') => parens += 1,
                        Tok::Punct(')' | ']') => parens -= 1,
                        _ => {}
                    }
                    let ends = parens <= 0
                        && match &t.tok {
                            Tok::Punct(';') => true,
                            Tok::Newline => !stmt.last().is_some_and(|l| is_punct(l, ',')),
                            _ => false,
                        };
                    if ends {
                        targets.extend(statement_targets(&stmt));
                        stmt.clear();
                    } else if t.tok != Tok::Newline {
                        stmt.push(t.clone());
                    }
                }
                targets.extend(statement_targets(&stmt));
                if !closed {
                    return (targets, false);
                }
                i = k;
                continue;
            }
        }
        match t.tok {
            Tok::Punct('{') => depth += 1,
            Tok::Punct('}') => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    (targets, true)
}

/// `rootProject.name = CHAÎNE` en instruction de premier niveau d'un settings (la dernière l'emporte).
pub fn root_project_name(text: &str) -> Option<String> {
    let toks = lex(text).tokens;
    root_name_of(&toks)
}

fn root_name_of(toks: &[Token]) -> Option<String> {
    let mut depth = 0usize;
    let mut found = None;
    for (i, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Punct('{') => depth += 1,
            Tok::Punct('}') => depth = depth.saturating_sub(1),
            Tok::Ident(n) if depth == 0 && n == "rootProject" => {
                let starts = i == 0 || matches!(toks[i - 1].tok, Tok::Newline | Tok::Punct(';'));
                if let (
                    true,
                    [_, d, name, eq, Token {
                        tok: Tok::Str(v), ..
                    }, ..],
                ) = (starts, &toks[i..])
                {
                    if is_punct(d, '.')
                        && name.tok == Tok::Ident("name".into())
                        && is_punct(eq, '=')
                        && !v.contains('$')
                    {
                        found = Some(v.clone());
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// Extrait un script `build.gradle(.kts)` ou `settings.gradle(.kts)`.
pub fn extract(path: &Path, text: &str, ctx: &ParseCtx) -> FileStructure {
    let lexed = lex(text);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with("settings.gradle") {
        let mut out = FileStructure::default();
        if lexed.unterminated {
            out.warnings
                .push(warning(path, "chaîne ou commentaire non fermé"));
        }
        return out;
    }
    let mut out = module_structure(path, ctx);
    let (targets, closed) = dependencies(&lexed.tokens);
    if lexed.unterminated {
        out.warnings
            .push(warning(path, "chaîne ou commentaire non fermé"));
    } else if !closed {
        out.warnings
            .push(warning(path, "bloc dependencies non fermé"));
    }
    if let Some(d) = out.declares.first() {
        let source = d.id();
        for target in targets {
            let r = Reference {
                source: source.clone(),
                kind: EdgeKind::DependsOn,
                target,
            };
            if !out.references.contains(&r) {
                out.references.push(r);
            }
        }
    }
    out
}

/// Module déclaré par un script de build, d'après son dossier et les settings du contexte.
fn module_structure(path: &Path, ctx: &ParseCtx) -> FileStructure {
    let dir = dir_of(path);
    // Plus proche ancêtre (dossier du script compris) contenant un settings, sinon la racine du dépôt.
    let mut base = dir.as_str();
    let base = loop {
        if ctx.gradle_settings.contains_key(base) || base.is_empty() {
            break base;
        }
        base = base.rsplit_once('/').map_or("", |(d, _)| d);
    };
    let rel = dir
        .strip_prefix(base)
        .map_or("", |r| r.trim_start_matches('/'));
    let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let (qualified, short) = if segments.is_empty() {
        let own = ctx.gradle_settings.get(base).and_then(|n| n.clone());
        let folder = if base.is_empty() {
            ctx.repo_root
                .as_deref()
                .and_then(|r| r.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        } else {
            base.rsplit('/').next().map(str::to_string)
        };
        // Aucun nom de dossier (racine `/`) : le module est tout de même déclaré, sous le nom `:`.
        let short = own
            .filter(|n| !n.is_empty())
            .or(folder)
            .unwrap_or_else(|| ":".to_string());
        (":".to_string(), short)
    } else {
        let last = segments[segments.len() - 1].to_string();
        (format!(":{}", segments.join(":")), last)
    };
    FileStructure {
        declares: vec![Declared {
            kind: NodeKind::Module,
            qualified,
            subkind: None,
            summary: None,
            short: Some(short),
        }],
        ..FileStructure::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn ctx(settings: &[(&str, Option<&str>)]) -> ParseCtx {
        ParseCtx {
            repo_root: Some(PathBuf::from("/work/monrepo")),
            gradle_settings: settings
                .iter()
                .map(|(d, n)| (d.to_string(), n.map(str::to_string)))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn decl(path: &str, c: &ParseCtx) -> (String, Option<String>) {
        let f = super::super::extract(Path::new(path), "", c).unwrap();
        assert_eq!(f.declares.len(), 1, "{path}");
        assert!(f.references.is_empty());
        (f.declares[0].qualified.clone(), f.declares[0].short.clone())
    }

    #[test]
    fn sous_projet_sans_settings() {
        let (q, s) = decl("services/api/build.gradle", &ctx(&[]));
        assert_eq!((q.as_str(), s.as_deref()), (":services:api", Some("api")));
        let (q, _) = decl("a/build.gradle.kts", &ctx(&[("", None)]));
        assert_eq!(q, ":a");
    }

    #[test]
    fn settings_imbrique() {
        let c = ctx(&[("", None), ("tools", None)]);
        let (q, s) = decl("tools/x/build.gradle", &c);
        assert_eq!((q.as_str(), s.as_deref()), (":x", Some("x")));
        let (q, s) = decl("tools/build.gradle", &c);
        assert_eq!((q.as_str(), s.as_deref()), (":", Some("tools")));
    }

    #[test]
    fn racine_nom_du_contexte_puis_nom_du_dossier() {
        let (q, s) = decl("build.gradle", &ctx(&[("", Some("mon-projet"))]));
        assert_eq!((q.as_str(), s.as_deref()), (":", Some("mon-projet")));
        let (q, s) = decl("build.gradle.kts", &ctx(&[("", None)]));
        assert_eq!((q.as_str(), s.as_deref()), (":", Some("monrepo")));
        let (_, s) = decl("build.gradle", &ctx(&[]));
        assert_eq!(s.as_deref(), Some("monrepo"));
    }

    #[test]
    fn racine_sans_nom_de_dossier_garde_le_module() {
        for root in [Some(PathBuf::from("/")), None] {
            let mut c = ctx(&[]);
            c.repo_root = root;
            let f = super::super::extract(
                Path::new("build.gradle"),
                "dependencies { implementation 'org.slf4j:slf4j-api:2.0.9' }",
                &c,
            )
            .unwrap();
            assert_eq!(f.declares.len(), 1);
            assert_eq!(f.declares[0].qualified, ":");
            assert_eq!(f.declares[0].short.as_deref(), Some(":"));
            assert_eq!(f.references.len(), 1, "{:?}", f.references);
        }
    }

    fn build(path: &str, text: &str) -> FileStructure {
        super::super::extract(Path::new(path), text, &ctx(&[("", Some("root"))])).unwrap()
    }

    fn targets(f: &FileStructure) -> Vec<&str> {
        f.references.iter().map(|r| r.target.as_str()).collect()
    }

    #[test]
    fn ac_022_4_groovy() {
        let f = build(
            "api/build.gradle",
            "dependencies {\n  implementation project(':core')\n  implementation 'org.slf4j:slf4j-api:2.0.9'\n  testImplementation group: 'junit', name: 'junit', version: '4.13'\n}\n",
        );
        assert_eq!(targets(&f), [":core", "org.slf4j:slf4j-api", "junit:junit"]);
        assert_eq!(f.references[0].source, "module::api");
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn root_project_path_is_a_target() {
        let f = build(
            "api/build.gradle",
            "dependencies {\n  implementation project(':')\n}\n",
        );
        assert_eq!(targets(&f), [":"]);
    }

    #[test]
    fn relative_project_path_is_ignored() {
        let f = build(
            "api/build.gradle",
            "dependencies {\n  implementation project('core')\n}\n",
        );
        assert!(targets(&f).is_empty());
    }

    #[test]
    fn ac_022_5_kotlin() {
        let f = build(
            "api/build.gradle.kts",
            "dependencies {\n  implementation(project(\":core\"))\n  implementation(\"org.slf4j:slf4j-api:2.0.9\")\n  testImplementation(group = \"junit\", name = \"junit\", version = \"4.13\")\n}\n",
        );
        assert_eq!(targets(&f), [":core", "org.slf4j:slf4j-api", "junit:junit"]);
    }

    #[test]
    fn ac_022_9() {
        let f = build(
            "build.gradle",
            "buildscript { dependencies { classpath 'com.acme:plugin:1' } }\ndependencies {\n implementation project(path: ':core')\n implementation platform('org.springframework.boot:spring-boot-dependencies:3.3.0')\n implementation libs.foo\n implementation files('x.jar')\n implementation \"org.slf4j:slf4j-api:$slf4jVersion\"\n}\n",
        );
        assert_eq!(
            targets(&f),
            [
                ":core",
                "org.springframework.boot:spring-boot-dependencies",
                "org.slf4j:slf4j-api"
            ]
        );
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn interpolation_non_litterale_et_autres_arguments_ignores() {
        let f = build(
            "build.gradle.kts",
            "dependencies {\n implementation(\"$g:a:1\")\n implementation(\"g:$a:1\")\n implementation(kotlin(\"stdlib\"))\n implementation(fileTree(\"libs\"))\n implementation(v)\n api(\"solo\")\n implementation(project(path = \":p\", configuration = \"x\"))\n implementation(enforcedPlatform(\"a:b:1\"))\n}",
        );
        assert_eq!(targets(&f), [":p", "a:b"]);
    }

    #[test]
    fn commentaires_et_chaines_avec_accolade() {
        let f = build(
            "build.gradle",
            "dependencies {\n // }\n /* } */\n implementation 'a:b:1' // }\n implementation \"c:d:}\"\n}\nimplementation 'x:y:1'\n",
        );
        assert_eq!(targets(&f), ["a:b", "c:d"]);
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn fermeture_finale_et_instructions_sur_plusieurs_lignes() {
        let f = build(
            "build.gradle",
            "dependencies {\n implementation('g:a:1') { exclude group: 'x' }\n implementation 'e:f:1', 'h:i:1'\n implementation project(':a'),\n   project(':b')\n api 'm:n:1'; api 'o:p:1'\n implementation(\n  'q:r:1'\n )\n constraints { implementation 'no:pe:1' }\n}",
        );
        assert_eq!(
            targets(&f),
            ["g:a", "e:f", "h:i", ":a", ":b", "m:n", "o:p", "q:r"]
        );
    }

    #[test]
    fn chaine_non_terminee() {
        let f = build(
            "api/build.gradle",
            "dependencies {\n implementation 'a:b:1'\n implementation 'c:d",
        );
        assert_eq!(targets(&f), ["a:b"]);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("api/build.gradle"));
    }

    #[test]
    fn bloc_non_ferme() {
        let f = build("build.gradle", "dependencies {\n implementation 'a:b:1'\n");
        assert_eq!(targets(&f), ["a:b"]);
        assert_eq!(f.warnings.len(), 1);
    }

    #[test]
    fn root_project_name_groovy_et_kotlin() {
        assert_eq!(
            root_project_name("rootProject.name = 'mon-app'\ninclude 'a'"),
            Some("mon-app".into())
        );
        assert_eq!(
            root_project_name("// c\nrootProject.name = \"kt\"\n"),
            Some("kt".into())
        );
        assert_eq!(root_project_name("rootProject.name = \"a$b\""), None);
        assert_eq!(root_project_name("rootProject.name = x"), None);
        assert_eq!(
            root_project_name("pluginManagement { rootProject.name = 'no' }"),
            None
        );
    }

    #[test]
    fn root_project_name_devient_le_nom_court_de_la_racine() {
        let name = root_project_name("rootProject.name = 'app'");
        let c = ctx(&[("", name.as_deref())]);
        let (q, s) = decl("build.gradle.kts", &c);
        assert_eq!((q.as_str(), s.as_deref()), (":", Some("app")));
    }

    #[test]
    fn settings_ne_declare_rien() {
        let c = ctx(&[("", Some("x"))]);
        for p in [
            "settings.gradle",
            "settings.gradle.kts",
            "a/settings.gradle",
        ] {
            let f = super::super::extract(Path::new(p), "rootProject.name = 'x'", &c).unwrap();
            assert_eq!(f, FileStructure::default(), "{p}");
        }
    }

    #[test]
    fn autre_gradle_reste_groovy() {
        let c = ctx(&[]);
        let f = super::super::extract(Path::new("gradle/deps.gradle"), "class A {}\n", &c).unwrap();
        assert!(f.declares.iter().any(|d| d.kind != NodeKind::Module));
    }
}
