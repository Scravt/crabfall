mod approve;
mod check;
mod claude;
mod compare;
mod findings;
mod hook;
mod prepare;
mod report;
mod run;
mod runs;

use std::{path::Path, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use crabfall_core::{
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
    name = "crabfall",
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
    /// Nombre (busca en .crabfall/scenarios/) o ruta al archivo.
    scenario: String,
    /// Entorno de .crabfall/config.yaml (por defecto, default_environment).
    #[arg(long)]
    env: Option<String>,
    /// Variable `nombre=valor`; pisa a las del escenario y el entorno. Se puede repetir.
    #[arg(long = "var", value_name = "NOMBRE=VALOR")]
    vars: Vec<String>,
    #[command(flatten)]
    tweaks: Tweaks,
}

/// Ajustes de la corrida sin editar el YAML (ADR-008). Cambian el tráfico, así que la
/// aprobación los firma: pasá los mismos a `approve` y a `run`.
#[derive(Args, Default)]
#[command(next_help_heading = "Ajustes (pisan al escenario)")]
pub struct Tweaks {
    /// Executor: constant_rate, ramping_rate o constant_vus. Descarta los campos de `load` que no le aplican.
    #[arg(long)]
    executor: Option<crabfall_core::scenario::Executor>,
    /// Iteraciones por segundo de constant_rate (50/s, 3000/m).
    #[arg(long, value_name = "50/s")]
    rate: Option<crabfall_core::units::Rate>,
    /// Duración de constant_rate o constant_vus (30s, 5m).
    #[arg(long, value_name = "30s")]
    duration: Option<crabfall_core::units::HumanDuration>,
    /// Usuarios concurrentes de constant_vus.
    #[arg(long, value_name = "N")]
    vus: Option<u32>,
    /// Máximo de iteraciones en vuelo de constant_rate y ramping_rate.
    #[arg(long, value_name = "N")]
    max_vus: Option<u32>,
    /// Tasa inicial de ramping_rate.
    #[arg(long, value_name = "0/s")]
    start_rate: Option<crabfall_core::units::Rate>,
    /// Etapas de ramping_rate: tasa:duración separadas por comas (100/s:1m,0/s:30s).
    #[arg(long, value_name = "TASA:DURACIÓN,...", value_parser = crabfall_core::overrides::parse_stages)]
    stages: Option<crabfall_core::overrides::Stages>,
    /// Espera a las iteraciones en curso al terminar (30s).
    #[arg(long, value_name = "30s")]
    graceful_stop: Option<crabfall_core::units::HumanDuration>,
    /// Pausa después de cada paso de los flows, fija (2s) o al azar (1s-3s); 0s la quita. Pisa a la del escenario.
    #[arg(long, value_name = "1s-3s")]
    think: Option<crabfall_core::units::Think>,
    /// Cada cuánto renueva la sesión cada VU (4m), para tokens que vencen durante la carga. Sin vu_refresh en el escenario, repite el vu_setup (login de nuevo).
    #[arg(long, value_name = "4m")]
    refresh_every: Option<crabfall_core::units::HumanDuration>,
    /// Otro archivo para un dataset, relativo a .crabfall/. Se puede repetir.
    #[arg(long = "data", value_name = "DATASET=ARCHIVO", value_parser = crabfall_core::overrides::parse_pair::<String>)]
    data: Vec<(String, String)>,
    /// Cómo elige filas un dataset: sequential o random. Se puede repetir.
    #[arg(long = "pick", value_name = "DATASET=MODO", value_parser = crabfall_core::overrides::parse_pair::<crabfall_core::scenario::Pick>)]
    pick: Vec<(String, crabfall_core::scenario::Pick)>,
    /// Criterio de la corrida, como en los resultados: "http.latency.p95<300ms", "http.latency.p99{step=login}<1s", "http.error_rate<1%", "http.rps>=40". Si pasás alguno, reemplazan a los del escenario. No cambian el tráfico: no hace falta volver a aprobar. Se puede repetir.
    #[arg(long = "threshold", value_name = "MÉTRICA<LÍMITE")]
    thresholds: Vec<crabfall_core::scenario::Threshold>,
}

impl Tweaks {
    pub fn overrides(&self) -> crabfall_core::overrides::Overrides {
        crabfall_core::overrides::Overrides {
            executor: self.executor,
            rate: self.rate,
            duration: self.duration,
            vus: self.vus,
            max_vus: self.max_vus,
            start_rate: self.start_rate,
            stages: self.stages.clone(),
            graceful_stop: self.graceful_stop,
            think: self.think,
            refresh_every: self.refresh_every,
            data: self.data.clone(),
            pick: self.pick.clone(),
            thresholds: self.thresholds.clone(),
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Integra Crabfall con Claude Code en este repo: skill, hook y permisos.
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
        #[command(flatten)]
        target: Target,
        /// Salida JSON para agentes.
        #[arg(long, help_heading = None::<&str>)]
        json: bool,
    },
    /// Hallazgos de una corrida (por defecto, la más reciente).
    Findings {
        /// Id de la corrida (carpeta en .crabfall/runs/) o `latest`.
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
        /// Corrida base (carpeta en .crabfall/runs/). Por defecto, la anterior a la nueva.
        base: Option<String>,
        /// Corrida nueva. Por defecto, `latest`.
        new: Option<String>,
        /// Salida JSON para agentes.
        #[arg(long)]
        json: bool,
    },
    /// Reporte HTML de una corrida (por defecto, la más reciente): gráficos, hallazgos y comparación.
    ///
    /// Escribe `report.html` al lado de `results.json`. `crabfall run` lo genera solo.
    Report {
        /// Id de la corrida (carpeta en .crabfall/runs/) o `latest`.
        run: Option<String>,
        /// Lo abre en el navegador.
        #[arg(long)]
        open: bool,
    },
    /// Imprime el JSON Schema del escenario (para editores y agentes).
    Schema,
    /// Smoke check: corre cada flow una vez, en serie, solo contra loopback. No necesita aprobación.
    Check {
        #[command(flatten)]
        target: Target,
        /// Salida JSON para agentes (incluye los bodies, truncados a 2 KB).
        #[arg(long, help_heading = None::<&str>)]
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
        #[arg(long, help_heading = None::<&str>)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Validate { target, json } => cmd_validate(&target, json),
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
        Command::Report { run, open } => report::cmd_report(run.as_deref(), open),
        Command::Schema => {
            let schema = crabfall_core::scenario::json_schema();
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

fn cmd_validate(target: &Target, as_json: bool) -> u8 {
    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd));
    let path = Project::scenario_path(project.as_ref(), &target.scenario);

    let report = match std::fs::read_to_string(&path) {
        Ok(text) => match validate::parse_scenario(&text) {
            Ok(mut scenario) => {
                let mut errors = target.tweaks.overrides().apply(&mut scenario);
                if errors.is_empty() {
                    errors = plan_errors(project.as_ref(), &scenario, target);
                }
                Report {
                    errors,
                    todos: vec![],
                }
            }
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
    scenario: &crabfall_core::scenario::Scenario,
    target: &Target,
) -> Vec<Issue> {
    let Some(project) = project else {
        return vec![];
    };
    let config = match project.load_config() {
        Ok(c) => c,
        Err(e) => return vec![issue(e)],
    };
    let env = match config.environment(target.env.as_deref()) {
        Ok((_, env)) => env,
        Err(e) => return vec![issue(e)],
    };
    let mut inputs = match prepare::inputs(project, &config, scenario, &target.vars) {
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
    /// Escribe .claude/skills/crabfall/SKILL.md y agrega el hook y los permisos a .claude/settings.json.
    Install,
}

#[derive(Subcommand)]
enum HookEvent {
    /// Bloquea `crabfall run` y `crabfall approve` invocados por Claude.
    PreToolUse,
}
