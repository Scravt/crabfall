//! Corridas guardadas en `.crabfall/runs/<id>/results.json`.

use std::path::PathBuf;

use crabfall_core::project::Project;
use serde_json::Value;

use crate::display;

/// Las corridas con `results.json` y su documento, de la más vieja a la más reciente.
///
/// Se ordenan por `started_at`, un instante absoluto, y no por el id: el id sale de la hora
/// local de la máquina que corrió, así que una corrida hecha en WSL (en UTC) y otra en
/// Windows (en hora local) contra el mismo proyecto se ordenarían mal. El id desempata.
pub fn all(project: &Project) -> Vec<(String, Value)> {
    let mut runs: Vec<(String, Value)> = std::fs::read_dir(project.runs_dir())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let doc = read(&e.path().join("results.json")).ok()?;
            Some((id, doc))
        })
        .collect();
    runs.sort_by_cached_key(|(id, doc)| (started(doc), id.clone()));
    runs
}

/// El instante de inicio en nanosegundos desde 1970; sin `started_at` legible, 0 (va primero).
fn started(doc: &Value) -> i128 {
    doc["started_at"]
        .as_str()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
        .map_or(0, |t| t.as_nanosecond())
}

/// Ids de las corridas, de la más vieja a la más reciente (ver `all`).
fn ids(project: &Project) -> Result<Vec<String>, String> {
    let ids: Vec<String> = all(project).into_iter().map(|(id, _)| id).collect();
    if ids.is_empty() && !project.runs_dir().is_dir() {
        return Err(crabfall_core::tr!(
            "there are no runs in .crabfall/runs/ yet",
            "todavía no hay corridas en .crabfall/runs/"
        ));
    }
    Ok(ids)
}

/// `results.json` de la corrida `id`; sin id o con `latest`, la más reciente.
pub fn file(project: &Project, run: Option<&str>) -> Result<PathBuf, String> {
    let id = match run {
        Some(id) if id != "latest" => id.to_string(),
        _ => ids(project)?.pop().ok_or(crabfall_core::tr!(
            "there are no runs in .crabfall/runs/ yet",
            "todavía no hay corridas en .crabfall/runs/"
        ))?,
    };
    let file = project.runs_dir().join(&id).join("results.json");
    if file.is_file() {
        Ok(file)
    } else {
        Err(crabfall_core::tr!(
            "run `{id}` does not exist",
            "no existe la corrida `{id}`"
        ))
    }
}

/// La corrida anterior a `run_id` del mismo escenario y entorno.
pub fn previous(
    project: &Project,
    run_id: &str,
    scenario: &str,
    env: &str,
) -> Result<PathBuf, String> {
    let ids = ids(project)?;
    let before = ids.iter().position(|id| id == run_id).unwrap_or(ids.len());
    ids[..before]
        .iter()
        .rev()
        .map(|id| project.runs_dir().join(id).join("results.json"))
        .find(|f| read(f).is_ok_and(|doc| doc["scenario"] == scenario && doc["environment"] == env))
        .ok_or_else(|| {
            crabfall_core::tr!(
                "there is no run of `{scenario}` in `{env}` before `{run_id}` to compare with",
                "no hay una corrida de `{scenario}` en `{env}` anterior a `{run_id}` para comparar"
            )
        })
}

pub fn read(file: &std::path::Path) -> Result<Value, String> {
    std::fs::read_to_string(file)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|e| {
            crabfall_core::tr!(
                "could not read {}: {e}",
                "no se pudo leer {}: {e}",
                display(file)
            )
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn runs_are_ordered_by_when_they_started_not_by_id() {
        let dir = std::env::temp_dir().join(format!("crabfall-runs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = Project {
            dir: dir.join(".crabfall"),
        };
        // Lo que pasó en market-api: una corrida en WSL (id en UTC) y otra después en
        // Windows (id en hora local, UTC-3), así que la más nueva tiene el id menor.
        for (id, started) in [
            ("20261024-133910", "2026-10-24T13:39:10Z"),
            ("20261024-133051", "2026-10-24T16:30:51Z"),
            ("20261024-095758", "2026-10-24T12:57:58Z"),
        ] {
            let run = project.runs_dir().join(id);
            std::fs::create_dir_all(&run).unwrap();
            let doc = json!({ "run_id": id, "started_at": started, "scenario": "s", "environment": "local" });
            std::fs::write(run.join("results.json"), doc.to_string()).unwrap();
        }
        let ids: Vec<String> = all(&project).into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            ["20261024-095758", "20261024-133910", "20261024-133051"]
        );
        assert!(
            file(&project, None)
                .unwrap()
                .ends_with("20261024-133051/results.json")
        );
        let before = previous(&project, "20261024-133051", "s", "local").unwrap();
        assert!(before.ends_with("20261024-133910/results.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
