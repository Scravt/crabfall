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

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use crabfall_core::{
    engine::plan::Plan,
    i18n::{self, Lang},
    project::{Project, describe},
    tr,
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

// La ayuda está en inglés (los comentarios de documentación) y `spanish_help` la traduce
// cuando el idioma es español (ADR-016).
#[derive(Parser)]
#[command(
    name = "crabfall",
    version,
    about = "Load tests designed with Claude, approved by a person"
)]
struct Cli {
    /// Language of the output: en (default) or es. Also CRABFALL_LANG or `language:` in .crabfall/config.yaml.
    #[arg(long, global = true, value_name = "en|es")]
    lang: Option<Lang>,
    #[command(subcommand)]
    command: Command,
}

/// Qué escenario, contra qué entorno y con qué variables.
#[derive(Args)]
pub struct Target {
    /// Name (looked up in .crabfall/scenarios/) or path to the file.
    scenario: String,
    /// Environment from .crabfall/config.yaml (default: default_environment).
    #[arg(long)]
    env: Option<String>,
    /// Variable `name=value`; overrides the scenario's and the environment's. Can be repeated.
    #[arg(long = "var", value_name = "NAME=VALUE")]
    vars: Vec<String>,
    #[command(flatten)]
    tweaks: Tweaks,
}

/// Ajustes de la corrida sin editar el YAML (ADR-008). Cambian el tráfico, así que la
/// aprobación los firma: pasá los mismos a `approve` y a `run`.
#[derive(Args, Default)]
#[command(next_help_heading = "Tweaks (override the scenario)")]
pub struct Tweaks {
    /// Executor: constant_rate, ramping_rate or constant_vus. Drops the `load` fields that do not apply to it.
    #[arg(long)]
    executor: Option<crabfall_core::scenario::Executor>,
    /// Iterations per second for constant_rate (50/s, 3000/m).
    #[arg(long, value_name = "50/s")]
    rate: Option<crabfall_core::units::Rate>,
    /// Duration of constant_rate or constant_vus (30s, 5m).
    #[arg(long, value_name = "30s")]
    duration: Option<crabfall_core::units::HumanDuration>,
    /// Concurrent users for constant_vus.
    #[arg(long, value_name = "N")]
    vus: Option<u32>,
    /// Maximum iterations in flight for constant_rate and ramping_rate.
    #[arg(long, value_name = "N")]
    max_vus: Option<u32>,
    /// Starting rate of ramping_rate.
    #[arg(long, value_name = "0/s")]
    start_rate: Option<crabfall_core::units::Rate>,
    /// Stages of ramping_rate: rate:duration separated by commas (100/s:1m,0/s:30s).
    #[arg(long, value_name = "RATE:DURATION,...", value_parser = crabfall_core::overrides::parse_stages)]
    stages: Option<crabfall_core::overrides::Stages>,
    /// Waits for the iterations in progress when finishing (30s).
    #[arg(long, value_name = "30s")]
    graceful_stop: Option<crabfall_core::units::HumanDuration>,
    /// Pause after each flow step, fixed (2s) or random (1s-3s); 0s removes it. Overrides the scenario's.
    #[arg(long, value_name = "1s-3s")]
    think: Option<crabfall_core::units::Think>,
    /// How often each VU renews its session (4m), for tokens that expire during the load. Without vu_refresh in the scenario, it repeats vu_setup (logs in again).
    #[arg(long, value_name = "4m")]
    refresh_every: Option<crabfall_core::units::HumanDuration>,
    /// Another file for a dataset, relative to .crabfall/. Can be repeated.
    #[arg(long = "data", value_name = "DATASET=FILE", value_parser = crabfall_core::overrides::parse_pair::<String>)]
    data: Vec<(String, String)>,
    /// How a dataset picks rows: sequential or random. Can be repeated.
    #[arg(long = "pick", value_name = "DATASET=MODE", value_parser = crabfall_core::overrides::parse_pair::<crabfall_core::scenario::Pick>)]
    pick: Vec<(String, crabfall_core::scenario::Pick)>,
    /// Pass criterion, as in the results: "http.latency.p95<300ms", "http.latency.p99{step=login}<1s", "http.error_rate<1%", "http.rps>=40". If you pass any, they replace the scenario's. They do not change the traffic: no new approval needed. Can be repeated.
    #[arg(long = "threshold", value_name = "METRIC<LIMIT")]
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
    /// Integrates Crabfall with Claude Code in this repo: skill, hook and permissions.
    Claude {
        #[command(subcommand)]
        action: ClaudeAction,
    },
    /// Claude Code hooks (Claude Code invokes them, not a person).
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
    /// Validates a scenario: fields, types, units, variables and pending values (`todo`).
    Validate {
        #[command(flatten)]
        target: Target,
        /// JSON output for agents.
        #[arg(long, help_heading = None::<&str>)]
        json: bool,
    },
    /// Findings of a run (by default, the most recent one).
    Findings {
        /// Run id (folder in .crabfall/runs/) or `latest`.
        run: Option<String>,
        /// Compact summary for agents: totals, thresholds, findings and steps, without series.
        #[arg(long)]
        json: bool,
    },
    /// Compares two runs: what improved, what got worse and whether it is real or noise.
    ///
    /// Without arguments, the most recent one against the previous one of the same scenario
    /// and environment. Exits with 1 if there are regressions.
    Compare {
        /// Base run (folder in .crabfall/runs/). By default, the one before the new one.
        base: Option<String>,
        /// New run. By default, `latest`.
        new: Option<String>,
        /// JSON output for agents.
        #[arg(long)]
        json: bool,
    },
    /// HTML report of a run (by default, the most recent one): charts, findings and comparison.
    ///
    /// Writes `report.html` next to `results.json`. `crabfall run` generates it on its own.
    Report {
        /// Run id (folder in .crabfall/runs/) or `latest`.
        run: Option<String>,
        /// Opens it in the browser.
        #[arg(long)]
        open: bool,
    },
    /// Prints the JSON Schema of the scenario (for editors and agents).
    Schema,
    /// Smoke check: runs each flow once, in series, only against loopback. Needs no approval.
    Check {
        #[command(flatten)]
        target: Target,
        /// JSON output for agents (includes the bodies, truncated to 2 KB).
        #[arg(long, help_heading = None::<&str>)]
        json: bool,
    },
    /// Approves a scenario for an environment. A person runs it, in a terminal.
    Approve {
        #[command(flatten)]
        target: Target,
    },
    /// Runs an approved scenario and saves the results.
    Run {
        #[command(flatten)]
        target: Target,
        /// Prints the full results as JSON.
        #[arg(long, help_heading = None::<&str>)]
        json: bool,
    },
}

