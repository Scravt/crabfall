//! Smoke check (ADR-004 §4): corre cada flow una vez, en serie, solo contra loopback.
//! Es lo único que un agente puede ejecutar sin aprobación.

use std::time::Instant;

use serde::Serialize;
use tokio::time::timeout;

use crate::engine::{
    http::{Client, FailureKind},
    plan::{Plan, StepPlan},
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub body_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

/// Verifica que todos los hosts del plan resuelvan solo a direcciones de loopback.
pub async fn ensure_loopback(plan: &Plan) -> Result<(), String> {
    let mut checked = std::collections::BTreeSet::new();
    for step in plan.steps() {
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

    for flow in &plan.flows {
        let mut flow_failed = false;
        for step in &flow.steps {
            if flow_failed || steps.len() >= MAX_REQUESTS {
                skipped.push(step.name.clone());
                continue;
            }
            let result = run_step(&client, step).await;
            flow_failed = result.failure.is_some();
            steps.push(result);
        }
    }

    Ok(CheckResult {
        ok: steps.iter().all(|s| s.ok),
        steps,
        skipped,
    })
}

async fn run_step(client: &Client, step: &StepPlan) -> CheckStep {
    let start = Instant::now();
    let result = timeout(step.timeout, client.send(&step.origin, || step.request())).await;
    let duration_ms = (start.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;

    let mut out = CheckStep {
        flow: step.flow.clone(),
        name: step.name.clone(),
        method: step.method.to_string(),
        url: step.url(),
        ok: false,
        status: None,
        duration_ms,
        failure: None,
        content_type: None,
        body: None,
        body_truncated: false,
    };
    match result {
        Ok(Ok(r)) => {
            out.ok = (200..300).contains(&r.status);
            out.status = Some(r.status);
            out.content_type = r.content_type;
            out.body_truncated = r.body.len() > MAX_BODY;
            let shown = &r.body[..r.body.len().min(MAX_BODY)];
            out.body = Some(String::from_utf8_lossy(shown).into_owned());
        }
        Ok(Err(f)) => {
            out.failure = Some(Failure {
                kind: f.kind,
                message: f.message,
            })
        }
        Err(_) => {
            out.failure = Some(Failure {
                kind: FailureKind::Timeout,
                message: format!(
                    "sin respuesta en {}",
                    crate::units::HumanDuration(step.timeout)
                ),
            })
        }
    }
    out
}
