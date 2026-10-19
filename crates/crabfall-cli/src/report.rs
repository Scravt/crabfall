//! `crabfall report`: un HTML autocontenido por corrida, con gráficos, hallazgos y la
//! comparación con la corrida anterior (ADR-009). `crabfall run` lo genera al terminar.

use std::path::{Path, PathBuf};

use crabfall_core::{
    compare::{Run, compare},
    project::Project,
};
use serde_json::{Value, json};

use crate::{display, exit, runs};

const TEMPLATE: &str = include_str!("report.html");
const PLACEHOLDER: &str = "/*CRABFALL_DATA*/";

pub fn cmd_report(run: Option<&str>, open: bool) -> u8 {
    let Some(project) = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd))
    else {
        eprintln!("✗ no encontré .crabfall/ en este directorio ni en los superiores");
        return exit::INVALID;
    };
    let file = match runs::file(&project, run) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("✗ {e}");
            return exit::INVALID;
        }
    };
    match write(&project, &file) {
        Ok(report) => {
            println!("{}", display(&report));
            if open {
                open_in_browser(&report);
            }
            exit::OK
        }
        Err(e) => {
            eprintln!("✗ {e}");
            exit::RUNTIME
        }
    }
}

/// Escribe `report.html` al lado de `results.json` y devuelve su ruta.
pub fn write(project: &Project, results: &Path) -> Result<PathBuf, String> {
    let doc = runs::read(results)?;
    let html = render(&doc, comparison(project, &doc), &display(results));
    let out = results.with_file_name("report.html");
    std::fs::write(&out, html)
        .map_err(|e| format!("no se pudo escribir {}: {e}", display(&out)))?;
    Ok(out)
}

/// La comparación con la corrida anterior del mismo escenario y entorno, si hay una, y
/// los puntos por segundo de esa corrida (para el gráfico de antes y después).
fn comparison(project: &Project, doc: &Value) -> Option<(Value, Value)> {
    let base = runs::previous(
        project,
        doc["run_id"].as_str()?,
        doc["scenario"].as_str()?,
        doc["environment"].as_str()?,
    )
    .ok()?;
    let base_doc = runs::read(&base).ok()?;
    let points = base_doc["series"]["points"].clone();
    let base: Run = serde_json::from_value(base_doc).ok()?;
    let new: Run = serde_json::from_value(doc.clone()).ok()?;
    Some((
        serde_json::to_value(compare(&base, &new).ok()?).ok()?,
        points,
    ))
}

/// La plantilla con los datos adentro. El JSON va en un `<script type="application/json">`:
/// se escapa `<` para que ningún valor (un body, un mensaje de error) pueda cerrar el tag.
fn render(doc: &Value, comparison: Option<(Value, Value)>, results_file: &str) -> String {
    let (comparison, base_points) = comparison.unzip();
    let data = json!({
        "run": doc,
        "comparison": comparison,
        "base_points": base_points,
        "crabfall_version": env!("CARGO_PKG_VERSION"),
        "results_file": results_file,
    });
    let data = serde_json::to_string(&data)
        .expect("JSON siempre serializa")
        .replace('<', "\\u003c");
    TEMPLATE.replacen(PLACEHOLDER, &data, 1)
}

fn open_in_browser(path: &Path) {
    let path = path.to_string_lossy().into_owned();
    let result = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path])
            .status()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&path).status()
    } else {
        std::process::Command::new("xdg-open").arg(&path).status()
    };
    if !result.is_ok_and(|s| s.success()) {
        eprintln!("AVISO: no se pudo abrir el navegador; abrí el archivo a mano.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_the_data_without_letting_it_close_the_script() {
        let doc = json!({ "scenario": "s", "warnings": ["</script><script>alert(1)</script>"] });
        let html = render(&doc, None, "r.json");
        assert!(!html.contains(PLACEHOLDER));
        assert_eq!(
            html.matches("</script>").count(),
            TEMPLATE.matches("</script>").count()
        );
        let start = html
            .find(r#"<script id="crabfall-data" type="application/json">"#)
            .unwrap();
        let json = &html[start..];
        let json = &json[json.find('>').unwrap() + 1..json.find("</script>").unwrap()];
        let parsed: Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed["run"]["warnings"][0],
            "</script><script>alert(1)</script>"
        );
    }
}
