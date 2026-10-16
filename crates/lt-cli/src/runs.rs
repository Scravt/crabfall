//! Corridas guardadas en `.loadtest/runs/<id>/results.json`.

use std::path::PathBuf;

use lt_core::project::Project;
use serde_json::Value;

use crate::display;

/// Ids de las corridas con `results.json`, de la más vieja a la más reciente.
fn ids(project: &Project) -> Result<Vec<String>, String> {
    let mut ids: Vec<String> = std::fs::read_dir(project.runs_dir())
        .map_err(|_| "todavía no hay corridas en .loadtest/runs/".to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("results.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    Ok(ids)
}

/// `results.json` de la corrida `id`; sin id o con `latest`, la más reciente.
pub fn file(project: &Project, run: Option<&str>) -> Result<PathBuf, String> {
    let id = match run {
        Some(id) if id != "latest" => id.to_string(),
        _ => ids(project)?
            .pop()
            .ok_or("todavía no hay corridas en .loadtest/runs/")?,
    };
    let file = project.runs_dir().join(&id).join("results.json");
    if file.is_file() {
        Ok(file)
    } else {
        Err(format!("no existe la corrida `{id}`"))
    }
}

/// La corrida anterior a `run_id` del mismo escenario y entorno.
pub fn previous(
    project: &Project,
    run_id: &str,
    scenario: &str,
    env: &str,
) -> Result<PathBuf, String> {
    ids(project)?
        .into_iter()
        .rev()
        .filter(|id| id.as_str() < run_id)
        .map(|id| project.runs_dir().join(id).join("results.json"))
        .find(|f| read(f).is_ok_and(|doc| doc["scenario"] == scenario && doc["environment"] == env))
        .ok_or_else(|| {
            format!(
                "no hay una corrida de `{scenario}` en `{env}` anterior a `{run_id}` para comparar"
            )
        })
}

pub fn read(file: &std::path::Path) -> Result<Value, String> {
    std::fs::read_to_string(file)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|e| format!("no se pudo leer {}: {e}", display(file)))
}
