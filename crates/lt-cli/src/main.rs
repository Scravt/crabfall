mod approve;
mod check;
mod claude;
mod compare;
mod findings;
mod hook;
mod prepare;
mod run;
mod runs;

use std::{path::Path, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use lt_core::{
    engine::plan::Plan,
    project::{Project, describe},
    validate::{self, Issue, Report},
};
use serde_json::json;

/// Códigos de salida. Son parte del contrato con Claude y con CI.
mod exit {
    pub const OK: u8 = 0;
    /// Corrió, pero falló: en `check`, algún paso sin respuesta 2xx.
    pub const FAILED: u8 = 1;
    pub const INVALID: u8 = 2;
    /// No permitido: host fuera de allowed_hosts, límites superados, sin aprobación válida o sin terminal.
    pub const REFUSED: u8 = 3;
    /// Error al guardar resultados o aprobaciones, o al acceder al keyring.
    pub const RUNTIME: u8 = 4;
}

/// Versión de la salida `--json`.
const JSON_FORMAT: u32 = 1;

#[derive(Parser)]
#[command(
    name = "lt",
    version,
    about = "Pruebas de carga diseñadas con Claude, aprobadas por una persona"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Qué escenario, contra qué entorno y con qué variables.
#[derive(Args)]
pub struct Target {
    /// Nombre (busca en .loadtest/scenarios/) o ruta al archivo.
    scenario: String,
    /// Entorno de .loadtest/config.yaml (por defecto, default_environment).
    #[arg(long)]
    env: Option<String>,
    /// Variable `nombre=valor`; pisa a las del escenario y el entorno. Se puede repetir.
    #[arg(long = "var", value_name = "NOMBRE=VALOR")]
    vars: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Integra lt con Claude Code en este repo: skill, hook y permisos.
    Claude {
        #[command(subcommand)]
        action: ClaudeAction,
    },
    /// Hooks de Claude Code (los invoca Claude Code, no una persona).
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
    /// Valida un escenario: campos, tipos, unidades, variables y valores pendientes (`todo`).
    Validate {
        /// Nombre (busca en .loadtest/scenarios/) o ruta al archivo.
        scenario: String,
        /// Entorno contra el que se verifican variables y hosts (por defecto, default_environment).
        #[arg(long)]
        env: Option<String>,
        /// Salida JSON para agentes.
        #[arg(long)]
        json: bool,
    },
    /// Hallazgos de una corrida (por defecto, la más reciente).
    Findings {
        /// Id de la corrida (carpeta en .loadtest/runs/) o `latest`.
        run: Option<String>,
        /// Resumen compacto para agentes: totales, thresholds, hallazgos y pasos, sin series.
        #[arg(long)]
        json: bool,
    },
    /// Compara dos corridas: qué mejoró, qué empeoró y si es real o ruido.
    ///
    /// Sin argumentos, la más reciente contra la anterior del mismo escenario y entorno.
    /// Sale con 1 si hay regresiones.
    Compare {
        /// Corrida base (carpeta en .loadtest/runs/). Por defecto, la anterior a la nueva.
        base: Option<String>,
        /// Corrida nueva. Por defecto, `latest`.
        new: Option<String>,
        /// Salida JSON para agentes.
        #[arg(long)]
        json: bool,
    },
    /// Imprime el JSON Schema del escenario (para editores y agentes).
    Schema,
    /// Smoke check: corre cada flow una vez, en serie, solo contra loopback. No necesita aprobación.
    Check {
        #[command(flatten)]
        target: Target,
        /// Salida JSON para agentes (incluye los bodies, truncados a 2 KB).
        #[arg(long)]
        json: bool,
    },
    /// Aprueba un escenario para un entorno. Lo ejecuta una persona, en una terminal.
    Approve {
        #[command(flatten)]
        target: Target,
    },
    /// Ejecuta un escenario aprobado y guarda los resultados.
    Run {
        #[command(flatten)]
        target: Target,
        /// Imprime los resultados completos en JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Validate {
            scenario,
            env,
            json,
        } => cmd_validate(&scenario, env.as_deref(), json),
        Command::Claude {
            action: ClaudeAction::Install,
        } => claude::cmd_install(),
        Command::Hook {
            event: HookEvent::PreToolUse,
        } => hook::cmd_pre_tool_use(),
        Command::Findings { run, json } => findings::cmd_findings(run.as_deref(), json),
        Command::Compare { base, new, json } => {
            compare::cmd_compare(base.as_deref(), new.as_deref(), json)
        }
        Command::Schema => {
            let schema = lt_core::scenario::json_schema();
            println!("{}", serde_json::to_string_pretty(&schema).unwrap());
            exit::OK
        }
        Command::Check { target, json } => check::cmd_check(&target, json),
        Command::Approve { target } => approve::cmd_approve(&target),
        Command::Run { target, json } => run::cmd_run(&target, json),
    };
    ExitCode::from(code)
}

