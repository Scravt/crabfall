//! Motor de hallazgos deterministas (rebanada 5).
//!
//! Cada regla lee lo medido (series por segundo, métricas por paso, totales) y, si
//! detecta un patrón, devuelve un hallazgo con su evidencia. No interpreta el código:
//! eso lo hace el agente a partir de `source`, y lo marca como hipótesis.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    engine::http::FailureKind,
    load::LoadPlan,
    metrics::{Point, RunResult, StepPoint, StepResult},
    scenario::Source,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub from_s: u32,
    pub to_s: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    /// Una línea, con los números que importan.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    /// Métricas que sostienen el hallazgo.
    pub evidence: Value,
    /// Causas probables a investigar, de más a menos probable. No son conclusiones.
    pub causes: Vec<String>,
}

/// Umbrales de las reglas. Están juntos para poder ajustarlos con el laboratorio.
const MAX_LAG_P99_MS: f64 = 5.0;
const DEGRADED_FACTOR: f64 = 3.0;
const DEGRADED_MIN_DELTA_MS: f64 = 50.0;
const ERROR_RATE_HIGH: f64 = 0.05;
const ERROR_RATE_LOW: f64 = 0.01;
const DROPPED_MAX_RATIO: f64 = 0.01;
const DRIFT_FACTOR: f64 = 2.0;
const DRIFT_MIN_DELTA_MS: f64 = 20.0;
const DRIFT_MIN_R2: f64 = 0.6;
const TAIL_RATIO: f64 = 10.0;
const TAIL_MIN_DELTA_MS: f64 = 50.0;
const TAIL_MIN_REQUESTS: u64 = 200;
const NEW_CONN_RATIO: f64 = 0.10;
const NEW_CONN_MIN_REQUESTS: u64 = 100;

pub fn diagnose(load: &LoadPlan, r: &RunResult) -> Vec<Finding> {
    if r.aborted.is_some() {
        return vec![];
    }
    // Solo las ventanas de la carga, sin la cola de `graceful_stop`.
    let windows = (load.duration().as_secs_f64().ceil() as usize).min(r.series.points.len());
    let points = &r.series.points[..windows];
    let ramping = matches!(load, LoadPlan::Open { ramping: true, .. });

    let mut out = Vec::new();
    out.extend(invalid_result(r));
    let mut saturated = false;
    if ramping {
        if let Some(f) = breaking_point(r, points) {
            saturated = true;
            out.push(f);
        }
    } else {
        if let Some(f) = capacity_exceeded(r, points) {
            saturated = true;
            out.push(f);
        }
        out.extend(degradation_over_time(r, points));
    }
    for (step, series) in r.steps.iter().zip(&r.series.steps) {
        out.extend(error_threshold(
            step,
            &series.points[..windows.min(series.points.len())],
            points,
            Session::of(r),
        ));
        if !saturated {
            out.extend(long_tail(step));
        }
        out.extend(new_connections(step, load.concurrency()));
    }
    out.sort_by_key(|f| f.severity);
    out
}

fn invalid_result(r: &RunResult) -> Option<Finding> {
    let lag = r.scheduler_lag_ms?;
    (lag.p99 > MAX_LAG_P99_MS).then(|| Finding {
        id: "invalid_result".into(),
        severity: Severity::High,
        title: format!(
            "Resultado no confiable: el generador llegó tarde (lag p99 {} ms). No saques conclusiones de esta corrida.",
            lag.p99
        ),
        step: None,
        source: None,
        window: None,
        evidence: json!({ "scheduler_lag_ms": { "p50": lag.p50, "p99": lag.p99, "max": lag.max } }),
        causes: vec![
            "La máquina que genera la carga está saturada (CPU) o compartida con el servidor.".into(),
            "Repetir con menos carga, o generar desde otra máquina.".into(),
        ],
    })
}

/// Iteraciones planificadas en la ventana: iniciadas + descartadas.
fn offered(p: &Point) -> u64 {
    p.started + p.dropped
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    Some(values[values.len() / 2])
}

/// p95 típico de las primeras ventanas con datos: la referencia "sana".
fn baseline<T>(points: &[T], p95: impl Fn(&T) -> Option<f64>) -> Option<f64> {
    let with_data: Vec<f64> = points.iter().filter_map(&p95).collect();
    let n = (with_data.len() / 10).max(3).min(with_data.len());
    median(with_data[..n].to_vec())
}

