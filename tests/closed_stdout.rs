//! Sortie standard fermée par le lecteur, pour toutes les commandes (REQ-086, DEC-095).

mod support;

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

#[cfg(target_os = "linux")]
const E17: &str = "erreur: écriture sur stdout impossible : Broken pipe (os error 32)";
#[cfg(not(target_os = "linux"))]
const E17: &str = "erreur: écriture sur stdout impossible : ";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    /// Dépôt sans graphe : `files` classes Java reliées entre elles (`method0` à `method7`).
    fn unbuilt(files: usize) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("monrepo");
        for i in 0..files {
            let dir = repo.join(format!("src/p{}", i % 20));
            fs::create_dir_all(&dir).unwrap();
            let mut src = format!("package p{};\n\npublic class C{i} {{\n", i % 20);
            for m in 0..8 {
                src.push_str(&format!(
                    "    public int method{m}() {{ Ext{i}.go(); return new C{}().method{}(); }}\n",
                    (i + 1) % files,
                    (m + 1) % 8
                ));
            }
            src.push_str("}\n");
            fs::write(dir.join(format!("C{i}.java")), src).unwrap();
        }
        fs::write(repo.join("NOTES.md"), "# Notes\n\nTexte.\n").unwrap();
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&mock).unwrap();
        fs::write(
            mock.join("_ask.json"),
            r#"{"found":true,"answer":"Réponse.","nodes":["p0.C0","p0.C0.method0"]}"#,
        )
        .unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        Env {
            _tmp: tmp,
            repo,
            mock,
            config,
        }
    }

    fn built(files: usize) -> Env {
        let env = Env::unbuilt(files);
        let o = env.run(&["build", "--no-llm"]);
        assert!(o.status.success(), "{}", text(&o.stderr));
        env
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env_remove("NEPHILA_MOCK_FAIL_AFTER");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Lance la commande avec une sortie standard dont le lecteur est déjà fermé.
    fn run_closed(&self, args: &[&str]) -> Output {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        self.command(args)
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    }

    fn export(&self) -> String {
        let o = self.run(&["export", "--output", "-"]);
        assert!(o.status.success(), "{}", text(&o.stderr));
        support::normalized_export(&o.stdout)
    }

    fn modify(&self) {
        let p = self.repo.join("src/p0/C0.java");
        let mut s = fs::read_to_string(&p).unwrap();
        s.push_str("// modifié\n");
        fs::write(p, s).unwrap();
    }
}

fn assert_up_to_date(env: &Env) {
    let o = env.run(&["status"]);
    assert_eq!(text(&o.stdout).lines().next(), Some("graphe à jour"));
}

fn assert_no_panic(label: &str, o: &Output) {
    assert_eq!(o.status.code(), Some(1), "{label} : {}", text(&o.stderr));
    let err = text(&o.stderr);
    for banned in ["panicked", "thread '", "\u{1b}"] {
        assert!(!err.contains(banned), "{label} : {err:?}");
    }
}

/// Code 1 et une seule ligne `erreur:` exacte sur stderr (AC-086-1, AC-086-2, AC-086-6).
fn assert_e17(label: &str, o: &Output) {
    assert_no_panic(label, o);
    let err = text(&o.stderr);
    assert!(err.starts_with(E17), "{label} : {err:?}");
    assert_eq!(err.matches('\n').count(), 1, "{label} : {err:?}");
    #[cfg(target_os = "linux")]
    assert_eq!(err, format!("{E17}\n"), "{label}");
    for banned in ["bilan", "EPIPE", "SIGPIPE"] {
        assert!(!err.contains(banned), "{label} : {err:?}");
    }
}

