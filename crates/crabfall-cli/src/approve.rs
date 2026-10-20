//! `crabfall approve` y la verificación de `crabfall run` (ADR-004 §2-§3). Las piezas
//! (resumen, firma, verificación) devuelven datos, no imprimen: las usa también la TUI.

use std::{
    io::{BufRead, IsTerminal, Write},
    path::PathBuf,
};

use crabfall_core::{
    approval::{self, Approval, OsKeyring, Rejection},
    config::Environment,
    engine::plan::{Plan, is_loopback},
    i18n::pick,
    project::Project,
    tr,
    units::HumanDuration,
};

use crate::{
    display, exit,
    prepare::{Prepared, prepare},
};

pub fn cmd_approve(target: &crate::Target) -> u8 {
    let Prepared {
        project,
        env_name,
        env,
        plan,
        tweaks,
        ..
    } = match prepare(target) {
        Ok(p) => p,
        Err(code) => return code,
    };
    if let Err(lines) = within_limits(&plan, &env, &env_name) {
        for line in lines {
            eprintln!("{line}");
        }
        return exit::REFUSED;
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        eprintln!(
            "{}",
            tr!(
                "✗ `crabfall approve` needs an interactive terminal: a person gives the approval (ADR-004).",
                "✗ `crabfall approve` necesita una terminal interactiva: la aprobación la da una persona (ADR-004)."
            )
        );
        return exit::REFUSED;
    }

    let (change, warning) = change(&project, &plan, &env, &env_name);
    if let Some(w) = warning {
        eprintln!("{w}");
    }
    for line in summary(&plan, &env, &env_name, &change, &tweaks) {
        eprintln!("{line}");
    }

    let target = target_host(&plan);
    eprint!(
        "{}",
        tr!(
            "\nTo approve, type the target host ({target}): ",
            "\nPara aprobar, escribí el host destino ({target}): "
        )
    );
    std::io::stderr().flush().ok();
    let mut typed = String::new();
    std::io::stdin().lock().read_line(&mut typed).ok();
    if !typed.trim().eq_ignore_ascii_case(target) {
        eprintln!(
            "{}",
            tr!(
                "✗ it does not match `{target}`: nothing was approved.",
                "✗ no coincide con `{target}`: no se aprobó nada."
            )
        );
        return exit::REFUSED;
    }

    match sign(&project, &plan, &env, &env_name, &tweaks) {
        Ok(file) => {
            eprintln!(
                "{}",
                tr!("✓ approved: {}", "✓ aprobado: {}", display(&file))
            );
            // Con los mismos ajustes: la aprobación los firma y `run` sin ellos la rechaza.
            eprintln!(
                "{}",
                tr!(
                    "  Run it with: {}",
                    "  Ejecutalo con: {}",
                    command("run", &plan.scenario, &env_name, &tweaks)
                )
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("✗ {e}");
            exit::RUNTIME
        }
    }
}

/// Los límites duros del entorno (ADR-004): si el plan los supera, no se puede aprobar.
pub fn within_limits(plan: &Plan, env: &Environment, env_name: &str) -> Result<(), Vec<String>> {
    let violations = plan.limit_violations(env);
    if violations.is_empty() {
        return Ok(());
    }
    let mut lines = vec![tr!(
        "✗ {} exceeds the limits of `{env_name}`; it cannot be approved:",
        "✗ {} supera los límites de `{env_name}`; no se puede aprobar:",
        plan.scenario
    )];
    lines.extend(violations.into_iter().map(|v| format!("  {v}")));
    Err(lines)
}

/// El host que tiene que tipear quien aprueba: el primero que no es loopback.
pub fn target_host(plan: &Plan) -> &str {
    plan.hosts
        .iter()
        .find(|h| !is_loopback(h))
        .or_else(|| plan.hosts.iter().next())
        .expect("un plan válido tiene al menos un host")
}

/// Firma el plan y guarda la aprobación. Devuelve el archivo.
pub fn sign(
    project: &Project,
    plan: &Plan,
    env: &Environment,
    env_name: &str,
    tweaks: &[String],
) -> Result<PathBuf, String> {
    let approved_by = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| pick("unknown", "desconocido").into());
    let approved_at = jiff::Timestamp::now().to_string();
    let mut signed = approval::approve(plan, env, env_name, &approved_by, &approved_at, &OsKeyring)
        .map_err(|e| tr!("could not sign: {e}", "no se pudo firmar: {e}"))?;
    signed.flags = tweaks.to_vec();
    let file = approval::path(&project.approvals_dir(), &plan.scenario, env_name);
    project
        .ensure_gitignore()
        .and_then(|_| {
            std::fs::create_dir_all(project.approvals_dir())?;
            std::fs::write(&file, serde_json::to_string_pretty(&signed).unwrap())
        })
        .map_err(|e| {
            tr!(
                "could not save the approval: {e}",
                "no se pudo guardar la aprobación: {e}"
            )
        })?;
    Ok(file)
}

