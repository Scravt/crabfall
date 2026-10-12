//! Modelo del escenario (ADR-002). Por ahora es el subconjunto de la rebanada 1:
//! flows con peso, pasos con request y el executor `constant_rate`.

use std::{collections::BTreeMap, time::Duration};

use indexmap::IndexMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::units::{HumanDuration, Rate};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// En el orden del archivo.
    pub flows: IndexMap<String, Flow>,
    pub load: Load,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    #[serde(default = "default_weight")]
    pub weight: u32,
    pub steps: Vec<Step>,
}

fn default_weight() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    pub request: RequestSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<HumanDuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// Dónde está el código que atiende este paso. El motor no lo usa: lo copia en los resultados.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

/// Bloque `load`. Es un struct plano (no un enum etiquetado) para que los errores
/// conserven ruta y línea; qué campos aplican a cada executor lo valida `validate`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Load {
    pub executor: Executor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<Rate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<HumanDuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_vus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graceful_stop: Option<HumanDuration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    ConstantRate,
}

/// `load` ya validado, listo para el motor.
#[derive(Debug, Clone, Copy)]
pub enum LoadPlan {
    ConstantRate {
        rate: Rate,
        duration: Duration,
        max_vus: u32,
        graceful_stop: Duration,
    },
}

pub const DEFAULT_GRACEFUL_STOP: Duration = Duration::from_secs(30);

impl Load {
    /// Requiere que el escenario haya pasado `validate`.
    pub fn plan(&self) -> LoadPlan {
        match self.executor {
            Executor::ConstantRate => LoadPlan::ConstantRate {
                rate: self.rate.expect("validado"),
                duration: self.duration.expect("validado").0,
                max_vus: self.max_vus.expect("validado"),
                graceful_stop: self.graceful_stop.map_or(DEFAULT_GRACEFUL_STOP, |g| g.0),
            },
        }
    }
}

impl Scenario {
    /// Todos los pasos con el nombre de su flow, en orden.
    pub fn steps(&self) -> impl Iterator<Item = (&str, &Step)> {
        self.flows
            .iter()
            .flat_map(|(flow, f)| f.steps.iter().map(move |s| (flow.as_str(), s)))
    }

    /// Cota de requests por segundo: tasa de iteraciones × máximo de pasos por flow.
    pub fn max_request_rate(&self) -> f64 {
        let max_steps = self
            .flows
            .values()
            .map(|f| f.steps.len())
            .max()
            .unwrap_or(0);
        match self.load.plan() {
            LoadPlan::ConstantRate { rate, .. } => rate.per_second() * max_steps as f64,
        }
    }
}