#[test]
fn req086_ac1_search_closed_after_10_bytes() {
    let env = Env::built(800);
    let full = env.run(&["query", "search", "method", "--limit", "100000"]);
    assert!(full.status.success());
    assert!(full.stdout.len() > 256 * 1024, "{}", full.stdout.len());
    let mut child = env
        .command(&["query", "search", "method", "--limit", "100000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut head = vec![0u8; 10];
    stdout.read_exact(&mut head).unwrap();
    drop(stdout);
    let out = child.wait_with_output().unwrap();
    assert_eq!(head, full.stdout[..10]);
    assert_e17("search | head -c 10", &out);
}

#[test]
fn req086_ac2_ac6_every_command_stops_on_one_error_line() {
    let env = Env::built(3);
    let cmds: &[&[&str]] = &[
        &["status"],
        &["status", "--json"],
        &["summary"],
        &["summary", "--json"],
        &["query", "search", "method"],
        &["query", "search", "method", "--json"],
        &["query", "neighbors", "p0.C0"],
        &["query", "neighbors", "p0.C0", "--json"],
        &["query", "path", "p0.C0", "p0.C0.method0"],
        &["query", "path", "p0.C0", "p0.C0.method0", "--json"],
        &["query", "ask", "Que fait method0 ?"],
        &["query", "ask", "Que fait method0 ?", "--json"],
    ];
    // `status` n'écrit que `graphe à jour` ; un graphe en retard ajoute des lignes.
    for args in cmds {
        let open = env.run(args);
        assert!(open.status.success(), "{args:?} : {}", text(&open.stderr));
        assert!(!open.stdout.is_empty(), "{args:?}");
        assert_e17(&args.join(" "), &env.run_closed(args));
    }
    assert_up_to_date(&env);
}

#[test]
fn req086_ac2_build_closed_keeps_the_graph() {
    let closed = Env::built(3);
    let open = Env::built(3);
    let o = closed.run_closed(&["build", "--no-llm"]);
    assert_e17("build", &o);
    open.run(&["build", "--no-llm"]);
    assert_up_to_date(&closed);
    assert_eq!(closed.export(), open.export());

    // Premier build, sans graphe : le graphe est écrit avant le bilan.
    let fresh = Env::unbuilt(3);
    assert_e17("build initial", &fresh.run_closed(&["build", "--no-llm"]));
    let reference = Env::built(3);
    assert_up_to_date(&fresh);
    assert_eq!(fresh.export(), reference.export());
}

#[test]
fn req086_ac2_update_closed_keeps_the_graph() {
    let env = Env::built(3);
    assert_e17("update inchangé", &env.run_closed(&["update", "--no-llm"]));
    assert_up_to_date(&env);

    let closed = Env::built(3);
    let open = Env::built(3);
    closed.modify();
    open.modify();
    assert_e17("update", &closed.run_closed(&["update", "--no-llm"]));
    assert!(open.run(&["update", "--no-llm"]).status.success());
    assert_up_to_date(&closed);
    assert_eq!(closed.export(), open.export());
}

#[test]
fn req086_ac2_ac9_export_closed_keeps_the_complete_file() {
    let env = Env::built(3);
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.json");
    let o = env.run_closed(&["export", "--output", f.to_str().unwrap()]);
    assert_e17("export --output f.json", &o);
    assert!(!text(&o.stderr).contains("écriture de"));
    let expected = env.run(&["export", "--output", "-"]).stdout;
    assert_eq!(fs::read(&f).unwrap(), expected);
}

#[test]
fn req086_ac7_commands_without_stdout_output_keep_their_message() {
    let env = Env::built(3);
    let args = ["query", "search", "terme-absent-du-graphe"];
    let open = env.run(&args);
    let closed = env.run_closed(&args);
    assert_eq!(open.status.code(), Some(1));
    assert_eq!(closed.status.code(), open.status.code());
    assert_eq!(closed.stderr, open.stderr);
    assert!(!text(&closed.stderr).contains("écriture sur stdout"));

    let empty = Env::unbuilt(1);
    let open = empty.run(&["status"]);
    let closed = empty.run_closed(&["status"]);
    assert_eq!(open.status.code(), Some(1));
    assert_eq!(closed.status.code(), open.status.code());
    assert_eq!(closed.stderr, open.stderr);
    assert!(!text(&closed.stderr).contains("écriture sur stdout"));
}

#[test]
fn req086_ac8_lines_written_before_the_failure_are_kept() {
    let env = Env::built(3);
    env.modify();
    let o = env.run_closed(&["query", "search", "method", "--check-stale"]);
    assert_no_panic("check-stale", &o);
    let lines: Vec<String> = text(&o.stderr).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        lines[0],
        "avertissement: graphe pas à jour : 1 fichier modifié"
    );
    assert!(lines[1].starts_with(E17), "{lines:?}");

    let open_env = Env::unbuilt(6);
    let closed_env = Env::unbuilt(6);
    let open = open_env.run(&["build", "--no-llm", "--show-progress"]);
    assert!(open.status.success());
    let progress = |o: &Output| -> Vec<String> {
        text(&o.stderr)
            .lines()
            .filter(|l| l.starts_with("progression:"))
            .map(str::to_string)
            .collect()
    };
    let o = closed_env.run_closed(&["build", "--no-llm", "--show-progress"]);
    assert_no_panic("build progress", &o);
    let err = text(&o.stderr);
    let all: Vec<&str> = err.lines().collect();
    assert!(all.last().unwrap().starts_with(E17), "{err:?}");
    assert_eq!(
        all.iter().filter(|l| l.starts_with("erreur:")).count(),
        1,
        "{err:?}"
    );
    assert!(!progress(&open).is_empty());
    assert_eq!(progress(&o), progress(&open));
}

#[test]
fn req086_ac9_nothing_is_written_after_the_error() {
    let env = Env::unbuilt(3);
    let o = env.run_closed(&["build", "--no-llm", "--exclude", "*.md"]);
    assert_no_panic("build --exclude", &o);
    let err = text(&o.stderr);
    assert!(err.lines().last().unwrap().starts_with(E17), "{err:?}");
    assert!(
        !err.lines()
            .any(|l| l.starts_with("avertissement: --exclude et --exclude-type")),
        "{err:?}"
    );
    // Le graphe est enregistré malgré l'échec du bilan.
    assert!(env.run(&["status"]).status.success());
}
