//! `crabfall approve` y la verificación de `crabfall run` (ADR-004 §2-§3).

use std::io::{BufRead, IsTerminal, Write};

use crabfall_core::{
    approval::{self, Approval, OsKeyring, Rejection},
    config::Environment,
    engine::plan::{Plan, is_loopback},
    project::Project,
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
    let violations = plan.limit_violations(&env);
    if !violations.is_empty() {
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "✗ {} exceeds the limits of `{env_name}`; it cannot be approved:",
                "✗ {} supera los límites de `{env_name}`; no se puede aprobar:",
                plan.scenario
            )
        );
        for v in violations {
            eprintln!("  {v}");
        }
        return exit::REFUSED;
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "✗ `crabfall approve` needs an interactive terminal: a person gives the approval (ADR-004).",
                "✗ `crabfall approve` necesita una terminal interactiva: la aprobación la da una persona (ADR-004)."
            )
        );
        return exit::REFUSED;
    }

    let previous = match load(&project, &plan.scenario, &env_name) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "{}",
                crabfall_core::tr!("WARNING: {e}; it is replaced.", "AVISO: {e}; se reemplaza.")
            );
            None
        }
    };
    let canonical = plan.canonical(&env);
    let change = match &previous {
        None => Change::First,
        Some(p) if p.hash == approval::fingerprint(&canonical) => Change::Same,
        Some(p) => Change::Changed(approval::diff(&p.plan, &canonical)),
    };
    print_summary(&plan, &env, &env_name, &change, &tweaks);

    let target = plan
        .hosts
        .iter()
        .find(|h| !is_loopback(h))
        .or_else(|| plan.hosts.iter().next())
        .expect("un plan válido tiene al menos un host");
    eprint!(
        "{}",
        crabfall_core::tr!(
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
            crabfall_core::tr!(
                "✗ it does not match `{target}`: nothing was approved.",
                "✗ no coincide con `{target}`: no se aprobó nada."
            )
        );
        return exit::REFUSED;
    }

    let approved_by = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or("desconocido".into());
    let approved_at = jiff::Timestamp::now().to_string();
    let mut signed = match approval::approve(
        &plan,
        &env,
        &env_name,
        &approved_by,
        &approved_at,
        &OsKeyring,
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "{}",
                crabfall_core::tr!("✗ could not sign: {e}", "✗ no se pudo firmar: {e}")
            );
            return exit::RUNTIME;
        }
    };
    signed.flags = tweaks.clone();
    let file = approval::path(&project.approvals_dir(), &plan.scenario, &env_name);
    let saved = project.ensure_gitignore().and_then(|_| {
        std::fs::create_dir_all(project.approvals_dir())?;
        std::fs::write(&file, serde_json::to_string_pretty(&signed).unwrap())
    });
    match saved {
        Ok(()) => {
            eprintln!("✓ aprobado: {}", display(&file));
            // Con los mismos ajustes: la aprobación los firma y `run` sin ellos la rechaza.
            eprintln!(
                "{}",
                crabfall_core::tr!(
                    "  Run it with: {}",
                    "  Ejecutalo con: {}",
                    command("run", &plan.scenario, &env_name, &tweaks)
                )
            );
            exit::OK
        }
        Err(e) => {
            eprintln!(
                "{}",
                crabfall_core::tr!(
                    "✗ could not save the approval: {e}",
                    "✗ no se pudo guardar la aprobación: {e}"
                )
            );
            exit::RUNTIME
        }
    }
}

/// Verifica la aprobación antes de `crabfall run`. Imprime el motivo si la rechaza.
pub fn verify_for_run(
    project: &Project,
    plan: &Plan,
    env: &Environment,
    env_name: &str,
    flags: &[String],
) -> Result<Approval, ()> {
    let command = |verb: &str, flags: &[String]| command(verb, &plan.scenario, env_name, flags);
    let fail = |msg: String| {
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "✗ {} in `{env_name}`: {msg}",
                "✗ {} en `{env_name}`: {msg}",
                plan.scenario
            )
        );
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "  A person has to approve it: {}",
                "  Una persona tiene que aprobarlo: {}",
                command("approve", flags)
            )
        );
    };
    let current = match load(project, &plan.scenario, env_name) {
        Ok(a) => a,
        Err(e) => {
            fail(e);
            return Err(());
        }
    };
    match approval::verify(current.as_ref(), plan, env, env_name, &OsKeyring) {
        Ok(()) => Ok(current.expect("verificada")),
        Err(Rejection::Changed(diff)) => {
            let approved = &current.as_ref().expect("hay aprobación").flags;
            if approved != flags {
                // Lo más común: se aprobó con otros flags. El diff de abajo lo confirma.
                let shown = |f: &[String]| {
                    if f.is_empty() {
                        crabfall_core::tr!("no tweaks", "sin ajustes").to_string()
                    } else {
                        f.join(" ")
                    }
                };
                eprintln!(
                    "{}",
                    crabfall_core::tr!(
                        "✗ {} in `{env_name}`: it was approved with other tweaks",
                        "✗ {} en `{env_name}`: se aprobó con otros ajustes",
                        plan.scenario
                    )
                );
                eprintln!(
                    "{}",
                    crabfall_core::tr!("  approved: {}", "  aprobado: {}", shown(approved))
                );
                eprintln!(
                    "{}",
                    crabfall_core::tr!("  now:      {}", "  ahora:    {}", shown(flags))
                );
                eprintln!(
                    "{}",
                    crabfall_core::tr!(
                        "  To run what was approved: {}",
                        "  Para correr lo aprobado: {}",
                        command("run", approved)
                    )
                );
                eprintln!(
                    "{}",
                    crabfall_core::tr!(
                        "  To approve this:          {}",
                        "  Para aprobar esto:       {}",
                        command("approve", flags)
                    )
                );
            } else {
                fail(crabfall_core::tr!(
                    "it changed since the last approval",
                    "cambió desde la última aprobación"
                ));
            }
            eprintln!("{}", limit_lines(&diff, 40));
            Err(())
        }
        Err(r) => {
            fail(r.to_string());
            Err(())
        }
    }
}