fn main() -> ExitCode {
    choose_language();
    let mut command = Cli::command();
    if i18n::is_es() {
        command = spanish_help(command);
    }
    let cli = match Cli::from_arg_matches(&command.get_matches()) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
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

/// `--lang` > `CRABFALL_LANG` > `language:` del config.yaml > inglés (ADR-016). Se resuelve
/// antes de que clap arme la ayuda, así que `--lang` se busca a mano en los argumentos.
fn choose_language() {
    let args: Vec<String> = std::env::args().collect();
    let flag = args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--lang=")
            .map(String::from)
            .or_else(|| (a == "--lang").then(|| args.get(i + 1).cloned()).flatten())
    });
    if let Some(lang) = flag.and_then(|l| l.parse::<Lang>().ok()) {
        i18n::set(lang);
        return;
    }
    let config = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd))
        .and_then(|p| p.load_config().ok());
    if let Some(lang) = config.and_then(|c| c.language) {
        i18n::set_from_config(lang);
    }
}

/// La ayuda en español: los mismos textos que los comentarios de arriba.
fn spanish_help(command: clap::Command) -> clap::Command {
    const TARGET: &[(&str, &str)] = &[
        (
            "scenario",
            "Nombre (busca en .crabfall/scenarios/) o ruta al archivo",
        ),
        (
            "env",
            "Entorno de .crabfall/config.yaml (por defecto, default_environment)",
        ),
        (
            "vars",
            "Variable `nombre=valor`; pisa a las del escenario y el entorno. Se puede repetir",
        ),
        (
            "executor",
            "Executor: constant_rate, ramping_rate o constant_vus. Descarta los campos de `load` que no le aplican",
        ),
        (
            "rate",
            "Iteraciones por segundo de constant_rate (50/s, 3000/m)",
        ),
        (
            "duration",
            "Duración de constant_rate o constant_vus (30s, 5m)",
        ),
        ("vus", "Usuarios concurrentes de constant_vus"),
        (
            "max_vus",
            "Máximo de iteraciones en vuelo de constant_rate y ramping_rate",
        ),
        ("start_rate", "Tasa inicial de ramping_rate"),
        (
            "stages",
            "Etapas de ramping_rate: tasa:duración separadas por comas (100/s:1m,0/s:30s)",
        ),
        (
            "graceful_stop",
            "Espera a las iteraciones en curso al terminar (30s)",
        ),
        (
            "think",
            "Pausa después de cada paso de los flows, fija (2s) o al azar (1s-3s); 0s la quita. Pisa a la del escenario",
        ),
        (
            "refresh_every",
            "Cada cuánto renueva la sesión cada VU (4m), para tokens que vencen durante la carga. Sin vu_refresh en el escenario, repite el vu_setup (login de nuevo)",
        ),
        (
            "data",
            "Otro archivo para un dataset, relativo a .crabfall/. Se puede repetir",
        ),
        (
            "pick",
            "Cómo elige filas un dataset: sequential o random. Se puede repetir",
        ),
        (
            "thresholds",
            "Criterio de la corrida, como en los resultados: \"http.latency.p95<300ms\", \"http.latency.p99{step=login}<1s\", \"http.error_rate<1%\", \"http.rps>=40\". Si pasás alguno, reemplazan a los del escenario. No cambian el tráfico: no hace falta volver a aprobar. Se puede repetir",
        ),
    ];
    const TWEAKS: &[&str] = &[
        "executor",
        "rate",
        "duration",
        "vus",
        "max_vus",
        "start_rate",
        "stages",
        "graceful_stop",
        "think",
        "refresh_every",
        "data",
        "pick",
        "thresholds",
    ];
    let target = |mut c: clap::Command| {
        for (id, help) in TARGET {
            c = c.mut_arg(*id, |a| {
                let a = a.help(*help);
                if TWEAKS.contains(id) {
                    a.help_heading("Ajustes (pisan al escenario)")
                } else {
                    a
                }
            });
        }
        c
    };
    command
        .about("Pruebas de carga diseñadas con Claude, aprobadas por una persona")
        .mut_arg("lang", |a| {
            a.help("Idioma de la salida: en (por defecto) o es. También CRABFALL_LANG o `language:` en .crabfall/config.yaml")
        })
        .mut_subcommand("claude", |c| {
            c.about("Integra Crabfall con Claude Code en este repo: skill, hook y permisos")
                .mut_subcommand("install", |c| {
                    c.about("Escribe .claude/skills/crabfall/SKILL.md y agrega el hook y los permisos a .claude/settings.json")
                })
        })
        .mut_subcommand("validate", |c| {
            target(c)
                .about("Valida un escenario: campos, tipos, unidades, variables y valores pendientes (`todo`)")
                .mut_arg("json", |a| a.help("Salida JSON para agentes"))
        })
        .mut_subcommand("findings", |c| {
            c.about("Hallazgos de una corrida (por defecto, la más reciente)")
                .mut_arg("run", |a| a.help("Id de la corrida (carpeta en .crabfall/runs/) o `latest`"))
                .mut_arg("json", |a| {
                    a.help("Resumen compacto para agentes: totales, thresholds, hallazgos y pasos, sin series")
                })
        })
        .mut_subcommand("compare", |c| {
            c.about("Compara dos corridas: qué mejoró, qué empeoró y si es real o ruido")
                .long_about(
                    "Compara dos corridas: qué mejoró, qué empeoró y si es real o ruido.\n\n\
                     Sin argumentos, la más reciente contra la anterior del mismo escenario y entorno.\n\
                     Sale con 1 si hay regresiones",
                )
                .mut_arg("base", |a| {
                    a.help("Corrida base (carpeta en .crabfall/runs/). Por defecto, la anterior a la nueva")
                })
                .mut_arg("new", |a| a.help("Corrida nueva. Por defecto, `latest`"))
                .mut_arg("json", |a| a.help("Salida JSON para agentes"))
        })
        .mut_subcommand("report", |c| {
            c.about("Reporte HTML de una corrida (por defecto, la más reciente): gráficos, hallazgos y comparación")
                .long_about(
                    "Reporte HTML de una corrida (por defecto, la más reciente): gráficos, hallazgos y comparación.\n\n\
                     Escribe `report.html` al lado de `results.json`. `crabfall run` lo genera solo",
                )
                .mut_arg("run", |a| a.help("Id de la corrida (carpeta en .crabfall/runs/) o `latest`"))
                .mut_arg("open", |a| a.help("Lo abre en el navegador"))
        })
        .mut_subcommand("schema", |c| {
            c.about("Imprime el JSON Schema del escenario (para editores y agentes)")
        })
        .mut_subcommand("check", |c| {
            target(c)
                .about("Smoke check: corre cada flow una vez, en serie, solo contra loopback. No necesita aprobación")
                .mut_arg("json", |a| {
                    a.help("Salida JSON para agentes (incluye los bodies, truncados a 2 KB)")
                })
        })
        .mut_subcommand("approve", |c| {
            target(c).about("Aprueba un escenario para un entorno. Lo ejecuta una persona, en una terminal")
        })
        .mut_subcommand("run", |c| {
            target(c)
                .about("Ejecuta un escenario aprobado y guarda los resultados")
                .mut_arg("json", |a| a.help("Imprime los resultados completos en JSON"))
        })
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
            errors: vec![issue(tr!(
                "could not read {}: {e}",
                "no se pudo leer {}: {e}",
                display(&path)
            ))],
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
        println!("{}", tr!("✓ {} is valid", "✓ {} es válido", display(&path)));
    } else {
        for issue in &report.errors {
            println!("✗ {}", describe(issue));
        }
        for todo in &report.todos {
            println!("✗ {}", describe(todo));
        }
        if !report.todos.is_empty() {
            println!(
                "{}",
                tr!(
                    "{} pending value(s): fill them in before running.",
                    "{} pendiente(s): completalos antes de ejecutar.",
                    report.todos.len()
                )
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
    /// Writes .claude/skills/crabfall/SKILL.md and adds the hook and the permissions to .claude/settings.json.
    Install,
}

#[derive(Subcommand)]
enum HookEvent {
    /// Blocks `crabfall run` and `crabfall approve` invoked by Claude.
    PreToolUse,
}
