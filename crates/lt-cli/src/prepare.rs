//! Carga común de `run`, `check` y `approve`: proyecto, entorno, escenario validado con
//! los ajustes de la CLI, y plan.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use lt_core::{
    config::{Config, Environment},
    data::Dataset,
    engine::plan::{Inputs, Plan},
    project::{Project, describe},
    scenario::Scenario,
    validate,
};
use serde_json::Value;

use crate::{display, exit};

pub struct Prepared {
    pub project: Project,
    pub env_name: String,
    pub env: Environment,
    pub path: PathBuf,
    pub plan: Plan,
    /// `--var` y ajustes de la CLI aplicados, como flags (`--rate 100/s`), listos para repetir.
    pub tweaks: Vec<String>,
}

/// Imprime los errores en stderr y devuelve el código de salida si algo falla.
pub fn prepare(target: &crate::Target) -> Result<Prepared, u8> {
    let (arg, env_name, vars) = (&target.scenario, target.env.as_deref(), &target.vars);
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
    let mut scenario = validate::parse_scenario(&text).map_err(|report| {
        eprintln!("✗ {} no es válido (corré `lt validate`):", display(&path));
        for issue in report.errors.iter().chain(&report.todos) {
            eprintln!("  {}", describe(issue));
        }
        exit::INVALID
    })?;
    let overrides = target.tweaks.overrides();
    let problems = overrides.apply(&mut scenario);
    if !problems.is_empty() {
        eprintln!("✗ los ajustes no sirven para {}:", scenario.name);
        for issue in &problems {
            eprintln!("  {}", describe(issue));
        }
        return Err(exit::INVALID);
    }
    let inputs = inputs(&project, &config, &scenario, vars).map_err(|e| fail(exit::INVALID, e))?;
    let plan = Plan::build(&scenario, &env, &inputs).map_err(|errors| {
        eprintln!("✗ no se puede armar {} en `{env_name}`:", scenario.name);
        for e in errors {
            eprintln!("  {e}");
        }
        exit::INVALID
    })?;

    Ok(Prepared {
        project,
        env_name,
        env,
        path,
        plan,
        tweaks: vars
            .iter()
            .map(|v| format!("--var {v}"))
            .chain(overrides.describe())
            .map(|flag| quote(&flag))
            .collect(),
    })
}

/// `--var`, los secretos declarados en `config.yaml` (con su valor si lo tienen) y los
/// datasets de `data`, leídos desde `.loadtest/`.
pub fn inputs(
    project: &Project,
    config: &Config,
    scenario: &Scenario,
    vars: &[String],
) -> Result<Inputs, String> {
    let mut parsed = BTreeMap::new();
    for var in vars {
        let (name, raw) = var
            .split_once('=')
            .ok_or_else(|| format!("--var `{var}`: usá nombre=valor"))?;
        // `42` es un número, `true` un booleano y el resto un string, como en YAML.
        let value = validate::parse_yaml::<Value>(raw)
            .ok()
            .filter(|v| !v.is_object() && !v.is_array() && !v.is_null())
            .unwrap_or_else(|| Value::String(raw.to_string()));
        parsed.insert(name.trim().to_string(), value);
    }

    let root = project.dir.parent().unwrap_or(Path::new("."));
    let dotenv = read_dotenv(&root.join(".env"));
    let secrets = config
        .secrets
        .iter()
        .map(|name| {
            let value = std::env::var(name)
                .ok()
                .or_else(|| dotenv.get(name).cloned());
            (name.clone(), value)
        })
        .collect();
    let mut data = BTreeMap::new();
    let mut errors = Vec::new();
    for (name, spec) in &scenario.data {
        match Dataset::load(&project.dir, name, spec) {
            Ok(d) => {
                data.insert(name.clone(), d);
            }
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join(
            "
  ",
        ));
    }
    Ok(Inputs {
        vars: parsed,
        secrets,
        data,
    })
}

/// `.env` simple: `NOMBRE=valor`, comentarios con `#`, comillas opcionales.
fn read_dotenv(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.trim_start_matches("export ").split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(v);
            (k.trim().to_string(), v.to_string())
        })
        .collect()
}

/// `--var nombre=un valor` → `--var "nombre=un valor"`, para copiar y pegar en una shell.
fn quote(flag: &str) -> String {
    match flag.split_once(' ') {
        Some((name, value)) if value.contains([' ', '"', '\'', '<', '>', '|', '&']) => {
            format!("{name} \"{}\"", value.replace('"', "\\\""))
        }
        _ => flag.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn quotes_values_with_spaces_only() {
        assert_eq!(quote("--rate 100/s"), "--rate 100/s");
        assert_eq!(
            quote("--threshold http.latency.p95<300ms"),
            "--threshold \"http.latency.p95<300ms\""
        );
        assert_eq!(quote("--var who=bob maría"), "--var \"who=bob maría\"");
        assert_eq!(quote("--var q=say \"hi\""), "--var \"q=say \\\"hi\\\"\"");
    }
}