fn degraded(p: &Point, base: f64) -> bool {
    let error_rate = if p.requests > 0 {
        p.errors as f64 / p.requests as f64
    } else {
        0.0
    };
    let slow = p
        .p95
        .is_some_and(|v| v > (base * DEGRADED_FACTOR).max(base + DEGRADED_MIN_DELTA_MS));
    p.dropped > 0 || slow || error_rate >= ERROR_RATE_HIGH
}

/// Primera ventana degradada que se sostiene (2 de 3 desde ella).
fn first_sustained(flags: &[bool]) -> Option<usize> {
    (0..flags.len()).find(|&i| {
        let next = &flags[i..(i + 3).min(flags.len())];
        flags[i] && next.iter().filter(|f| **f).count() >= next.len().min(2)
    })
}

/// El paso que más empeoró desde su base en las ventanas `from..`.
fn culprit(r: &RunResult, from: usize) -> Option<(&StepResult, f64, f64, f64)> {
    r.steps
        .iter()
        .zip(&r.series.steps)
        .filter_map(|(step, series)| {
            let base = baseline(
                &series.points[..from.min(series.points.len()).max(1)],
                |p: &StepPoint| p.p95,
            )?;
            let after = median(
                series
                    .points
                    .iter()
                    .skip(from)
                    .take(3)
                    .filter_map(|p| p.p95)
                    .collect(),
            )?;
            Some((step, base, after, after / base.max(0.001)))
        })
        .max_by(|a, b| a.3.total_cmp(&b.3))
}

/// Pasos cuyo p95 al menos se duplicó desde `from`: si son varios, el recurso es compartido.
fn affected_steps(r: &RunResult, from: usize) -> Vec<String> {
    r.steps
        .iter()
        .zip(&r.series.steps)
        .filter(|(_, series)| {
            let base = baseline(
                &series.points[..from.min(series.points.len()).max(1)],
                |p: &StepPoint| p.p95,
            );
            let after = median(
                series
                    .points
                    .iter()
                    .skip(from)
                    .take(3)
                    .filter_map(|p| p.p95)
                    .collect(),
            );
            matches!((base, after), (Some(b), Some(a)) if a >= 2.0 * b && a - b >= 10.0)
        })
        .map(|(step, _)| step.name.clone())
        .collect()
}

fn saturation_causes(affected: &[String]) -> Vec<String> {
    let mut causes = Vec::new();
    if affected.len() >= 2 {
        causes.push(format!(
            "Varios pasos se degradan juntos ({}): recurso compartido (CPU, event loop, pool de conexiones a la base, thread pool).",
            affected.join(", ")
        ));
    } else {
        causes.push("Un recurso propio de este endpoint se agota: pool de conexiones, lock, servicio externo con concurrencia limitada.".into());
    }
    causes.push(
        "Trabajo de CPU sincrónico (hashing, serialización, regex) que bloquea mientras atiende."
            .into(),
    );
    causes.push("Una cola interna que crece cuando la llegada supera la capacidad.".into());
    causes
}

fn breaking_point(r: &RunResult, points: &[Point]) -> Option<Finding> {
    let base = baseline(points, |p: &Point| p.p95)?;
    let flags: Vec<bool> = points.iter().map(|p| degraded(p, base)).collect();
    let t = first_sustained(&flags)?;
    let at = &points[t];
    let after = &points[t..];
    let p95_after = median(after.iter().take(3).filter_map(|p| p.p95).collect());
    let dropped_after: u64 = after.iter().map(|p| p.dropped).sum();
    let max_throughput = points.iter().map(|p| p.requests).max().unwrap_or(0);
    let affected = affected_steps(r, t);
    let culprit = culprit(r, t);

    let mut title = format!(
        "Punto de quiebre en ~{} it/s ({} req/s): el p95 pasa de {base} ms a {} ms",
        offered(at),
        at.requests,
        p95_after.map_or("?".into(), |v| v.to_string())
    );
    if dropped_after > 0 {
        title.push_str(&format!(" y se descartan {dropped_after} iteraciones"));
    }
    if let Some((step, ..)) = culprit {
        title.push_str(&format!("; el que más empeora es `{}`", step.name));
    }
    Some(Finding {
        id: "breaking_point".into(),
        severity: Severity::High,
        title,
        step: culprit.map(|(s, ..)| s.name.clone()),
        source: culprit.and_then(|(s, ..)| s.source.clone()),
        window: Some(Window {
            from_s: at.t,
            to_s: points.last().map_or(at.t, |p| p.t),
        }),
        evidence: json!({
            "offered_iterations_per_s": offered(at),
            "requests_per_s_at_break": at.requests,
            "max_requests_per_s": max_throughput,
            "baseline_p95_ms": base,
            "p95_ms_after_break": p95_after,
            "dropped_after_break": dropped_after,
            "steps_degraded": affected,
            "culprit": culprit.map(|(s, b, a, ratio)| json!({ "step": s.name, "baseline_p95_ms": b, "p95_ms": a, "ratio": (ratio * 10.0).round() / 10.0 })),
        }),
        causes: saturation_causes(&affected),
    })
}

