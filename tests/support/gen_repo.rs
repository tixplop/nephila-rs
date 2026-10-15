//! Générateur de dépôt synthétique : fichiers Java et Markdown, fixtures du LLM simulé.

use std::fs;
use std::path::Path;

pub const JAVA_FILES: usize = 9_000;
pub const MD_FILES: usize = 1_000;
const PER_PACKAGE: usize = 90;

pub fn package_of(i: usize) -> String {
    format!("com.gen.p{:03}", i / PER_PACKAGE)
}

pub fn class_name(i: usize) -> String {
    format!("Gen{i:05}")
}

pub fn java_path(i: usize) -> String {
    format!("src/p{:03}/{}.java", i / PER_PACKAGE, class_name(i))
}

fn md_path(i: usize) -> String {
    format!("docs/d{:02}/Doc{i:04}.md", i / 100)
}

/// Écrit 10 000 fichiers sous `repo` ; `mock` (fixtures d'enrichissement, schéma version 2) reste
/// vide : la structure vient de l'analyse des sources. Chaque classe en appelle deux autres (la
/// suivante et une plus lointaine), de sorte que le graphe est connexe.
pub fn generate(repo: &Path, mock: &Path) {
    fs::create_dir_all(mock).unwrap();
    for i in 0..JAVA_FILES {
        let (pkg, cls, path) = (package_of(i), class_name(i), java_path(i));
        let next = (i + 1) % JAVA_FILES;
        let far = (i * 7 + 13) % JAVA_FILES;
        write(
            &repo.join(&path),
            &format!(
                "package {pkg};\n\npublic class {cls} {{\n    private {n} next;\n    private {f} far;\n\n    void run() {{\n        next.run();\n        far.run();\n    }}\n}}\n",
                n = class_name(next),
                f = class_name(far),
            ),
        );
    }
    for i in 0..MD_FILES {
        let path = md_path(i);
        write(&repo.join(&path), &format!("# Doc {i}\n\nTexte.\n"));
    }
}

pub fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}