/// Verifica la aprobación antes de `crabfall run`. Imprime el motivo si la rechaza.
pub fn verify_for_run(
    project: &Project,
    plan: &Plan,
    env: &Environment,
    env_name: &str,
    flags: &[String],
) -> Result<Approval, ()> {
    verify(project, plan, env, env_name, flags).map_err(|lines| {
        for line in lines {
            eprintln!("{line}");
        }
    })
}

/// La aprobación vigente, o por qué no la hay (en líneas, con los comandos para resolverlo).
pub fn verify(
    project: &Project,
    plan: &Plan,
    env: &Environment,
    env_name: &str,
    flags: &[String],
) -> Result<Approval, Vec<String>> {
    let command = |verb: &str, flags: &[String]| command(verb, &plan.scenario, env_name, flags);
    let fail = |msg: String| {
        vec![
            tr!(
                "✗ {} in `{env_name}`: {msg}",
                "✗ {} en `{env_name}`: {msg}",
                plan.scenario
            ),
            tr!(
                "  A person has to approve it: {}",
                "  Una persona tiene que aprobarlo: {}",
                command("approve", flags)
            ),
        ]
    };
    let current = load(project, &plan.scenario, env_name).map_err(fail)?;
    match approval::verify(current.as_ref(), plan, env, env_name, &OsKeyring) {
        Ok(()) => Ok(current.expect("verificada")),
        Err(Rejection::Changed(diff)) => {
            let approved = &current.as_ref().expect("hay aprobación").flags;
            let mut lines = if approved != flags {
                // Lo más común: se aprobó con otros flags. El diff de abajo lo confirma.
                let shown = |f: &[String]| {
                    if f.is_empty() {
                        pick("no tweaks", "sin ajustes").to_string()
                    } else {
                        f.join(" ")
                    }
                };
                vec![
                    tr!(
                        "✗ {} in `{env_name}`: it was approved with other tweaks",
                        "✗ {} en `{env_name}`: se aprobó con otros ajustes",
                        plan.scenario
                    ),
                    tr!("  approved: {}", "  aprobado: {}", shown(approved)),
                    tr!("  now:      {}", "  ahora:    {}", shown(flags)),
                    tr!(
                        "  To run what was approved: {}",
                        "  Para correr lo aprobado: {}",
                        command("run", approved)
                    ),
                    tr!(
                        "  To approve this:          {}",
                        "  Para aprobar esto:       {}",
                        command("approve", flags)
                    ),
                ]
            } else {
                fail(tr!(
                    "it changed since the last approval",
                    "cambió desde la última aprobación"
                ))
            };
            lines.extend(limit_lines(&diff, 40).lines().map(String::from));
            Err(lines)
        }
        Err(r) => Err(fail(r.to_string())),
    }
}

/// `crabfall <verb> <escenario> --env <entorno> <flags>`, para copiar y pegar.
pub fn command(verb: &str, scenario: &str, env_name: &str, flags: &[String]) -> String {
    let mut c = format!("crabfall {verb} {scenario} --env {env_name}");
    for f in flags {
        c.push(' ');
        c.push_str(f);
    }
    c
}

pub fn load(project: &Project, scenario: &str, env_name: &str) -> Result<Option<Approval>, String> {
    let file = approval::path(&project.approvals_dir(), scenario, env_name);
    match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
            tr!(
                "{} is damaged ({e})",
                "{} está dañado ({e})",
                display(&file)
            )
        }),
        Err(_) => Ok(None),
    }
}

/// Cómo está el plan frente a su última aprobación.
pub enum Change {
    First,
    Same,
    Changed(String),
}

/// Compara el plan con la última aprobación. El aviso, si el archivo está dañado.
pub fn change(
    project: &Project,
    plan: &Plan,
    env: &Environment,
    env_name: &str,
) -> (Change, Option<String>) {
    let (previous, warning) = match load(project, &plan.scenario, env_name) {
        Ok(p) => (p, None),
        Err(e) => (
            None,
            Some(tr!(
                "WARNING: {e}; it is replaced.",
                "AVISO: {e}; se reemplaza."
            )),
        ),
    };
    let canonical = plan.canonical(env);
    let change = match &previous {
        None => Change::First,
        Some(p) if p.hash == approval::fingerprint(&canonical) => Change::Same,
        Some(p) => Change::Changed(approval::diff(&p.plan, &canonical)),
    };
    (change, warning)
}

