//! `crabfall mcp`: servidor MCP por stdio (ADR-019).
//!
//! Una interfaz delgada más (ADR-001): cada herramienta corre el mismo binario con `--json`
//! en el directorio del proyecto, así que valen las mismas reglas que en la terminal, guarda
//! incluida. Solo se exponen los comandos que un agente puede usar; la carga sale únicamente
//! por `request_run`, que es `crabfall run --ask`: una persona confirma en una ventana.

use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crabfall_core::{i18n::pick, project::Project, tr};
use serde_json::{Map, Value, json};

use crate::exit;

/// Versiones del protocolo que entiende, de la más nueva a la más vieja.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Los ajustes que se pueden pasar en `tweaks`, como en la CLI (ADR-008, ADR-018). Las claves
/// van con `_` y se pasan como `--clave-con-guiones`; una lista se repite.
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
    "threshold",
    "diagnosis_off",
    "latency_margin",
    "min_requests",
];

pub fn cmd_mcp(project: Option<&Path>) -> u8 {
    let root = match project {
        Some(dir) => Project::discover(dir),
        None => std::env::current_dir()
            .ok()
            .and_then(|cwd| Project::discover(&cwd)),
    };
    let Some(project) = root else {
        eprintln!(
            "{}",
            tr!(
                "✗ no .crabfall/ found: start `crabfall mcp` in the project, or pass --project",
                "✗ no encontré .crabfall/: arrancá `crabfall mcp` en el proyecto, o pasá --project"
            )
        );
        return exit::INVALID;
    };
    let server = Server {
        root: project.dir.parent().unwrap_or(&project.dir).to_path_buf(),
        dir: project.dir.clone(),
        exe: std::env::current_exe().unwrap_or_else(|_| "crabfall".into()),
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle(&line) {
            // Un mensaje por línea (transporte stdio de MCP).
            if writeln!(stdout, "{reply}")
                .and_then(|()| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
    exit::OK
}

struct Server {
    /// Donde corren los comandos: la carpeta que tiene `.crabfall/`.
    root: PathBuf,
    /// `.crabfall/`.
    dir: PathBuf,
    exe: PathBuf,
}

impl Server {
    /// Una línea JSON-RPC; `None` si era una notificación (no lleva respuesta).
    fn handle(&self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or_default();
        let params = &msg["params"];
        let Some(id) = id else {
            // `notifications/initialized`, `notifications/cancelled`…: nada que responder.
            return None;
        };
        let result = match method {
            "initialize" => Ok(initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call(params),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"].as_str().unwrap_or_default();
        let args = params
            .get("arguments")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let invalid = |message: String| Ok(tool_error(&message));
        match name {
            "list_scenarios" => Ok(self.list_scenarios()),
            "schema" => Ok(self.crabfall(&["schema"])),
            "validate" | "check" | "request_run" => {
                let mut cli = match name {
                    "validate" => vec!["validate".to_string()],
                    "check" => vec!["check".to_string()],
                    _ => vec!["run".to_string()],
                };
                match target_args(&args) {
                    Ok(target) => cli.extend(target),
                    Err(e) => return invalid(e),
                }
                if name == "request_run" {
                    cli.push("--ask".into());
                }
                cli.push("--json".into());
                Ok(self.crabfall(&cli))
            }
            "findings" => {
                let mut cli = vec!["findings".to_string()];
                if let Some(run) = string(&args, "run") {
                    cli.push(run);
                }
                cli.push("--json".into());
                Ok(self.crabfall(&cli))
            }
            "compare" => {
                let mut cli = vec!["compare".to_string()];
                let baseline = args.get("baseline").and_then(Value::as_bool) == Some(true);
                if baseline {
                    cli.push("--baseline".into());
                } else if let Some(base) = string(&args, "base") {
                    cli.push(base);
                }
                if let Some(new) = string(&args, "new") {
                    cli.push(new);
                }
                cli.push("--json".into());
                Ok(self.crabfall(&cli))
            }
            _ => Err((-32602, format!("unknown tool: {name}"))),
        }
    }

    /// Corre `crabfall <args>` en el proyecto. El JSON que imprime va en `structuredContent`;
    /// lo de stderr (avisos, por qué rechazó) va al lado, en `messages`.
    fn crabfall<S: AsRef<str>>(&self, args: &[S]) -> Value {
        let out = Command::new(&self.exe)
            .args(args.iter().map(AsRef::as_ref))
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                return tool_error(&tr!(
                    "could not start crabfall: {e}",
                    "no se pudo arrancar crabfall: {e}"
                ));
            }
        };
        let code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let output: Option<Value> = serde_json::from_str(stdout.trim()).ok();
        let structured = json!({
            "exit_code": code,
            "meaning": meaning(code),
            "output": output,
            "messages": stderr,
        });
        let text = match &output {
            Some(o) => serde_json::to_string_pretty(o).unwrap_or_default(),
            None if !stdout.trim().is_empty() => stdout.trim().to_string(),
            None => stderr.clone(),
        };
        let mut content = vec![json!({ "type": "text", "text": text })];
        if output.is_some() && !stderr.is_empty() {
            content.push(json!({ "type": "text", "text": stderr }));
        }
        json!({
            "content": content,
            "structuredContent": structured,
            // Sin JSON no hay resultado que leer: el comando no llegó a responder.
            "isError": output.is_none(),
        })
    }

    /// Los escenarios del proyecto, los entornos, y para cada escenario su última corrida y si
    /// tiene base fija. No hay un comando de la CLI para esto: lo lee directo del proyecto.
    fn list_scenarios(&self) -> Value {
        let project = Project {
            dir: self.dir.clone(),
        };
        let config = project.load_config().ok();
        let environments: Vec<String> = config
            .as_ref()
            .map(|c| c.environments.keys().cloned().collect())
            .unwrap_or_default();
        let runs = latest_runs(&project);
        let mut scenarios = Vec::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(project.dir.join("scenarios"))
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.sort();
        for file in files {
            if !matches!(
                file.extension().and_then(|e| e.to_str()),
                Some("yaml" | "yml" | "json")
            ) {
                continue;
            }
            let name = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parsed = std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|t| {
                    crabfall_core::validate::parse_scenario(&t).map_err(|r| {
                        r.errors
                            .first()
                            .or(r.todos.first())
                            .map(|i| format!("{}: {}", i.path, i.message))
                            .unwrap_or_default()
                    })
                });
            let (description, executor, problem) = match parsed {
                Ok(s) => (
                    s.description.clone(),
                    serde_json::to_value(s.load.executor).ok(),
                    None,
                ),
                Err(e) => (None, None, Some(e)),
            };
            let baselines: Vec<&String> = environments
                .iter()
                .filter(|env| crate::baseline::path(&project, &name, env).is_file())
                .collect();
            scenarios.push(json!({
                "name": name,
                "description": description,
                "executor": executor,
                "invalid": problem,
                "latest_run": runs.get(&name),
                "baseline_in": baselines,
            }));
        }
        let output = json!({
            "project": crate::display(&self.root),
            "environments": environments,
            "default_environment": config.and_then(|c| c.default_environment),
            "scenarios": scenarios,
        });
        json!({
            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&output).unwrap_or_default() }],
            "structuredContent": output,
            "isError": false,
        })
    }
}

