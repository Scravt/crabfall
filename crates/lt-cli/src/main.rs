mod check;
mod prepare;
mod run;

use std::{path::Path, process::ExitCode};

use clap::{Parser, Subcommand};
use lt_core::{
    project::{Project, describe},
    validate,
};
use serde_json::json;

/// Códigos de salida. Son parte del contrato con Claude y con CI.
mod exit {
    pub const OK: u8 = 0;
    /// Corrió, pero falló: en `check`, algún paso sin respuesta 2xx.
    pub const FAILED: u8 = 1;
    pub const INVALID: u8 = 2;
    /// No permitido: host fuera de allowed_hosts, límites superados o sin confirmación.
    pub const REFUSED: u8 = 3;
    /// La corrida terminó pero no se pudieron guardar los resultados.
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

#[derive(Subcommand)]
enum Command {
    /// Valida un escenario: campos, tipos, unidades y valores pendientes (`todo`).
    Validate {
        /// Nombre (busca en .loadtest/scenarios/) o ruta al archivo.
        scenario: String,
        /// Salida JSON para agentes.
        #[arg(long)]
        json: bool,
    },
    /// Smoke check: corre cada flow una vez, en serie, solo contra loopback. No necesita aprobación.
    Check {
        /// Nombre (busca en .loadtest/scenarios/) o ruta al archivo.
        scenario: String,
        /// Entorno de .loadtest/config.yaml (por defecto, default_environment).
        #[arg(long)]
        env: Option<String>,
        /// Salida JSON para agentes (incluye los bodies, truncados a 2 KB).
        #[arg(long)]
        json: bool,
    },
    /// Ejecuta un escenario contra un entorno y guarda los resultados.
    Run {
        /// Nombre (busca en .loadtest/scenarios/) o ruta al archivo.
        scenario: String,
        /// Entorno de .loadtest/config.yaml (por defecto, default_environment).
        #[arg(long)]
        env: Option<String>,
        /// Imprime los resultados completos en JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Validate { scenario, json } => cmd_validate(&scenario, json),
        Command::Run {
            scenario,
            env,
            json,
        } => run::cmd_run(&scenario, env.as_deref(), json),
        Command::Check {
            scenario,
            env,
            json,
        } => check::cmd_check(&scenario, env.as_deref(), json),
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

fn cmd_validate(arg: &str, as_json: bool) -> u8 {
    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd));
    let path = Project::scenario_path(project.as_ref(), arg);

    let report = match std::fs::read_to_string(&path) {
        Ok(text) => validate::parse_scenario(&text).err().unwrap_or_default(),
        Err(e) => validate::Report {
            errors: vec![validate::Issue {
                path: String::new(),
                line: None,
                column: None,
                message: format!("no se pudo leer {}: {e}", display(&path)),
            }],
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
