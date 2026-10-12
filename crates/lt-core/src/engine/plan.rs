//! Plan ejecutable: escenario validado + entorno resuelto.

use std::{collections::BTreeSet, time::Duration};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    HeaderMap, Method, Request,
    header::{CONTENT_TYPE, HOST, HeaderName, HeaderValue, USER_AGENT},
};
use serde_json::{Value, json};
use url::Url;

use super::http::{Body, Origin};
use crate::{
    config::Environment,
    scenario::{LoadPlan, Scenario, Source},
    units::HumanDuration,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Plan {
    pub scenario: String,
    pub flows: Vec<FlowPlan>,
    pub load: LoadPlan,
    /// Hosts a los que se va a conectar.
    pub hosts: BTreeSet<String>,
    pub max_request_rate: f64,
}

pub struct FlowPlan {
    pub name: String,
    pub weight: u32,
    pub steps: Vec<StepPlan>,
}

pub struct StepPlan {
    /// Índice global del paso: clave de las métricas.
    pub id: usize,
    pub name: String,
    pub flow: String,
    pub tags: Vec<String>,
    pub source: Option<Source>,
    pub method: Method,
    pub origin: Origin,
    /// Path + query, para la línea de request.
    pub target: String,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub timeout: Duration,
}

impl StepPlan {
    pub fn request(&self) -> Request<Body> {
        let mut request = Request::builder()
            .method(self.method.clone())
            .uri(self.target.as_str())
            .body(Full::new(self.body.clone()))
            .expect("validado al armar el plan");
        *request.headers_mut() = self.headers.clone();
        request
    }

    pub fn url(&self) -> String {
        let scheme = if self.origin.https { "https" } else { "http" };
        format!(
            "{scheme}://{}:{}{}",
            self.origin.host, self.origin.port, self.target
        )
    }
}

impl Plan {
    /// Arma el plan: resuelve URLs y verifica `allowed_hosts`. No aplica los límites
    /// (ver `limit_violations`). Requiere un escenario que pasó `validate`.
    pub fn build(scenario: &Scenario, env: &Environment) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        let base = Url::parse(&env.base_url)
            .map_err(|e| vec![format!("base_url `{}` inválida: {e}", env.base_url)])?;
        let allowed: BTreeSet<String> = env
            .allowed_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();

        let mut hosts = BTreeSet::new();
        let mut flows = Vec::new();
        let mut id = 0;
        for (flow_name, flow) in &scenario.flows {
            let mut steps = Vec::new();
            for step in &flow.steps {
                match build_step(id, flow_name, step, &base, &allowed) {
                    Ok(plan) => {
                        hosts.insert(plan.origin.host.clone());
                        steps.push(plan);
                    }
                    Err(e) => errors.push(format!("{}: {e}", step.name)),
                }
                id += 1;
            }
            flows.push(FlowPlan {
                name: flow_name.clone(),
                weight: flow.weight,
                steps,
            });
        }

        let plan = Self {
            scenario: scenario.name.clone(),
            flows,
            load: scenario.load.plan(),
            hosts,
            max_request_rate: scenario.max_request_rate(),
        };
        if errors.is_empty() {
            Ok(plan)
        } else {
            Err(errors)
        }
    }

    /// Representación canónica de todo lo que define el tráfico: es lo que firma una
    /// aprobación (ADR-004 §1). Incluye el entorno resuelto; excluye `description`,
    /// `meta` y `source`, que no cambian el tráfico. El orden de las claves es fijo.
    pub fn canonical(&self, env: &Environment) -> Value {
        let LoadPlan::ConstantRate {
            rate,
            duration,
            max_vus,
            graceful_stop,
        } = self.load;
        let d = |d: Duration| HumanDuration(d).to_string();
        let mut allowed: Vec<String> = env
            .allowed_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        allowed.sort();
        allowed.dedup();
        let flows: Vec<Value> = self
            .flows
            .iter()
            .map(|f| {
                let steps: Vec<Value> = f
                    .steps
                    .iter()
                    .map(|s| {
                        let mut headers: Vec<(String, String)> = s
                            .headers
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.to_string(),
                                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                                )
                            })
                            .collect();
                        headers.sort();
                        json!({
                            "name": s.name,
                            "method": s.method.as_str(),
                            "url": s.url(),
                            "headers": headers,
                            "body": String::from_utf8_lossy(&s.body),
                            "timeout": d(s.timeout),
                            "tags": s.tags,
                        })
                    })
                    .collect();
                json!({ "name": f.name, "weight": f.weight, "steps": steps })
            })
            .collect();
        json!({
            "scenario": self.scenario,
            "environment": {
                "base_url": env.base_url,
                "allowed_hosts": allowed,
                "vars": env.vars.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                "limits": {
                    "max_rps": env.limits.max_rps,
                    "max_vus": env.limits.max_vus,
                    "max_duration": env.limits.max_duration.map(|m| m.to_string()),
                },
            },
            "load": {
                "executor": "constant_rate",
                "rate": rate.to_string(),
                "duration": d(duration),
                "max_vus": max_vus,
                "graceful_stop": d(graceful_stop),
            },
            "flows": flows,
        })
    }

    pub fn steps(&self) -> impl Iterator<Item = &StepPlan> {
        self.flows.iter().flat_map(|f| &f.steps)
    }

    /// Límites duros del entorno (ADR-004). `run` los exige; `check` no los necesita
    /// porque ignora `load`.
    pub fn limit_violations(&self, env: &Environment) -> Vec<String> {
        let LoadPlan::ConstantRate {
            duration, max_vus, ..
        } = self.load;
        let limits = &env.limits;
        let mut out = Vec::new();
        if let Some(max) = limits.max_rps
            && self.max_request_rate > max
        {
            out.push(format!(
                "la tasa máxima de requests ({:.1}/s) supera limits.max_rps ({max})",
                self.max_request_rate
            ));
        }
        if let Some(max) = limits.max_vus
            && max_vus > max
        {
            out.push(format!(
                "load.max_vus ({max_vus}) supera limits.max_vus ({max})"
            ));
        }
        if let Some(max) = limits.max_duration
            && duration > max.0
        {
            out.push(format!("load.duration supera limits.max_duration ({max})"));
        }
        out
    }

    /// Verdadero si todos los hosts son loopback (literal: `localhost` o una IP de loopback).
    pub fn loopback_only(&self) -> bool {
        self.hosts.iter().all(|h| is_loopback(h))
    }
}

