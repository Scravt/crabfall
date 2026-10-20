//! `crabfall claude install`: integra Crabfall con Claude Code en el repo actual (ADR-001).
//!
//! Escribe el skill `crabfall` y agrega a `.claude/settings.json` el hook que bloquea
//! `crabfall run`/`crabfall approve` y los permisos para los comandos seguros.

use std::path::{Path, PathBuf};

use crabfall_core::project::Project;
use serde_json::{Value, json};

use crate::{display, exit};

const SKILL: &str = include_str!("../../../integrations/claude/skills/crabfall/SKILL.md");
const HOOK_COMMAND: &str = "crabfall hook pre-tool-use";
const ALLOWED: &[&str] = &[
    "Bash(crabfall validate:*)",
    "Bash(crabfall check:*)",
    "Bash(crabfall schema)",
    "Bash(crabfall findings:*)",
    "Bash(crabfall compare:*)",
    "Bash(crabfall report:*)",
];

pub fn cmd_install() -> u8 {
    let cwd = std::env::current_dir().unwrap_or_default();
    // La raíz es la carpeta que contiene .crabfall/, o el directorio actual.
    let root = Project::discover(&cwd)
        .and_then(|p| p.dir.parent().map(Path::to_path_buf))
        .unwrap_or(cwd);

    let skill = root.join(".claude/skills/crabfall/SKILL.md");
    let settings = root.join(".claude/settings.json");
    if let Err(e) = write_skill(&skill).and_then(|_| merge_settings(&settings)) {
        eprintln!("✗ {e}");
        return exit::INVALID;
    }

    println!("✓ skill:  {}", display(&skill));
    println!(
        "{}",
        crabfall_core::tr!(
            "✓ hook and permissions: {}",
            "✓ hook y permisos: {}",
            display(&settings)
        )
    );
    if !on_path("crabfall") {
        println!(
            "{}",
            crabfall_core::tr!(
                "WARNING: `crabfall` is not on the PATH. Claude and the hook invoke it as `crabfall`.\n  \
             Install it with `cargo install --path crates/crabfall-cli` from the Crabfall repo.",
                "AVISO: `crabfall` no está en el PATH. Claude y el hook lo invocan como `crabfall`.\n  \
             Instalalo con `cargo install --path crates/crabfall-cli` desde el repo de Crabfall."
            )
        );
    }
    println!(
        "{}",
        crabfall_core::tr!(
            "Open a new Claude Code session in {} and ask it for a load test.",
            "Abrí una sesión nueva de Claude Code en {} y pedile una prueba de carga.",
            root.display()
        )
    );
    exit::OK
}

fn write_skill(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(path, SKILL).map_err(|e| {
        crabfall_core::tr!(
            "could not write {}: {e}",
            "no se pudo escribir {}: {e}",
            display(path)
        )
    })
}

fn merge_settings(path: &Path) -> Result<(), String> {
    let mut settings: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            crabfall_core::tr!(
                "{} is not valid JSON ({e}); leaving it untouched",
                "{} no es JSON válido ({e}); no lo toco",
                display(path)
            )
        })?,
        Err(_) => json!({}),
    };
    add_hook(&mut settings)?;
    add_permissions(&mut settings)?;
    let text = serde_json::to_string_pretty(&settings).unwrap() + "\n";
    std::fs::write(path, text).map_err(|e| {
        crabfall_core::tr!(
            "could not write {}: {e}",
            "no se pudo escribir {}: {e}",
            display(path)
        )
    })
}

fn object<'a>(
    v: &'a mut Value,
    key: &str,
) -> Result<&'a mut serde_json::Map<String, Value>, String> {
    let map = v.as_object_mut().ok_or(crabfall_core::tr!(
        "settings.json is not an object",
        "settings.json no es un objeto"
    ))?;
    map.entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(crabfall_core::tr!(
            "`{key}` is not an object",
            "`{key}` no es un objeto"
        ))
}

fn array<'a>(
    map: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a mut Vec<Value>, String> {
    map.entry(key)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or(crabfall_core::tr!(
            "`{key}` is not a list",
            "`{key}` no es una lista"
        ))
}

fn add_hook(settings: &mut Value) -> Result<(), String> {
    let pre = array(object(settings, "hooks")?, "PreToolUse")?;
    let present = pre.iter().any(|m| {
        m["hooks"]
            .as_array()
            .is_some_and(|hs| hs.iter().any(|h| h["command"] == HOOK_COMMAND))
    });
    if !present {
        pre.push(json!({
            "matcher": "Bash",
            "hooks": [{ "type": "command", "command": HOOK_COMMAND }]
        }));
    }
    Ok(())
}

fn add_permissions(settings: &mut Value) -> Result<(), String> {
    let allow = array(object(settings, "permissions")?, "allow")?;
    for rule in ALLOWED {
        if !allow.iter().any(|r| r == rule) {
            allow.push(json!(rule));
        }
    }
    Ok(())
}

fn on_path(program: &str) -> bool {
    let names = [program.to_string(), format!("{program}.exe")];
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir: PathBuf| names.iter().any(|n| dir.join(n).is_file()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_without_duplicating_or_losing_settings() {
        let mut s = json!({
            "model": "opus",
            "permissions": { "allow": ["Bash(npm test)", "Bash(crabfall check:*)"] },
            "hooks": { "PreToolUse": [{ "matcher": "Edit", "hooks": [] }] }
        });
        for _ in 0..2 {
            add_hook(&mut s).unwrap();
            add_permissions(&mut s).unwrap();
        }
        assert_eq!(s["model"], "opus");
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            s["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
            HOOK_COMMAND
        );
        let allow = s["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 7);
        assert_eq!(allow[0], "Bash(npm test)");
    }
}
