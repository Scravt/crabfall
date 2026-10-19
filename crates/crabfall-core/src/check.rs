//! Smoke check (ADR-004 §4): corre cada flow una vez, en serie, solo contra loopback.
//! Es lo único que un agente puede ejecutar sin aprobación.

use std::{collections::HashMap, time::Instant};

use serde::Serialize;
use serde_json::Value;

use crate::engine::{
    http::{Client, FailureKind},
    plan::{Plan, StepPlan, Vars},
    run_step,
};

/// Tope total de requests de un check.
pub const MAX_REQUESTS: usize = 50;
/// Bytes del body que se muestran por paso.
pub const MAX_BODY: usize = 2048;

#[derive(Debug, Serialize)]
pub struct CheckResult {
    /// Verdadero si todos los pasos ejecutados respondieron 2xx.
    pub ok: bool,
    pub steps: Vec<CheckStep>,
    /// Pasos que no se ejecutaron (por el tope o porque un paso anterior del flow falló).
    pub skipped: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckStep {
    pub flow: String,
    pub name: String,
    pub method: String,
    pub url: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    /// Variables extraídas (valores recortados).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extracted: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub body_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct CheckOutcome {
    pub check: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

/// Verifica que todos los hosts del plan resuelvan solo a direcciones de loopback.
pub async fn ensure_loopback(plan: &Plan) -> Result<(), String> {
    let mut checked = std::collections::BTreeSet::new();
    for step in plan.all_steps() {
        let o = &step.origin;
        if !checked.insert((&o.host, o.port)) {
            continue;
        }
        let host = o.host.trim_start_matches('[').trim_end_matches(']');
        let addrs: Vec<_> = tokio::net::lookup_host((host, o.port))
            .await
            .map_err(|e| format!("no se pudo resolver `{host}`: {e}"))?
            .collect();
        if addrs.is_empty() || addrs.iter().any(|a| !a.ip().is_loopback()) {
            return Err(format!(
                "`{host}` no resuelve solo a loopback: el smoke check únicamente corre contra la máquina local"
            ));
        }
    }
    Ok(())
}

pub async fn check(plan: &Plan) -> Result<CheckResult, String> {
    ensure_loopback(plan).await?;

    let client = Client::new();
    let mut steps = Vec::new();
    let mut skipped = Vec::new();

    // setup: una vez, en orden; si falla, no tiene sentido seguir.
    let mut setup_vars = HashMap::new();
    let mut setup_failed = false;
    for step in &plan.setup {
        if setup_failed {
            skipped.push(step.name.clone());
            continue;
        }
        let (result, extracted) =
            check_step(&client, plan, step, &setup_vars, &HashMap::new()).await;
        setup_failed = !result.ok;
        setup_vars.extend(extracted);
        steps.push(result);
    }

    // vu_setup: un solo VU, que después recorre los flows.
    let mut vu_vars = plan.vu_row_vars();
    let mut vu_failed = setup_failed;
    for step in &plan.vu_setup {
        if vu_failed {
            skipped.push(step.name.clone());
            continue;
        }
        let (result, extracted) = check_step(&client, plan, step, &setup_vars, &vu_vars).await;
        vu_failed = !result.ok;
        vu_vars.extend(extracted);
        steps.push(result);
    }

    for flow in &plan.flows {
        let mut locals = vu_vars.clone();
        locals.extend(plan.row_vars(flow));
        let mut flow_failed = vu_failed;
        for step in &flow.steps {
            if flow_failed || steps.len() >= MAX_REQUESTS {
                skipped.push(step.name.clone());
                continue;
            }
            let (result, extracted) = check_step(&client, plan, step, &setup_vars, &locals).await;
            flow_failed = result.failure.is_some();
            locals.extend(extracted);
            steps.push(result);
        }
    }

    // vu_refresh: una vez, después de los flows, con las variables del VU.
    let mut refresh_failed = vu_failed;
    for step in plan.refresh_steps() {
        if refresh_failed || steps.len() >= MAX_REQUESTS {
            skipped.push(step.name.clone());
            continue;
        }
        let (mut result, extracted) = check_step(&client, plan, step, &setup_vars, &vu_vars).await;
        // Sin pasos propios repite los de `vu_setup`: que se vea que es la renovación.
        result.flow = "vu_refresh".into();
        refresh_failed = !result.ok;
        vu_vars.extend(extracted);
        steps.push(result);
    }

    Ok(CheckResult {
        ok: steps.iter().all(|s| s.ok),
        steps,
        skipped,
    })
}

async fn check_step(
    client: &Client,
    plan: &Plan,
    step: &StepPlan,
    setup: &HashMap<String, Value>,
    locals: &HashMap<String, Value>,
) -> (CheckStep, Vec<(String, Value)>) {
    // La URL resuelta ayuda más que la plantilla para depurar.
    let url = match step.target(&Vars {
        plan,
        setup,
        locals,
    }) {
        Ok(target) => {
            let scheme = if step.origin.https { "https" } else { "http" };
            format!(
                "{scheme}://{}:{}{target}",
                step.origin.host, step.origin.port
            )
        }
        Err(_) => step.url(),
    };
    let start = Instant::now();
    let run = run_step(client, plan, step, setup, locals).await;
    let duration_ms = (start.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;

    let mut out = CheckStep {
        flow: step.flow.clone(),
        name: step.name.clone(),
        method: step.method.to_string(),
        url,
        ok: false,
        status: None,
        duration_ms,
        failure: None,
        extracted: run
            .extracted
            .iter()
            .map(|(n, v)| (n.clone(), preview(v)))
            .collect(),
        checks: vec![],
        content_type: None,
        body: None,
        body_truncated: false,
    };
    match run.result {
        Ok(r) => {
            // Con checks de status, esos reemplazan la expectativa de 2xx.
            let status_ok =
                step.checks.iter().any(|c| c.is_status()) || (200..300).contains(&r.status);
            out.checks = step
                .checks
                .iter()
                .zip(&run.checks)
                .map(|(c, r)| CheckOutcome {
                    check: c.label.clone(),
                    passed: r.is_none(),
                    reason: r.clone(),
                })
                .collect();
            out.ok =
                status_ok && out.checks.iter().all(|c| c.passed) && run.extract_error.is_none();
            out.status = Some(r.status);
            out.content_type = r.content_type;
            out.body_truncated = r.body.len() > MAX_BODY;
            let shown = &r.body[..r.body.len().min(MAX_BODY)];
            out.body = Some(String::from_utf8_lossy(shown).into_owned());
            if let Some(e) = run.extract_error {
                out.failure = Some(Failure {
                    kind: FailureKind::Extract,
                    message: e,
                });
            }
        }
        Err(f) => {
            out.failure = Some(Failure {
                kind: f.kind,
                message: f.message,
            })
        }
    }
    (out, run.extracted)
}

/// Valor extraído, recortado: alcanza para ver que el `extract` funcionó.
fn preview(v: &Value) -> String {
    let text = crate::template::to_text(v);
    match text.char_indices().nth(40) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}