fn capacity_exceeded(r: &RunResult, points: &[Point]) -> Option<Finding> {
    let planned = r.totals.planned.max(1);
    let ratio = r.totals.dropped as f64 / planned as f64;
    if ratio <= DROPPED_MAX_RATIO {
        return None;
    }
    let first = points.iter().position(|p| p.dropped > 0).unwrap_or(0);
    let saturated: Vec<&Point> = points.iter().skip(first).collect();
    let offered_rate = median(saturated.iter().map(|p| offered(p) as f64).collect()).unwrap_or(0.0);
    let sustained = median(saturated.iter().map(|p| p.started as f64).collect()).unwrap_or(0.0);
    let affected = affected_steps(r, first);
    let culprit = culprit(r, first).or_else(|| {
        r.steps
            .iter()
            .filter_map(|s| s.latency_ms.map(|l| (s, l.p95, l.p95, 1.0)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    });
    Some(Finding {
        id: "capacity_exceeded".into(),
        severity: Severity::High,
        title: format!(
            "La tasa pedida ({offered_rate:.0} it/s) supera la capacidad: se sostuvieron ~{sustained:.0} it/s y se descartó el {:.1}% de las iteraciones",
            ratio * 100.0
        ),
        step: culprit.map(|(s, ..)| s.name.clone()),
        source: culprit.and_then(|(s, ..)| s.source.clone()),
        window: Some(Window {
            from_s: points.get(first).map_or(0, |p| p.t),
            to_s: points.last().map_or(0, |p| p.t),
        }),
        evidence: json!({
            "offered_iterations_per_s": offered_rate,
            "sustained_iterations_per_s": sustained,
            "dropped": r.totals.dropped,
            "dropped_ratio": (ratio * 1000.0).round() / 1000.0,
            "steps_degraded": affected,
        }),
        causes: saturation_causes(&affected),
    })
}

/// Recta de mínimos cuadrados: (valor ajustado al inicio, al final, R²).
fn trend(values: &[(f64, f64)]) -> Option<(f64, f64, f64)> {
    if values.len() < 4 {
        return None;
    }
    let n = values.len() as f64;
    let (sx, sy) = values
        .iter()
        .fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let sxx: f64 = values.iter().map(|(x, _)| (x - mx).powi(2)).sum();
    let sxy: f64 = values.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
    let syy: f64 = values.iter().map(|(_, y)| (y - my).powi(2)).sum();
    if sxx == 0.0 || syy == 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let r2 = sxy * sxy / (sxx * syy);
    let (first, last) = (values[0].0, values[values.len() - 1].0);
    Some((my + slope * (first - mx), my + slope * (last - mx), r2))
}

/// p95 por ventana como puntos (t, ms), salteando la primera si hay suficientes
/// (conexiones nuevas, calentamiento).
fn p95_points<T>(
    points: &[T],
    t: impl Fn(&T) -> u32,
    p95: impl Fn(&T) -> Option<f64>,
) -> Vec<(f64, f64)> {
    let skip = usize::from(points.len() >= 6);
    points
        .iter()
        .skip(skip)
        .filter_map(|p| Some((t(p) as f64, p95(p)?)))
        .collect()
}

fn degradation_over_time(r: &RunResult, points: &[Point]) -> Option<Finding> {
    // Con descartes, la latencia ya está explicada por la saturación.
    if points.len() < 5 || r.totals.dropped as f64 > r.totals.planned as f64 * DROPPED_MAX_RATIO {
        return None;
    }
    let (start, end, r2) = trend(&p95_points(points, |p| p.t, |p| p.p95))?;
    if r2 < DRIFT_MIN_R2
        || end < start.max(0.001) * DRIFT_FACTOR
        || end - start < DRIFT_MIN_DELTA_MS
    {
        return None;
    }
    let culprit = r
        .steps
        .iter()
        .zip(&r.series.steps)
        .filter_map(|(s, series)| {
            let pts = &series.points[..points.len().min(series.points.len())];
            let (a, b, _) = trend(&p95_points(pts, |p| p.t, |p| p.p95))?;
            Some((s, b - a))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| s);
    let round1 = |x: f64| (x * 10.0).round() / 10.0;
    Some(Finding {
        id: "degradation_over_time".into(),
        severity: Severity::Medium,
        title: format!(
            "Con la carga constante, el p95 sube de forma sostenida: de ~{} ms a ~{} ms a lo largo de la corrida",
            round1(start),
            round1(end)
        ),
        step: culprit.map(|s| s.name.clone()),
        source: culprit.and_then(|s| s.source.clone()),
        window: Some(Window { from_s: points[0].t, to_s: points[points.len() - 1].t }),
        evidence: json!({
            "p95_ms_trend_start": round1(start),
            "p95_ms_trend_end": round1(end),
            "r2": (r2 * 100.0).round() / 100.0,
            "p95_ms_by_second": points.iter().map(|p| p.p95).collect::<Vec<_>>(),
        }),
        causes: vec![
            "Algo crece con cada request: una tabla o colección sin índice, una caché sin límite, una cola.".into(),
            "Fuga de memoria que aumenta el trabajo del GC.".into(),
            "Recursos que no se liberan (conexiones, handles) y se agotan de a poco.".into(),
        ],
    })
}

/// Cómo obtiene su token el escenario: decide qué decir de un 401 que aparece a mitad de corrida.
#[derive(Clone, Copy)]
enum Session {
    None,
    /// `setup`: un token para todos.
    Shared,
    /// `vu_setup` sin `vu_refresh`.
    PerVu,
    /// `vu_setup` con `vu_refresh`.
    Renewed,
}

impl Session {
    fn of(r: &RunResult) -> Self {
        match (&r.vu_setup, &r.vu_refresh) {
            (_, Some(_)) => Self::Renewed,
            (Some(_), None) => Self::PerVu,
            (None, None) if !r.setup.is_empty() => Self::Shared,
            _ => Self::None,
        }
    }

    /// Por qué vence la sesión y qué hacer, según cómo se obtuvo el token.
    fn expired(self, at: u32) -> Option<String> {
        Some(match self {
            Self::None => return None,
            Self::Shared => format!(
                "El token de `setup` venció a los ~{at} s, y es uno solo para todos: pasá el login a `vu_setup` y agregá `vu_refresh` (ADR-011)."
            ),
            Self::PerVu => format!(
                "El token de cada VU venció a los ~{at} s: agregá `vu_refresh` (o `--refresh-every`) con `every` menor que su vida menos lo que dura una iteración (ADR-011)."
            ),
            Self::Renewed => format!(
                "Los tokens vencen antes de que `vu_refresh` los renueve (a los ~{at} s): bajá `every` por debajo de su vida menos lo que dura una iteración, o revisá que la renovación devuelva un token nuevo."
            ),
        })
    }
}

fn error_threshold(
    step: &StepResult,
    series: &[StepPoint],
    global: &[Point],
    session: Session,
) -> Option<Finding> {
    let rate = |p: &StepPoint| {
        if p.requests > 0 {
            p.errors as f64 / p.requests as f64
        } else {
            0.0
        }
    };
    let flags: Vec<bool> = series
        .iter()
        .map(|p| p.requests >= 5 && rate(p) >= ERROR_RATE_HIGH)
        .collect();
    let sustained = first_sustained(&flags)?;
    // Con pocas requests por segundo (pausas largas), los errores empiezan antes del primer
    // segundo que alcanza el mínimo: la ventana arranca en el primero que ya fallaba.
    let mut t = sustained;
    for i in (0..sustained).rev() {
        let p = &series[i];
        if p.requests == 0 {
            continue;
        }
        if rate(p) < ERROR_RATE_HIGH {
            break;
        }
        t = i;
    }
    let before: u64 = series[..t].iter().map(|p| p.requests).sum();
    let errors_before: u64 = series[..t].iter().map(|p| p.errors).sum();
    let from_start = before == 0 || errors_before as f64 / before as f64 >= ERROR_RATE_LOW;

    let codes: Vec<(&String, &u64)> = step
        .status_codes
        .iter()
        .filter(|(c, _)| c.as_str() >= "400")
        .collect();
    let dominant_code = codes
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(c, _)| c.as_str());
    let dominant_failure = step
        .failures
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(k, _)| *k);
    let offered_at = global.get(t).map_or(0, offered);
    // La tasa en toda la ventana: con pocas requests, la de un solo segundo es ruido.
    let (window_requests, window_errors) = series[t..]
        .iter()
        .fold((0, 0), |(r, e), p| (r + p.requests, e + p.errors));
    let rate_at = window_errors as f64 / window_requests.max(1) as f64;
    let expired = (dominant_code == Some("401") && !from_start)
        .then(|| session.expired(series[t].t))
        .flatten();

    let what = match (dominant_code, dominant_failure) {
        (Some(code), _) => format!("respuestas {code}"),
        (None, Some(kind)) => format!(
            "fallas de tipo {}",
            serde_json::to_value(kind).unwrap().as_str().unwrap_or("?")
        ),
        _ => "errores".into(),
    };
    let title = if expired.is_some() {
        format!(
            "`{}` recibe 401 desde el segundo {} ({:.0}%): venció la sesión",
            step.name,
            series[t].t,
            rate_at * 100.0
        )
    } else if from_start {
        format!(
            "`{}` falla desde el inicio ({what}, {:.0}%): no parece un efecto de la carga",
            step.name,
            rate_at * 100.0
        )
    } else {
        format!(
            "`{}` empieza a fallar con ~{offered_at} it/s: {what} ({:.0}% desde el segundo {})",
            step.name,
            rate_at * 100.0,
            series[t].t
        )
    };
    let mut causes: Vec<String> = match (dominant_code, dominant_failure) {
        (Some("429"), _) => vec!["Un rate limiter (de la aplicación, del gateway o de un proveedor) corta por encima de cierta tasa.".into()],
        (Some("502" | "503" | "504"), _) => vec![
            "Un proxy o balanceador no consigue un backend disponible: backends saturados o pool del proxy agotado.".into(),
        ],
        (Some(code), _) if code.starts_with('5') => vec![
            "La aplicación lanza errores bajo carga: timeouts internos, pool de base agotado, excepciones por concurrencia.".into(),
        ],
        (Some(code), _) if code.starts_with('4') => vec![
            "Errores del cliente que aparecen con la carga: datos que se agotan (stock, IDs), validaciones de concurrencia, tokens que vencen.".into(),
        ],
        (None, Some(FailureKind::Timeout)) => vec!["El servidor no responde a tiempo: las requests hacen cola más allá del timeout.".into()],
        (None, Some(FailureKind::Connect | FailureKind::Reset)) => vec![
            "Límite de conexiones: backlog del socket, límite de file descriptors o del proxy.".into(),
        ],
        (None, Some(FailureKind::Extract)) => vec!["La respuesta cambia bajo carga y el `extract` no encuentra el valor (¿una respuesta de error?).".into()],
        _ => vec![],
    };
    if let Some(why) = expired {
        // Reemplaza a la causa genérica de los 4xx, que ya mencionaba los tokens.
        causes = vec![why];
    }
    // Si el servidor declaró su límite, el hallazgo lo dice con números.
    let limit = step
        .rate_limit
        .as_ref()
        .filter(|_| dominant_code == Some("429"));
    if let Some(l) = limit {
        let declared: Vec<String> = [
            l.policy()
                .map(|p| format!("declara {p} (`RateLimit-Policy`)")),
            l.retry_after().map(|r| format!("pide esperar con `{r}`")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !declared.is_empty() {
            causes = vec![format!(
                "El rate limiter del servidor corta: {}. El límite suele ser por clave (IP, usuario o token): con una sola IP y pocos usuarios, la carga lo alcanza antes que el tráfico real.",
                declared.join(" y ")
            )];
        }
    }
    if from_start {
        causes.insert(0, "Un error funcional del escenario o de la API (URL, datos, auth): revisalo con `crabfall check`.".into());
    }
    Some(Finding {
        id: "error_threshold".into(),
        severity: Severity::High,
        title,
        step: Some(step.name.clone()),
        source: step.source.clone(),
        window: Some(Window {
            from_s: series[t].t,
            to_s: series.last().map_or(series[t].t, |p| p.t),
        }),
        evidence: json!({
            "offered_iterations_per_s": offered_at,
            "error_rate_in_window": (rate_at * 1000.0).round() / 1000.0,
            "error_rate_before": if before > 0 { (errors_before as f64 / before as f64 * 1000.0).round() / 1000.0 } else { 0.0 },
            "status_codes": step.status_codes,
            "failures": step.failures,
            "error_samples": step.error_samples,
            "rate_limit": limit,
        }),
        causes,
    })
}

fn long_tail(step: &StepResult) -> Option<Finding> {
    let l = step.latency_ms?;
    if l.count < TAIL_MIN_REQUESTS
        || l.p99 < l.p50 * TAIL_RATIO
        || l.p99 - l.p50 < TAIL_MIN_DELTA_MS
    {
        return None;
    }
    Some(Finding {
        id: "long_tail".into(),
        severity: Severity::Medium,
        title: format!(
            "`{}` tiene cola larga: p50 {} ms, p99 {} ms ({:.0}×) con la carga estable",
            step.name,
            l.p50,
            l.p99,
            l.p99 / l.p50.max(0.001)
        ),
        step: Some(step.name.clone()),
        source: step.source.clone(),
        window: None,
        evidence: json!({ "latency_ms": l }),
        causes: vec![
            "Algunas requests toman un camino lento: queries sin índice para ciertos parámetros, caché que falla, llamadas externas ocasionales.".into(),
            "Pausas del runtime (GC) o contención por un lock.".into(),
            "Conexiones nuevas (TLS, pool) en una fracción de las requests.".into(),
        ],
    })
}

/// El pool de conexiones es compartido y crece hasta la concurrencia: abrir una conexión
/// por VU es normal. Solo cuentan las que exceden ese tope, que se reabrieron.
fn new_connections(step: &StepResult, vus: u32) -> Option<Finding> {
    let connects = step.connect_ms.map_or(0, |c| c.count);
    let reopened = connects.saturating_sub(u64::from(vus));
    if step.requests < NEW_CONN_MIN_REQUESTS
        || (reopened as f64) < step.requests as f64 * NEW_CONN_RATIO
    {
        return None;
    }
    let ratio = connects as f64 / step.requests as f64;
    Some(Finding {
        id: "new_connections".into(),
        severity: Severity::Medium,
        title: format!(
            "`{}` abre conexión nueva en el {:.0}% de las requests ({connects} conexiones con {vus} VUs): no se reutilizan (keep-alive)",
            step.name,
            ratio * 100.0
        ),
        step: Some(step.name.clone()),
        source: step.source.clone(),
        window: None,
        evidence: json!({
            "requests": step.requests,
            "new_connections": connects,
            "vus": vus,
            "connect_ms": step.connect_ms,
            "tls_ms": step.tls_ms,
        }),
        causes: vec![
            "El servidor responde con `Connection: close` o cierra la conexión después de cada respuesta.".into(),
            "Un proxy con keep-alive deshabilitado o con un timeout de inactividad muy corto.".into(),
            "Cada conexión nueva suma connect y TLS a la latencia y consume puertos y file descriptors.".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        load::Segment,
        metrics::{Percentiles, Series, Totals},
    };

    fn result(lag_p99: f64) -> RunResult {
        let lag = Percentiles {
            count: 100,
            mean: 1.0,
            p50: 1.0,
            p90: 1.0,
            p95: 1.0,
            p99: lag_p99,
            p999: lag_p99,
            max: lag_p99,
        };
        RunResult {
            passed: true,
            duration_s: 1.0,
            aborted: None,
            setup: vec![],
            vu_setup: None,
            vu_refresh: None,
            totals: Totals::default(),
            thresholds: vec![],
            scheduler_lag_ms: Some(lag),
            steps: vec![],
            warnings: vec![],
            findings: vec![],
            series: Series::default(),
        }
    }

    fn load() -> LoadPlan {
        let s = Segment {
            from: 10.0,
            to: 10.0,
            duration: Duration::from_secs(1),
        };
        LoadPlan::Open {
            ramping: false,
            segments: vec![s],
            max_vus: 1,
            graceful_stop: Duration::ZERO,
        }
    }

    #[test]
    fn late_generator_invalidates_the_result() {
        let findings = diagnose(&load(), &result(12.0));
        assert_eq!(findings[0].id, "invalid_result");
        assert!(diagnose(&load(), &result(2.0)).is_empty());
    }

    #[test]
    fn trend_fits_a_line() {
        let (start, end, r2) =
            trend(&[(0.0, 10.0), (1.0, 20.0), (2.0, 30.0), (3.0, 40.0)]).unwrap();
        assert!(
            (start - 10.0).abs() < 1e-9 && (end - 40.0).abs() < 1e-9 && (r2 - 1.0).abs() < 1e-9
        );
        assert!(trend(&[(0.0, 5.0), (1.0, 5.0), (2.0, 5.0), (3.0, 5.0)]).is_none());
    }
}
