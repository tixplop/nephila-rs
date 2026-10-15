//! Générateur de dépôt synthétique : fichiers Java et Markdown, fixtures du LLM simulé.

use std::fs;
use std::path::Path;

pub const JAVA_FILES: usize = 6_000;
pub const KOTLIN_FILES: usize = 2_000;
pub const CODE_FILES: usize = JAVA_FILES + KOTLIN_FILES;
pub const MD_FILES: usize = 1_400;
pub const POM_FILES: usize = 600;
const PER_PACKAGE: usize = 90;
const METHODS: usize = 10;

pub fn package_of(i: usize) -> String {
    format!("com.gen.p{:03}", i / PER_PACKAGE)
}

pub fn class_name(i: usize) -> String {
    format!("Gen{i:05}")
}

pub fn java_path(i: usize) -> String {
    format!("src/p{:03}/{}.java", i / PER_PACKAGE, class_name(i))
}

fn kotlin_path(i: usize) -> String {
    format!("src/p{:03}/{}.kt", i / PER_PACKAGE, class_name(i))
}

fn md_path(i: usize) -> String {
    format!("docs/d{:02}/Doc{i:04}.md", i / 100)
}

fn pom_path(i: usize) -> String {
    format!("modules/m{i:04}/pom.xml")
}

fn java_source(i: usize) -> String {
    let (pkg, cls) = (package_of(i), class_name(i));
    let next = class_name((i + 1) % CODE_FILES);
    let far = class_name((i * 7 + 13) % CODE_FILES);
    let mut s = format!(
        "package {pkg};\n\npublic class {cls} {{\n    private {next} next;\n    private {far} far;\n"
    );
    for m in 0..METHODS {
        s.push_str(&format!(
            "\n    void run{m}(int x) {{\n        next.run{m}(x);\n        far.run{m}(x + 1);\n    }}\n"
        ));
    }
    s.push_str("}\n");
    s
}

fn kotlin_source(i: usize) -> String {
    let (pkg, cls) = (package_of(i), class_name(i));
    let next = class_name((i + 1) % CODE_FILES);
    let far = class_name((i * 7 + 13) % CODE_FILES);
    let mut s = format!(
        "package {pkg}\n\nclass {cls}(private val next: {next}, private val far: {far}) {{\n"
    );
    for m in 0..METHODS {
        s.push_str(&format!(
            "\n    fun run{m}(x: Int) {{\n        next.run{m}(x)\n        far.run{m}(x + 1)\n    }}\n"
        ));
    }
    s.push_str("}\n");
    s
}

fn md_source(i: usize) -> String {
    let mut s = format!("# Doc {i}\n\nTexte.\n");
    if i.is_multiple_of(2) {
        // La moitié des documents citent des classes du dépôt.
        let c = class_name((i * 5) % CODE_FILES);
        s.push_str(&format!(
            "\nVoir la classe {c} et {}.\n",
            class_name((i * 5 + 3) % CODE_FILES)
        ));
    }
    s
}

/// Mot unique du résumé de la fixture de `java_path(1234)` (retrouvé par `query search`).
pub const SUMMARY_WORD: &str = "zorglubation";

fn code_fixture(i: usize, pkg_cls: &str) -> String {
    let word = if i == 1234 { SUMMARY_WORD } else { "standard" };
    format!(
        r#"{{"version":2,"summaries":[{{"node":"type:{pkg_cls}","summary":"Résumé {word} de la classe numéro {i}"}}],"concepts":[{{"name":"concept{:02}","sections":[]}}]}}"#,
        i % 20
    )
}

fn md_fixture(i: usize) -> String {
    format!(
        r#"{{"version":2,"summaries":[{{"node":"section:{}#doc-{i}","summary":"Document {i} décrivant des classes"}}],"concepts":[{{"name":"concept{:02}","sections":["section:{}#doc-{i}"]}}]}}"#,
        md_path(i),
        i % 20,
        md_path(i)
    )
}

fn pom_source(i: usize) -> String {
    let next = (i + 1) % POM_FILES;
    format!(
        "<project>\n  <groupId>com.gen</groupId>\n  <artifactId>m{i:04}</artifactId>\n  <version>1.0</version>\n  <dependencies>\n    <dependency>\n      <groupId>com.gen</groupId>\n      <artifactId>m{next:04}</artifactId>\n    </dependency>\n    <dependency>\n      <groupId>org.ext</groupId>\n      <artifactId>lib{}</artifactId>\n    </dependency>\n  </dependencies>\n</project>\n",
        i % 50
    )
}

/// Écrit 10 000 fichiers sous `repo` (Java, Kotlin, Markdown, `pom.xml`) ; `mock` reçoit des fixtures
/// d'enrichissement (schéma version 2) pour une partie du dépôt ; la structure vient des sources.
/// Chaque classe a 10 méthodes et 20 appels vers deux autres classes (la suivante et une plus
/// lointaine), de sorte que le graphe est connexe.
pub fn generate(repo: &Path, mock: &Path) {
    fs::create_dir_all(mock).unwrap();
    for i in 0..JAVA_FILES {
        write(&repo.join(java_path(i)), &java_source(i));
    }
    for i in JAVA_FILES..CODE_FILES {
        write(&repo.join(kotlin_path(i)), &kotlin_source(i));
    }
    for i in 0..MD_FILES {
        write(&repo.join(md_path(i)), &md_source(i));
    }
    for i in 0..POM_FILES {
        write(&repo.join(pom_path(i)), &pom_source(i));
    }
    // Fixtures v2 : tous les 10e fichiers de code (dont java_path(1234)) et les Markdown pairs.
    for i in (0..JAVA_FILES).filter(|i| i % 10 == 4 || *i == 1234) {
        let id = format!("{}.{}", package_of(i), class_name(i));
        write(
            &mock.join(format!("{}.json", java_path(i))),
            &code_fixture(i, &id),
        );
    }
    for i in (0..MD_FILES).filter(|i| i % 2 == 0) {
        write(&mock.join(format!("{}.json", md_path(i))), &md_fixture(i));
    }
}

pub fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}
