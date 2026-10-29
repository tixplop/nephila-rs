use assert_cmd::Command;

fn nephila() -> Command {
    Command::cargo_bin("nephila").unwrap()
}

#[test]
fn req001_ac1_help_lists_commands() {
    let out = nephila()
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(out.stdout).unwrap();
    for c in ["build", "update", "status", "query", "export", "summary"] {
        assert!(text.contains(c), "commande absente de l'aide : {c}");
    }
    assert!(text.contains("--repo"));
}

#[test]
fn req001_ac1_query_help_lists_subcommands() {
    let out = nephila()
        .args(["query", "--help"])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(out.stdout).unwrap();
    for c in ["search", "neighbors", "path", "community", "ask"] {
        assert!(text.contains(c), "sous-commande absente : {c}");
    }
}

#[test]
fn req001_ac3_unknown_command_fails_with_usage_on_stderr() {
    let out = nephila()
        .arg("frobnicate")
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains("Usage"));
}

#[test]
fn req001_ac3_invalid_option_fails_with_usage_on_stderr() {
    let out = nephila()
        .args(["status", "--bogus"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains("Usage"));
}

#[test]
fn req001_ac2_error_exits_nonzero_with_prefix_on_stderr_only() {
    let out = nephila()
        .args(["-C", "/nonexistent/nephila-repo"])
        .arg("build")
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .starts_with("erreur:"));
}

// ---- Option --check-stale (REQ-017, AC-017-7, AC-017-8) ----

const CHECK_STALE_HELP: &str = "--check-stale Avertit sur stderr si le graphe n'est pas à jour, d'après la taille et la date de modification des fichiers (parcourt le dépôt sans lire les fichiers déjà indexés ; défaut : aucune vérification ; vérification par le contenu : nephila status)";

fn normalized_help(args: &[&str]) -> String {
    let out = nephila().args(args).arg("--help").assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn req017_ac7_ac8_check_stale_help_of_query_subcommands() {
    for sub in ["search", "neighbors", "path", "community", "ask"] {
        let h = normalized_help(&["query", sub]);
        assert!(h.contains(CHECK_STALE_HELP), "{sub} : {h}");
        let from = h.find("--check-stale").unwrap();
        let entry_end = from + CHECK_STALE_HELP.len();
        assert!(!h[from..entry_end + 20.min(h.len() - entry_end)].contains("[default:"));
        assert!(h.find("--json").unwrap() < from, "{sub}");
        if sub == "ask" {
            assert!(from < h.find("--format-retries").unwrap());
        }
    }
}

#[test]
fn req017_ac7_check_stale_absent_from_other_commands() {
    for cmd in ["status", "build", "update", "export", "summary"] {
        assert!(!normalized_help(&[cmd]).contains("--check-stale"), "{cmd}");
    }
}

// ---- REQ-163 : `--context-chars` et `--verbose` de `query ask` ----

#[test]
fn req163_ac5_ac8_context_chars_usage_errors() {
    for v in ["999", "0", "-1", "1.5", "abc"] {
        let out = nephila()
            .args(["query", "ask", "x", "--context-chars", v])
            .assert()
            .failure()
            .code(2)
            .get_output()
            .clone();
        assert!(out.stdout.is_empty(), "{v}");
        let err = String::from_utf8(out.stderr).unwrap();
        let want = format!(
            "error: invalid value '{v}' for '--context-chars <N>': `{v}` n'est pas un entier d'au moins 1000"
        );
        assert!(err.contains(&want), "{v} : {err}");
    }
    let out = nephila()
        .args(["query", "search", "x", "--context-chars", "2000"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("unexpected argument '--context-chars'"),
        "{err}"
    );
}

#[test]
fn req163_ac6_ac10_help_of_query_ask() {
    let h = normalized_help(&["query", "ask"]);
    let verbose = "-v, --verbose Configuration effective de l'appel LLM sur stderr, en une ligne (préfixe `trace:`)";
    let chars = "--context-chars <N> Nombre maximal de caractères du contexte envoyé au LLM, au moins 1000 (remplace `max_context_chars` de la section [ask] ; défaut 12000)";
    assert!(h.contains(verbose), "{h}");
    assert!(h.contains(chars), "{h}");
    let pos = |s: &str| h.find(s).unwrap_or_else(|| panic!("{s} : {h}"));
    let order = [
        pos("--json"),
        pos("--check-stale"),
        pos("-v, --verbose"),
        pos("--format-retries <N>"),
        pos("--context-chars <N>"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} : {h}");
    assert!(!h[pos("--json")..].contains("[default:"), "{h}");
    for bad in ["thinking", "effort", "max_output_tokens", "incompatible"] {
        assert!(!h.contains(bad), "{bad} : {h}");
    }
    for sub in ["search", "neighbors", "path", "community"] {
        assert!(
            !normalized_help(&["query", sub]).contains("--context-chars"),
            "{sub}"
        );
    }
}
