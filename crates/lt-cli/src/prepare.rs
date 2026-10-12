//! Carga común de `run` y `check`: proyecto, entorno, escenario validado y plan.

use std::path::PathBuf;

use lt_core::{
    config::Environment,
    engine::plan::Plan,
    project::{Project, describe},
    validate,
};

use crate::{display, exit};

pub struct Prepared {
    pub project: Project,
    pub env_name: String,
    pub env: Environment,
    pub path: PathBuf,
    pub plan: Plan,
}

/// Imprime los errores en stderr y devuelve el código de salida si algo falla.
pub fn prepare(arg: &str, env_name: Option<&str>) -> Result<Prepared, u8> {
    let fail = |code: u8, msg: String| {
        eprintln!("✗ {msg}");
        code
    };

    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd))
        .ok_or_else(|| {
            fail(
                exit::INVALID,
                "no encontré .loadtest/ en este directorio ni en los superiores".into(),
            )
        })?;
    let config = project.load_config().map_err(|e| fail(exit::INVALID, e))?;
    let (env_name, env) = config
        .environment(env_name)
        .map_err(|e| fail(exit::INVALID, e))?;
    let env = env.clone();

    let path = Project::scenario_path(Some(&project), arg);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        fail(
            exit::INVALID,
            format!("no se pudo leer {}: {e}", display(&path)),
        )
    })?;
    let scenario = validate::parse_scenario(&text).map_err(|report| {
        eprintln!("✗ {} no es válido (corré `lt validate`):", display(&path));
        for issue in report.errors.iter().chain(&report.todos) {
            eprintln!("  {}", describe(issue));
        }
        exit::INVALID
    })?;
    let plan = Plan::build(&scenario, &env).map_err(|errors| {
        eprintln!("✗ no se puede ejecutar {} en `{env_name}`:", scenario.name);
        for e in errors {
            eprintln!("  {e}");
        }
        exit::REFUSED
    })?;

    Ok(Prepared {
        project,
        env_name,
        env,
        path,
        plan,
    })
}