/// Muestra rutas relativas al directorio actual cuando se puede.
fn display(path: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    path.strip_prefix(&cwd)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn cmd_validate(arg: &str, env_name: Option<&str>, as_json: bool) -> u8 {
    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd));
    let path = Project::scenario_path(project.as_ref(), arg);

    let report = match std::fs::read_to_string(&path) {
        Ok(text) => match validate::parse_scenario(&text) {
            Ok(scenario) => Report {
                errors: plan_errors(project.as_ref(), &scenario, env_name),
                todos: vec![],
            },
            Err(report) => report,
        },
        Err(e) => Report {
            errors: vec![issue(format!("no se pudo leer {}: {e}", display(&path)))],
            todos: vec![],
        },
    };

    if as_json {
        let out = json!({
            "format": JSON_FORMAT,
            "scenario": display(&path),
            "valid": report.is_valid(),
            "errors": report.errors,
            "todos": report.todos,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else if report.is_valid() {
        println!("✓ {} es válido", display(&path));
    } else {
        for issue in &report.errors {
            println!("✗ {}", describe(issue));
        }
        for todo in &report.todos {
            println!("✗ {}", describe(todo));
        }
        if !report.todos.is_empty() {
            println!(
                "{} pendiente(s): completalos antes de ejecutar.",
                report.todos.len()
            );
        }
    }

    if report.is_valid() {
        exit::OK
    } else {
        exit::INVALID
    }
}

/// Con un proyecto, valida también contra el entorno: hosts, variables y secretos
/// declarados. Un secreto sin valor no es error acá: puede no estar en esta shell.
fn plan_errors(
    project: Option<&Project>,
    scenario: &lt_core::scenario::Scenario,
    env_name: Option<&str>,
) -> Vec<Issue> {
    let Some(project) = project else {
        return vec![];
    };
    let config = match project.load_config() {
        Ok(c) => c,
        Err(e) => return vec![issue(e)],
    };
    let env = match config.environment(env_name) {
        Ok((_, env)) => env,
        Err(e) => return vec![issue(e)],
    };
    let mut inputs = match prepare::inputs(project, &config, scenario, &[]) {
        Ok(i) => i,
        Err(e) => return vec![issue(e)],
    };
    for value in inputs.secrets.values_mut() {
        value.get_or_insert_with(String::new);
    }
    match Plan::build(scenario, env, &inputs) {
        Ok(_) => vec![],
        Err(errors) => errors.into_iter().map(issue).collect(),
    }
}

fn issue(message: String) -> Issue {
    Issue {
        path: String::new(),
        line: None,
        column: None,
        message,
    }
}

#[derive(Subcommand)]
enum ClaudeAction {
    /// Escribe .claude/skills/loadtest/SKILL.md y agrega el hook y los permisos a .claude/settings.json.
    Install,
}

#[derive(Subcommand)]
enum HookEvent {
    /// Bloquea `lt run` y `lt approve` invocados por Claude.
    PreToolUse,
}
