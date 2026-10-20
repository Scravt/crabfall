//! `crabfall baseline`: fija la corrida base de un escenario en un entorno (ADR-006). Va en
//! `.crabfall/baselines/<escenario>.<entorno>.json`, que se versiona: así el equipo compara
//! contra la misma referencia con `crabfall compare --baseline`.

use std::path::PathBuf;

use crabfall_core::{project::Project, tr};

use crate::{display, exit, runs};

/// Dónde vive la base de un escenario en un entorno.
pub fn path(project: &Project, scenario: &str, env: &str) -> PathBuf {
    project
        .dir
        .join("baselines")
        .join(format!("{scenario}.{env}.json"))
}

/// Copia `results.json` de una corrida como la base de su escenario y entorno.
pub fn set(
    project: &Project,
    results: &std::path::Path,
) -> Result<(PathBuf, serde_json::Value), String> {
    let doc = runs::read(results)?;
    let (Some(scenario), Some(env)) = (doc["scenario"].as_str(), doc["environment"].as_str())
    else {
        return Err(tr!(
            "{} does not say its scenario and environment",
            "{} no dice su escenario y entorno",
            display(results)
        ));
    };
    if doc["aborted"].is_string() {
        return Err(tr!(
            "run `{}` generated no load: it cannot be a baseline",
            "la corrida `{}` no generó carga: no puede ser la base",
            doc["run_id"].as_str().unwrap_or_default()
        ));
    }
    let file = path(project, scenario, env);
    std::fs::create_dir_all(file.parent().expect("baselines/ tiene padre"))
        .and_then(|_| std::fs::copy(results, &file).map(|_| ()))
        .map_err(|e| {
            tr!(
                "could not write {}: {e}",
                "no se pudo escribir {}: {e}",
                display(&file)
            )
        })?;
    Ok((file, doc))
}

/// El id de la corrida base de un escenario en un entorno, si hay.
pub fn run_id(project: &Project, scenario: &str, env: &str) -> Option<String> {
    let doc = runs::read(&path(project, scenario, env)).ok()?;
    doc["run_id"].as_str().map(String::from)
}

pub fn cmd_baseline(run: Option<&str>) -> u8 {
    let Some(project) = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd))
    else {
        eprintln!(
            "{}",
            tr!(
                "✗ no .crabfall/ found in this directory or its parents",
                "✗ no encontré .crabfall/ en este directorio ni en los superiores"
            )
        );
        return exit::INVALID;
    };
    match runs::file(&project, run).and_then(|f| set(&project, &f)) {
        Ok((file, doc)) => {
            println!(
                "{}",
                tr!(
                    "✓ baseline of {} in `{}`: run {} → {}",
                    "✓ base de {} en `{}`: la corrida {} → {}",
                    doc["scenario"].as_str().unwrap_or_default(),
                    doc["environment"].as_str().unwrap_or_default(),
                    doc["run_id"].as_str().unwrap_or_default(),
                    display(&file)
                )
            );
            println!(
                "{}",
                tr!(
                    "  Compare against it with: crabfall compare --baseline. Commit the file to share it.",
                    "  Compará contra ella con: crabfall compare --baseline. Versioná el archivo para compartirla."
                )
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("✗ {e}");
            exit::INVALID
        }
    }
}
