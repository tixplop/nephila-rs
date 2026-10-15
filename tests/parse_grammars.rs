//! Essai des grammaires tree-sitter (T-022) : chargement et analyse sans nœud ERROR.

use std::path::Path;

use rkg::parse::{extract, FileStructure, Lang, ParseCtx};

fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/langs")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn parse_clean(name: &str) {
    let lang = Lang::from_path(Path::new(name)).expect("langage reconnu");
    let tree = lang.parse(&fixture(name)).expect("analyse");
    let root = tree.root_node();
    assert!(
        !root.has_error(),
        "{name}: nœud ERROR ou MISSING dans l'arbre : {}",
        root.to_sexp()
    );
}

#[test]
fn grammaire_java21() {
    parse_clean("Sample21.java");
}

#[test]
fn grammaire_kotlin() {
    parse_clean("Sample.kt");
}

#[test]
fn grammaire_scala2() {
    parse_clean("Scala2.scala");
}

#[test]
fn grammaire_scala3() {
    parse_clean("Scala3.scala");
}

#[test]
fn grammaire_groovy() {
    parse_clean("Sample.groovy");
}

/// Limite connue, sans heuristique (DEC-016) : `tree-sitter-groovy` 0.1 ignore `trait`.
/// `trait Greeter { ... }` est lu comme un appel de commande `trait Greeter` suivi d'une
/// fermeture, et les méthodes du corps produisent des nœuds ERROR. Ce test sert de témoin :
/// il échoue si une version future de la grammaire reconnaît `trait`.
#[test]
fn grammaire_groovy_trait_limite_connue() {
    let text = fixture("Trait.groovy");
    let tree = Lang::Groovy.parse(&text).expect("analyse");
    let root = tree.root_node();
    assert!(
        root.has_error(),
        "la grammaire gère désormais trait : {}",
        root.to_sexp()
    );
    let mut cur = root.walk();
    let call = root
        .children(&mut cur)
        .find(|n| n.kind() == "juxt_function_call")
        .expect("`trait Greeter` lu comme appel de commande");
    let name = call.child_by_field_name("name").unwrap();
    assert_eq!(&text[name.byte_range()], "trait");
    assert_eq!(
        &text[call.child_by_field_name("args").unwrap().byte_range()],
        "Greeter"
    );
}

#[test]
fn grammaire_build_gradle() {
    parse_clean("build.gradle");
}

#[test]
fn grammaire_build_gradle_kts() {
    parse_clean("build.gradle.kts");
}

#[test]
fn broken_java_donne_un_arbre_partiel() {
    let text = fixture("Broken.java");
    let tree = Lang::Java.parse(&text).expect("analyse");
    let root = tree.root_node();
    assert!(root.has_error());
    let mut cur = root.walk();
    let class = root
        .children(&mut cur)
        .find(|n| n.kind() == "class_declaration")
        .expect("la classe est reconnue malgré l'erreur");
    let body = class.child_by_field_name("body").expect("corps");
    let mut cur = body.walk();
    let first = body
        .named_children(&mut cur)
        .find(|n| n.kind() == "method_declaration")
        .expect("méthode ok() reconnue");
    let name = first.child_by_field_name("name").unwrap();
    assert_eq!(&text[name.byte_range()], "ok");
}

#[test]
fn extract_renvoie_une_structure_vide_pour_chaque_langage() {
    for name in [
        "Sample21.java",
        "Sample.kt",
        "Scala3.scala",
        "Sample.groovy",
        "build.gradle",
        "build.gradle.kts",
        "pom.xml",
    ] {
        let s = extract(Path::new(name), "", &ParseCtx::default()).unwrap();
        assert_eq!(s, FileStructure::default(), "{name}");
    }
}
