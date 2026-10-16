//! `lt approve` y la verificación de `lt run` (ADR-004 §2-§3).

use std::io::{BufRead, IsTerminal, Write};

use lt_core::{
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
        ..
    } = match prepare(&target.scenario, target.env.as_deref(), &target.vars) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let violations = plan.limit_violations(&env);
    if !violations.is_empty() {
        eprintln!(
            "✗ {} supera los límites de `{env_name}`; no se puede aprobar:",
            plan.scenario
        );
        for v in violations {
            eprintln!("  {v}");
        }
        return exit::REFUSED;
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        eprintln!(
            "✗ `lt approve` necesita una terminal interactiva: la aprobación la da una persona (ADR-004)."
        );
        return exit::REFUSED;
    }

    let previous = match load(&project, &plan.scenario, &env_name) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AVISO: {e}; se reemplaza.");
            None
        }
    };
    let canonical = plan.canonical(&env);
    let change = match &previous {
        None => Change::First,
        Some(p) if p.hash == approval::fingerprint(&canonical) => Change::Same,
        Some(p) => Change::Changed(approval::diff(&p.plan, &canonical)),
    };
    print_summary(&plan, &env, &env_name, &change);

    let target = plan
        .hosts
        .iter()
        .find(|h| !is_loopback(h))
        .or_else(|| plan.hosts.iter().next())
        .expect("un plan válido tiene al menos un host");
    eprint!("\nPara aprobar, escribí el host destino ({target}): ");
    std::io::stderr().flush().ok();
    let mut typed = String::new();
    std::io::stdin().lock().read_line(&mut typed).ok();
    if !typed.trim().eq_ignore_ascii_case(target) {
        eprintln!("✗ no coincide con `{target}`: no se aprobó nada.");
        return exit::REFUSED;
    }

    let approved_by = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or("desconocido".into());
    let approved_at = jiff::Timestamp::now().to_string();
    let signed = match approval::approve(
        &plan,
        &env,
        &env_name,
        &approved_by,
        &approved_at,
        &OsKeyring,
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("✗ no se pudo firmar: {e}");
            return exit::RUNTIME;
        }
    };
    let file = approval::path(&project.approvals_dir(), &plan.scenario, &env_name);
    let saved = project.ensure_gitignore().and_then(|_| {
        std::fs::create_dir_all(project.approvals_dir())?;
        std::fs::write(&file, serde_json::to_string_pretty(&signed).unwrap())
    });
    match saved {
        Ok(()) => {
            eprintln!("✓ aprobado: {}", display(&file));
            eprintln!("  Ejecutalo con: lt run {} --env {env_name}", plan.scenario);
            exit::OK
        }
        Err(e) => {
            eprintln!("✗ no se pudo guardar la aprobación: {e}");
            exit::RUNTIME
        }
    }
}

/// Verifica la aprobación antes de `lt run`. Imprime el motivo si la rechaza.
pub fn verify_for_run(
    project: &Project,
    plan: &Plan,
    env: &Environment,
    env_name: &str,
) -> Result<Approval, ()> {
    let fail = |msg: String| {
        eprintln!("✗ {} en `{env_name}`: {msg}", plan.scenario);
        eprintln!(
            "  Una persona tiene que aprobarlo: lt approve {} --env {env_name}",
            plan.scenario
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
            fail("cambió desde la última aprobación".into());
            eprintln!("{}", limit_lines(&diff, 40));
            Err(())
        }
        Err(r) => {
            fail(r.to_string());
            Err(())
        }
    }
}

fn load(project: &Project, scenario: &str, env_name: &str) -> Result<Option<Approval>, String> {
    let file = approval::path(&project.approvals_dir(), scenario, env_name);
    match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{} está dañado ({e})", display(&file))),
        Err(_) => Ok(None),
    }
}

enum Change {
    First,
    Same,
    Changed(String),
}

fn print_summary(plan: &Plan, env: &Environment, env_name: &str, change: &Change) {
    let hosts: Vec<&str> = plan.hosts.iter().map(String::as_str).collect();
    let (duration, graceful_stop) = (plan.load.duration(), plan.load.graceful_stop());
    let status = match change {
        Change::First => "primera aprobación",
        Change::Same => "sin cambios desde la última aprobación",
        Change::Changed(_) => "CAMBIÓ desde la última aprobación",
    };

    eprintln!("Escenario   {}  ({status})", plan.scenario);
    eprintln!("Entorno     {env_name} → {}", hosts.join(", "));
    eprintln!("Carga       {}", plan.load.describe());
    if let Some(n) = plan.load.planned_iterations() {
        eprintln!("            ≈{n} iteraciones");
    }
    eprintln!("            {}", plan.rate_summary());
    eprintln!(
        "Duración    {} + {} de graceful stop",
        HumanDuration(duration),
        HumanDuration(graceful_stop)
    );
    for (i, d) in plan.data.iter().enumerate() {
        let label = if i == 0 { "Datos" } else { "" };
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
        eprintln!("Escribe     no");
    } else {
        eprintln!("Escribe     SÍ: {}", writes.join(", "));
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
        eprintln!("Límites     ninguno definido en el entorno");
    } else {
        eprintln!("Límites     {}", limits.join("  "));
    }

    if let Change::Changed(diff) = change {
        eprintln!(
            "\nCambios desde la última aprobación:\n{}",
            limit_lines(diff, 60)
        );
    }
}

fn limit_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.trim_end().to_string();
    }
    format!(
        "{}\n… ({} líneas más)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
