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

/// Écrit 10 000 fichiers sous `repo` et leurs fixtures sous `mock`. Chaque classe en appelle deux
/// autres (la suivante et une plus lointaine), de sorte que le graphe est connexe.
pub fn generate(repo: &Path, mock: &Path) {
    for i in 0..JAVA_FILES {
        let (pkg, cls, path) = (package_of(i), class_name(i), java_path(i));
        write(
            &repo.join(&path),
            &format!("package {pkg};\npublic class {cls} {{ void run() {{}} }}\n"),
        );
        let next = (i + 1) % JAVA_FILES;
        let far = (i * 7 + 13) % JAVA_FILES;
        let json = format!(
            concat!(
                "{{\"version\":1,\"declares\":[",
                "{{\"kind\":\"package\",\"name\":\"{pkg}\"}},",
                "{{\"kind\":\"type\",\"name\":\"{pkg}.{cls}\",\"subkind\":\"class\"}},",
                "{{\"kind\":\"method\",\"name\":\"{pkg}.{cls}.run\"}}],",
                "\"references\":[",
                "{{\"source\":\"method:{pkg}.{cls}.run\",\"kind\":\"calls\",\"target\":\"{n}.run\"}},",
                "{{\"source\":\"method:{pkg}.{cls}.run\",\"kind\":\"calls\",\"target\":\"{f}.run\"}}]}}"
            ),
            pkg = pkg,
            cls = cls,
            n = class_name(next),
            f = class_name(far)
        );
        write(&mock.join(format!("{path}.json")), &json);
    }
    for i in 0..MD_FILES {
        let path = md_path(i);
        write(&repo.join(&path), &format!("# Doc {i}\n\nTexte.\n"));
        let json = format!(
            "{{\"version\":1,\"declares\":[{{\"kind\":\"document\",\"name\":\"{path}\"}},{{\"kind\":\"section\",\"name\":\"{path}#doc-{i}\"}}],\"references\":[]}}"
        );
        write(&mock.join(format!("{path}.json")), &json);
    }
}

pub fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}