/// La corrida más reciente de cada escenario (en el orden de `runs::all`).
fn latest_runs(project: &Project) -> Map<String, Value> {
    let mut latest: Map<String, Value> = Map::new();
    for (_, doc) in crate::runs::all(project) {
        if let Some(scenario) = doc["scenario"].as_str() {
            latest.insert(
                scenario.to_string(),
                json!({
                    "run_id": doc["run_id"],
                    "environment": doc["environment"],
                    "passed": doc["passed"],
                    "findings": doc["findings"].as_array().map(|f| f.iter().map(|f| f["id"].clone()).collect::<Vec<_>>()),
                }),
            );
        }
    }
    latest
}

/// `scenario`, `env`, `vars` y `tweaks` como argumentos de la CLI. Solo los ajustes conocidos:
/// nada de `--window`, `--ask` o flags internos.
fn target_args(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let scenario = string(args, "scenario")
        .ok_or_else(|| pick("`scenario` is required", "falta `scenario`").to_string())?;
    if scenario.starts_with('-') {
        return Err(tr!(
            "`scenario` can't start with `-`",
            "`scenario` no puede empezar con `-`"
        ));
    }
    let mut out = vec![scenario];
    if let Some(env) = string(args, "env") {
        out.extend(["--env".into(), env]);
    }
    if let Some(vars) = args.get("vars").and_then(Value::as_object) {
        for (name, value) in vars {
            out.extend(["--var".into(), format!("{name}={}", scalar(value))]);
        }
    }
    if let Some(tweaks) = args.get("tweaks").and_then(Value::as_object) {
        for (key, value) in tweaks {
            if !TWEAKS.contains(&key.as_str()) {
                return Err(tr!(
                    "unknown tweak `{key}`; they are: {}",
                    "ajuste desconocido `{key}`; son: {}",
                    TWEAKS.join(", ")
                ));
            }
            let flag = format!("--{}", key.replace('_', "-"));
            let values = match value {
                Value::Array(items) => items.iter().map(scalar).collect(),
                // `data` y `pick` también aceptan un objeto: { "users": "data/otros.csv" }.
                Value::Object(pairs) => pairs
                    .iter()
                    .map(|(k, v)| format!("{k}={}", scalar(v)))
                    .collect(),
                v => vec![scalar(v)],
            };
            for v in values {
                out.extend([flag.clone(), v]);
            }
        }
    }
    Ok(out)
}

