//! The output language (ADR-016) seen from the binary: English by default, and the order
//! `--lang` > `CRABFALL_LANG` > `language:` in config.yaml.
//!
//! The workspace runs the tests with `CRABFALL_LANG=es` (.cargo/config.toml), so each case
//! sets or removes it explicitly.

use std::{
    path::PathBuf,
    process::{Command, Output},
};

/// A project whose scenario lacks `load.duration`, so `validate` always prints an error.
fn project(name: &str, language: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crabfall-lang-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".crabfall/scenarios")).unwrap();
    let language = language.map_or(String::new(), |l| format!("language: {l}\n"));
    std::fs::write(
        dir.join(".crabfall/config.yaml"),
        format!("version: 1\n{language}environments:\n  local:\n    base_url: http://127.0.0.1:9\n    allowed_hosts: [127.0.0.1]\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join(".crabfall/scenarios/s.yaml"),
        "version: 1\nname: s\nflows:\n  main:\n    steps:\n      - name: a\n        request: { method: GET, url: /x }\n\
         load:\n  executor: constant_rate\n  rate: 1/s\n  max_vus: 1\n",
    )
    .unwrap();
    dir
}

fn validate(dir: &PathBuf, env: Option<&str>, extra: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_crabfall"));
    cmd.arg("validate").arg("s").args(extra).current_dir(dir);
    match env {
        Some(v) => cmd.env("CRABFALL_LANG", v),
        None => cmd.env_remove("CRABFALL_LANG"),
    };
    out(&cmd.output().unwrap())
}

fn out(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const ENGLISH: &str = "`duration` is missing (required by constant_rate)";
const SPANISH: &str = "falta `duration` (obligatorio en constant_rate)";

#[test]
fn english_is_the_default() {
    let dir = project("default", None);
    let o = validate(&dir, None, &[]);
    assert!(o.contains(ENGLISH), "{o}");
}

#[test]
fn flag_beats_env_and_env_beats_config() {
    let dir = project("order", Some("es"));
    // Only config.yaml: Spanish.
    assert!(validate(&dir, None, &[]).contains(SPANISH));
    // The environment variable beats the config.
    assert!(validate(&dir, Some("en"), &[]).contains(ENGLISH));
    // The flag beats both, before or after the subcommand.
    assert!(validate(&dir, Some("es"), &["--lang", "en"]).contains(ENGLISH));
    assert!(validate(&dir, None, &["--lang=en"]).contains(ENGLISH));
}

#[test]
fn help_follows_the_language() {
    let help = |lang: &str| {
        let o = Command::new(env!("CARGO_BIN_EXE_crabfall"))
            .args(["--lang", lang, "run", "--help"])
            .env_remove("CRABFALL_LANG")
            .output()
            .unwrap();
        out(&o)
    };
    assert!(help("en").contains("Runs an approved scenario and saves the results"));
    assert!(help("en").contains("Tweaks (override the scenario)"));
    assert!(help("es").contains("Ejecuta un escenario aprobado y guarda los resultados"));
    assert!(help("es").contains("Ajustes (pisan al escenario)"));
}

#[test]
fn unknown_language_is_rejected() {
    let dir = project("unknown", None);
    let o = validate(&dir, None, &["--lang", "fr"]);
    assert!(o.contains("unknown language `fr`"), "{o}");
}