/// El resumen de riesgo que ve quien aprueba, línea por línea.
pub fn summary(
    plan: &Plan,
    env: &Environment,
    env_name: &str,
    change: &Change,
    tweaks: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    let hosts: Vec<&str> = plan.hosts.iter().map(String::as_str).collect();
    let (duration, graceful_stop) = (plan.load.duration(), plan.load.graceful_stop());
    let status = match change {
        Change::First => tr!("first approval", "primera aprobación"),
        Change::Same => tr!(
            "unchanged since the last approval",
            "sin cambios desde la última aprobación"
        ),
        Change::Changed(_) => tr!(
            "CHANGED since the last approval",
            "CAMBIÓ desde la última aprobación"
        ),
    };

    out.push(tr!(
        "Scenario    {}  ({status})",
        "Escenario   {}  ({status})",
        plan.scenario
    ));
    out.push(tr!(
        "Environment {env_name} → {}",
        "Entorno     {env_name} → {}",
        hosts.join(", ")
    ));
    if !tweaks.is_empty() {
        out.push(tr!(
            "Tweaks      {}  (override the scenario)",
            "Ajustes     {}  (pisan al escenario)",
            tweaks.join(" ")
        ));
    }
    out.push(tr!(
        "Load        {}",
        "Carga       {}",
        plan.load.describe()
    ));
    if let Some(n) = plan.load.planned_iterations() {
        out.push(tr!(
            "            ≈{n} iterations",
            "            ≈{n} iteraciones"
        ));
    }
    out.push(format!("            {}", plan.rate_summary()));
    out.push(tr!(
        "Duration    {} + {} of graceful stop",
        "Duración    {} + {} de graceful stop",
        HumanDuration(duration),
        HumanDuration(graceful_stop)
    ));
    if let Some(think) = plan.think_summary() {
        out.push(tr!("Pauses      {think}", "Pausas      {think}"));
    }
    if !plan.vu_setup.is_empty() {
        let vus = plan.load.concurrency();
        let steps: Vec<String> = plan
            .vu_setup
            .iter()
            .map(|s| format!("{} ({} {})", s.name, s.method, s.url()))
            .collect();
        out.push(tr!(
            "Per VU      {} × {vus} VUs = {} requests before the load",
            "Por VU      {} × {vus} VUs = {} requests antes de la carga",
            steps.join(", "),
            vus as usize * steps.len()
        ));
    }
    if let (Some(refresh), Some(r)) = (plan.refresh_summary(), &plan.vu_refresh) {
        // Cota: cada VU renueva a lo sumo una vez por período mientras dura la carga.
        let rounds = (plan.load.duration().as_secs_f64() / r.every.as_secs_f64()).floor() as usize;
        let requests = rounds * plan.load.concurrency() as usize * plan.refresh_steps().len();
        out.push(tr!(
            "Renewal     {refresh} per VU, in the background: up to {requests} requests",
            "Renovación  {refresh} por VU, en segundo plano: hasta {requests} requests"
        ));
    }
    for (i, u) in plan.uploads_summary().iter().enumerate() {
        let label = if i == 0 {
            pick("Files", "Archivos")
        } else {
            ""
        };
        out.push(tr!(
            "{label:<11} {u}  (multipart, in every request)",
            "{label:<11} {u}  (multipart, en cada request)"
        ));
    }
    out.push(format!("HTTP        {}", plan.http_summary()));
    for (i, d) in plan.data.iter().enumerate() {
        let label = if i == 0 { pick("Data", "Datos") } else { "" };
        out.push(format!("{label:<11} {}", d.dataset.describe()));
    }

    let writes: Vec<String> = plan
        .all_steps()
        .filter(|s| {
            s.tags.iter().any(|t| t == "write")
                || !matches!(s.method.as_str(), "GET" | "HEAD" | "OPTIONS")
        })
        .map(|s| format!("{} ({} {})", s.name, s.method, s.url()))
        .collect();
    if writes.is_empty() {
        out.push(pick("Writes      no", "Escribe     no").into());
    } else {
        out.push(tr!(
            "Writes      YES: {}",
            "Escribe     SÍ: {}",
            writes.join(", ")
        ));
    }

    let l = &env.limits;
    let mut limits = Vec::new();
    if let Some(m) = l.max_rps {
        limits.push(format!("max_rps {m} ✓"));
    }
    if let Some(m) = l.max_vus {
        limits.push(format!("max_vus {m} ✓"));
    }
    if let Some(m) = l.max_duration {
        limits.push(format!("max_duration {m} ✓"));
    }
    if limits.is_empty() {
        out.push(tr!(
            "Limits      none defined in the environment",
            "Límites     ninguno definido en el entorno"
        ));
    } else {
        out.push(tr!("Limits      {}", "Límites     {}", limits.join("  ")));
    }

    if let Change::Changed(diff) = change {
        out.push(String::new());
        out.push(tr!(
            "Changes since the last approval:",
            "Cambios desde la última aprobación:"
        ));
        out.extend(limit_lines(diff, 60).lines().map(String::from));
    }
    out
}

fn limit_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.trim_end().to_string();
    }
    tr!(
        "{}\n… ({} more lines)",
        "{}\n… ({} líneas más)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