/// `crabfall <verb> <escenario> --env <entorno> <flags>`, para copiar y pegar.
fn command(verb: &str, scenario: &str, env_name: &str, flags: &[String]) -> String {
    let mut c = format!("crabfall {verb} {scenario} --env {env_name}");
    for f in flags {
        c.push(' ');
        c.push_str(f);
    }
    c
}

fn load(project: &Project, scenario: &str, env_name: &str) -> Result<Option<Approval>, String> {
    let file = approval::path(&project.approvals_dir(), scenario, env_name);
    match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
            crabfall_core::tr!(
                "{} is damaged ({e})",
                "{} está dañado ({e})",
                display(&file)
            )
        }),
        Err(_) => Ok(None),
    }
}

enum Change {
    First,
    Same,
    Changed(String),
}

fn print_summary(
    plan: &Plan,
    env: &Environment,
    env_name: &str,
    change: &Change,
    tweaks: &[String],
) {
    let hosts: Vec<&str> = plan.hosts.iter().map(String::as_str).collect();
    let (duration, graceful_stop) = (plan.load.duration(), plan.load.graceful_stop());
    let status = match change {
        Change::First => crabfall_core::tr!("first approval", "primera aprobación"),
        Change::Same => crabfall_core::tr!(
            "unchanged since the last approval",
            "sin cambios desde la última aprobación"
        ),
        Change::Changed(_) => crabfall_core::tr!(
            "CHANGED since the last approval",
            "CAMBIÓ desde la última aprobación"
        ),
    };

    eprintln!(
        "{}",
        crabfall_core::tr!(
            "Scenario    {}  ({status})",
            "Escenario   {}  ({status})",
            plan.scenario
        )
    );
    eprintln!(
        "{}",
        crabfall_core::tr!(
            "Environment {env_name} → {}",
            "Entorno     {env_name} → {}",
            hosts.join(", ")
        )
    );
    if !tweaks.is_empty() {
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "Tweaks      {}  (override the scenario)",
                "Ajustes     {}  (pisan al escenario)",
                tweaks.join(" ")
            )
        );
    }
    eprintln!(
        "{}",
        crabfall_core::tr!("Load        {}", "Carga       {}", plan.load.describe())
    );
    if let Some(n) = plan.load.planned_iterations() {
        eprintln!("            ≈{n} iteraciones");
    }
    eprintln!("            {}", plan.rate_summary());
    eprintln!(
        "{}",
        crabfall_core::tr!(
            "Duration    {} + {} of graceful stop",
            "Duración    {} + {} de graceful stop",
            HumanDuration(duration),
            HumanDuration(graceful_stop)
        )
    );
    if let Some(think) = plan.think_summary() {
        eprintln!(
            "{}",
            crabfall_core::tr!("Pauses      {think}", "Pausas      {think}")
        );
    }
    if !plan.vu_setup.is_empty() {
        let vus = plan.load.concurrency();
        let steps: Vec<String> = plan
            .vu_setup
            .iter()
            .map(|s| format!("{} ({} {})", s.name, s.method, s.url()))
            .collect();
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "Per VU      {} × {vus} VUs = {} requests before the load",
                "Por VU      {} × {vus} VUs = {} requests antes de la carga",
                steps.join(", "),
                vus as usize * steps.len()
            )
        );
    }
    if let (Some(refresh), Some(r)) = (plan.refresh_summary(), &plan.vu_refresh) {
        // Cota: cada VU renueva a lo sumo una vez por período mientras dura la carga.
        let rounds = (plan.load.duration().as_secs_f64() / r.every.as_secs_f64()).floor() as usize;
        let requests = rounds * plan.load.concurrency() as usize * plan.refresh_steps().len();
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "Renewal     {refresh} per VU, in the background: up to {requests} requests",
                "Renovación  {refresh} por VU, en segundo plano: hasta {requests} requests"
            )
        );
    }
    for (i, u) in plan.uploads_summary().iter().enumerate() {
        let label = if i == 0 { "Archivos" } else { "" };
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "{label:<11} {u}  (multipart, in every request)",
                "{label:<11} {u}  (multipart, en cada request)"
            )
        );
    }
    eprintln!("HTTP        {}", plan.http_summary());
    for (i, d) in plan.data.iter().enumerate() {
        let label = if i == 0 {
            crabfall_core::i18n::pick("Data", "Datos")
        } else {
            ""
        };
        eprintln!("{label:<11} {}", d.dataset.describe());
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
        eprintln!(
            "{}",
            crabfall_core::i18n::pick("Writes      no", "Escribe     no")
        );
    } else {
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "Writes      YES: {}",
                "Escribe     SÍ: {}",
                writes.join(", ")
            )
        );
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
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "Limits      none defined in the environment",
                "Límites     ninguno definido en el entorno"
            )
        );
    } else {
        eprintln!(
            "{}",
            crabfall_core::tr!("Limits      {}", "Límites     {}", limits.join("  "))
        );
    }

    if let Change::Changed(diff) = change {
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "\nChanges since the last approval:\n{}",
                "\nCambios desde la última aprobación:\n{}",
                limit_lines(diff, 60)
            )
        );
    }
}

fn limit_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.trim_end().to_string();
    }
    crabfall_core::tr!(
        "{}\n… ({} more lines)",
        "{}\n… ({} líneas más)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
