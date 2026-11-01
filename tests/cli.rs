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

// ---- REQ-184, REQ-185 : `--dry-run` de `build` et `update` ----

#[test]
fn req184_ac1_dry_run_with_no_llm_is_a_usage_error() {
    for cmd in ["build", "update"] {
        for args in [["--dry-run", "--no-llm"], ["--no-llm", "--dry-run"]] {
            let dir = tempfile::tempdir().unwrap();
            let out = nephila()
                .arg("-C")
                .arg(dir.path())
                .arg(cmd)
                .args(args)
                .assert()
                .failure()
                .code(2)
                .get_output()
                .clone();
            assert!(out.stdout.is_empty(), "{cmd}");
            let err = String::from_utf8(out.stderr).unwrap();
            assert!(
                err.contains("error: the argument '--dry-run' cannot be used with '--no-llm'")
                    || err
                        .contains("error: the argument '--no-llm' cannot be used with '--dry-run'"),
                "{cmd} : {err}"
            );
            assert!(!dir.path().join(".nephila").exists(), "{cmd}");
        }
    }
}

#[test]
fn req184_ac1_dry_run_alone_is_accepted() {
    for cmd in ["build", "update"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# A\n").unwrap();
        nephila()
            .env("NEPHILA_CONFIG", dir.path().join("absent.toml"))
            .arg("-C")
            .arg(dir.path())
            .args([cmd, "--dry-run"])
            .assert()
            .success();
    }
}

#[test]
fn req185_ac1_dry_run_help_of_build_and_update() {
    let want = "--dry-run Estime les appels, octets et jetons de l'enrichissement, sans rien envoyer au LLM (graphe écrit comme avec --no-llm ; incompatible avec --no-llm)";
    for cmd in ["build", "update"] {
        let text = normalized_help(&[cmd]);
        assert!(text.contains(want), "{cmd} : {text}");
        let at = |s: &str| text.find(s).unwrap();
        assert!(at("--enrich-code") < at("--dry-run"), "{cmd}");
        assert!(at("--dry-run") < at("-v, --verbose"), "{cmd}");
    }
}

#[test]
fn req184_dry_run_sans_cle_ecrit_bilan_puis_estimation_sur_stdout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n\ntexte\n").unwrap();
    let conf = dir.path().join("c.toml");
    std::fs::write(
        &conf,
        "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"http://127.0.0.1:1/v1\"\napi_key_env = \"NEPHILA_CLE_ABSENTE_184\"\n",
    )
    .unwrap();
    let out = nephila()
        .env("NEPHILA_CONFIG", &conf)
        .env_remove("NEPHILA_CLE_ABSENTE_184")
        .arg("-C")
        .arg(dir.path())
        .args(["build", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].ends_with("enrichissement : estimé sans envoi"),
        "{text}"
    );
    assert_eq!(
        lines[1],
        "estimation : 1 fichiers à enrichir dont 0 envoyés seuls et 0 sans appel"
    );
    assert!(
        lines[2].starts_with("estimation minimale : 1 appels, "),
        "{text}"
    );
    assert!(lines[3].ends_with("(3 essais par appel)"), "{text}");
    assert_eq!(lines.len(), 4, "{text}");
}