fn string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(String::from)
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Qué quiere decir cada código de salida, para que el agente no tenga que saberlos.
fn meaning(code: i32) -> &'static str {
    match code {
        0 => "ok",
        1 => "failed: steps, thresholds, setup or regressions",
        2 => "invalid scenario, configuration or arguments",
        3 => "refused: not approved, host not allowed, limits, or the person did not confirm",
        4 => "keyring error",
        _ => "unexpected",
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "crabfall", "title": "Crabfall", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

const INSTRUCTIONS: &str = "Crabfall load tests for this project. Design scenarios in .crabfall/scenarios/*.yaml (see the `schema` tool), \
check them with `validate` and `check` (a smoke check, loopback only), and analyze runs with `findings` and `compare`. \
You cannot approve or run load yourself: `request_run` opens a window where a person confirms the run and watches it; \
it can take minutes. Report what was measured separately from your hypotheses about the code, and never turn diagnosis \
rules off or loosen them to make a run look healthy.";

fn tools() -> Value {
    let target = |extra: &str| {
        json!({
            "type": "object",
            "properties": {
                "scenario": { "type": "string", "description": "Scenario name (in .crabfall/scenarios/) or path to its file." },
                "env": { "type": "string", "description": "Environment from .crabfall/config.yaml. Default: default_environment." },
                "vars": { "type": "object", "description": "Variables name → value; they override the scenario's and the environment's.", "additionalProperties": { "type": ["string", "number", "boolean"] } },
                "tweaks": {
                    "type": "object",
                    "description": format!("Per-run tweaks, like the CLI flags without the dashes: {}. Lists repeat the flag; `data` and `pick` take an object dataset → value. {extra}", TWEAKS.join(", ")),
                    "additionalProperties": true
                }
            },
            "required": ["scenario"],
            "additionalProperties": false
        })
    };
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    json!([
        {
            "name": "list_scenarios",
            "title": "List scenarios",
            "description": "The project's scenarios with their description and executor (or why they don't parse), the environments, and for each scenario its latest run (with finding ids) and where it has a fixed baseline.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": read_only,
        },
        {
            "name": "schema",
            "title": "Scenario JSON Schema",
            "description": "The full JSON Schema of a scenario, with a description of each field. Read it before writing or changing a scenario.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": read_only,
        },
        {
            "name": "validate",
            "title": "Validate a scenario",
            "description": "Validates a scenario with its tweaks: fields, types, units, variables, datasets, limits and pending `todo` values, each error with its exact path. Sends nothing.",
            "inputSchema": target("Tweaks that change the traffic will need the person's approval when running."),
            "annotations": read_only,
        },
        {
            "name": "check",
            "title": "Smoke check",
            "description": "Runs setup and each flow once, serially, with 1 VU, only against loopback (127.0.0.0/8, ::1): it refuses any other host and never follows a redirect outside. Use it to fix a scenario before asking for a run. It sends real requests, so steps that write will write once.",
            "inputSchema": target("The load block is ignored."),
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false },
        },
        {
            "name": "request_run",
            "title": "Ask a person to run the load",
            "description": "Asks for a load run: a window opens on the person's screen with the risk summary; they approve it (typing the host if the plan is new or changed) or confirm, and the load runs there, where they can stop it. Waits up to 5 minutes for an answer plus the run's duration. Exit code 3 means it was refused or nobody confirmed: don't retry on your own, ask the person. Windows and Linux with a desktop only. Then read the result with `findings` and `compare`.",
            "inputSchema": target("The same tweaks as for `validate`."),
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": true },
        },
        {
            "name": "findings",
            "title": "Findings of a run",
            "description": "Compact summary of a run for agents: totals, thresholds, diagnosis settings, findings (with evidence, source and probable causes) and per-step metrics, without the per-second series.",
            "inputSchema": {
                "type": "object",
                "properties": { "run": { "type": "string", "description": "Run id (folder in .crabfall/runs/) or `latest` (default)." } },
                "additionalProperties": false
            },
            "annotations": read_only,
        },
        {
            "name": "compare",
            "title": "Compare two runs",
            "description": "What improved, what got worse, and whether each change is consistent or may be noise (a sign test over paired seconds). With `baseline`, compares against the scenario's fixed baseline. Two fixed-VU runs that differ only in VUs also get `scaling`: whether the throughput followed the VUs or the server is saturated. Exit code 1 means regressions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "base": { "type": "string", "description": "Base run. Default: the one before `new`." },
                    "new": { "type": "string", "description": "New run. Default: `latest`." },
                    "baseline": { "type": "boolean", "description": "Compare `new` against the fixed baseline instead of the previous run." }
                },
                "additionalProperties": false
            },
            "annotations": read_only,
        }
    ])
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn target_arguments_become_cli_flags() {
        let a = args(json!({
            "scenario": "checkout",
            "env": "staging",
            "vars": { "product_id": 7 },
            "tweaks": { "rate": "100/s", "diagnosis_off": ["long_tail", "time_split"], "data": { "users": "data/x.csv" } }
        }));
        assert_eq!(
            target_args(&a).unwrap(),
            [
                "checkout",
                "--env",
                "staging",
                "--var",
                "product_id=7",
                "--rate",
                "100/s",
                "--diagnosis-off",
                "long_tail",
                "--diagnosis-off",
                "time_split",
                "--data",
                "users=data/x.csv"
            ]
        );
    }

    #[test]
    fn only_known_tweaks_and_no_flags_as_scenario() {
        for bad in [
            json!({ "scenario": "s", "tweaks": { "window": "x.json" } }),
            json!({ "scenario": "s", "tweaks": { "ask": true } }),
            json!({ "scenario": "--window" }),
            json!({ "env": "local" }),
        ] {
            assert!(target_args(&args(bad.clone())).is_err(), "{bad}");
        }
    }

    #[test]
    fn negotiates_the_protocol_version() {
        assert_eq!(
            initialize(&json!({ "protocolVersion": "2025-03-26" }))["protocolVersion"],
            "2025-03-26"
        );
        assert_eq!(
            initialize(&json!({ "protocolVersion": "1999-01-01" }))["protocolVersion"],
            PROTOCOL_VERSIONS[0]
        );
    }
}