pub fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn build_step(
    id: usize,
    flow: &str,
    step: &crate::scenario::Step,
    base: &Url,
    allowed: &BTreeSet<String>,
) -> Result<StepPlan, String> {
    let r = &step.request;
    let mut url = resolve_url(base, &r.url)?;
    if !r.query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in &r.query {
            let value = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            pairs.append_pair(key, &value);
        }
    }

    let https = match url.scheme() {
        "http" => false,
        "https" => true,
        other => return Err(format!("esquema `{other}` no soportado")),
    };
    let host = url
        .host_str()
        .ok_or("la URL no tiene host")?
        .to_ascii_lowercase();
    if !allowed.contains(&host) {
        return Err(format!(
            "el host `{host}` no está en allowed_hosts del entorno"
        ));
    }
    let port = url
        .port_or_known_default()
        .ok_or("la URL no tiene puerto")?;
    let target = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };

    let mut headers = HeaderMap::new();
    let authority = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    headers.insert(
        HOST,
        HeaderValue::from_str(&authority).map_err(|e| e.to_string())?,
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("lt/", env!("CARGO_PKG_VERSION"))),
    );

    let body = match (&r.json, &r.body) {
        (Some(json), _) => {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            Bytes::from(serde_json::to_vec(json).map_err(|e| e.to_string())?)
        }
        (None, Some(body)) => Bytes::from(body.clone()),
        (None, None) => Bytes::new(),
    };
    for (name, value) in &r.headers {
        let name =
            HeaderName::try_from(name.as_str()).map_err(|e| format!("header `{name}`: {e}"))?;
        let value = HeaderValue::from_str(value).map_err(|e| format!("header `{name}`: {e}"))?;
        headers.insert(name, value);
    }

    Ok(StepPlan {
        id,
        name: step.name.clone(),
        flow: flow.to_string(),
        tags: step.tags.clone(),
        source: step.source.clone(),
        method: Method::from_bytes(r.method.as_bytes()).map_err(|e| e.to_string())?,
        origin: Origin { https, host, port },
        target,
        headers,
        body,
        timeout: r.timeout.map_or(DEFAULT_TIMEOUT, |t| t.0),
    })
}

/// `/products` se agrega al path de `base_url` (`https://h/api` → `https://h/api/products`).
/// Una URL absoluta se usa tal cual.
fn resolve_url(base: &Url, url: &str) -> Result<Url, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Url::parse(url).map_err(|e| format!("url inválida: {e}"));
    }
    let relative =
        Url::parse(&format!("http://relative{url}")).map_err(|e| format!("url inválida: {e}"))?;
    let mut out = base.clone();
    out.set_path(&format!(
        "{}{}",
        base.path().trim_end_matches('/'),
        relative.path()
    ));
    out.set_query(relative.query());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_urls_against_base_path() {
        let base = Url::parse("https://api.example.com/v1/").unwrap();
        assert_eq!(
            resolve_url(&base, "/orders?x=1").unwrap().as_str(),
            "https://api.example.com/v1/orders?x=1"
        );
        let base = Url::parse("http://localhost:3000").unwrap();
        assert_eq!(
            resolve_url(&base, "/orders").unwrap().as_str(),
            "http://localhost:3000/orders"
        );
    }

    #[test]
    fn detects_loopback_hosts() {
        assert!(is_loopback("localhost"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("[::1]"));
        assert!(!is_loopback("api.example.com"));
        assert!(!is_loopback("10.0.0.1"));
    }
}
