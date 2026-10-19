//! El README no se queda atrás: menciona cada comando de `crabfall` y enlaza cada ADR.

use std::{path::Path, process::Command};

fn readme() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md")).unwrap()
}

#[test]
fn readme_mentions_every_command() {
    let help = Command::new(env!("CARGO_BIN_EXE_crabfall"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    let commands: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|c| *c != "help")
        .collect();
    assert!(commands.len() >= 8, "no se pudo leer --help: {help}");

    let readme = readme();
    let missing: Vec<_> = commands
        .iter()
        .filter(|c| !readme.contains(&format!("crabfall {c}")))
        .collect();
    assert!(missing.is_empty(), "el README no menciona: {missing:?}");
}

#[test]
fn readme_links_every_adr() {
    let readme = readme();
    let adrs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/adr");
    let missing: Vec<String> = std::fs::read_dir(adrs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !readme.contains(&format!("docs/adr/{name}")))
        .collect();
    assert!(missing.is_empty(), "el README no enlaza: {missing:?}");
}
