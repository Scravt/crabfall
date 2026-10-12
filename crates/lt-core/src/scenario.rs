//! Modelo del escenario (ADR-002). Por ahora es el subconjunto de la rebanada 1:
//! flows con peso, pasos con request y el executor `constant_rate`.
//!
//! Los doc comments de los campos van al JSON Schema (`lt schema`): están escritos
//! para quien escribe el escenario, incluido Claude.

use std::{collections::BTreeMap, time::Duration};

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::units::{HumanDuration, Rate};

pub const FORMAT_VERSION: u32 = 1;

/// Escenario de prueba de carga de lt. Va en `.loadtest/scenarios/<name>.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Versión del formato. Siempre `1`.
    pub version: u32,
    /// Nombre del escenario: letras, números, `_`, `.` y `-`. Conviene que coincida con el nombre del archivo.
    pub name: String,
    /// Qué simula el escenario, en una o dos oraciones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Datos libres que no afectan el tráfico, por ejemplo `created_by: agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// Variables del escenario, usables como `{{nombre}}`. Las del entorno, `--var` y `extract` las pisan (en ese orden).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub vars: IndexMap<String, Value>,
    /// Headers y timeout que heredan todos los pasos de `flows` (no los de `setup`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<Defaults>,
    /// Pasos que corren una vez, en orden, antes de la carga (por ejemplo, login). Sus `extract` quedan como variables para todos los flows. Si alguno falla, la corrida se aborta.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<Step>,
    /// Flujos de usuario. Cada iteración elige uno según su `weight` y ejecuta sus pasos en orden.
    pub flows: IndexMap<String, Flow>,
    /// Perfil de carga. La tasa se mide en iteraciones por segundo, no en requests.
    pub load: Load,
    /// Criterios de aprobación de la corrida. Si alguno falla, `lt run` sale con código 1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<Threshold>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Un header definido en el paso pisa al de `defaults`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<HumanDuration>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    /// Peso relativo frente a los otros flows (no tienen que sumar 100). Por defecto 1.
    #[serde(default = "default_weight")]
    #[schemars(range(min = 1))]
    pub weight: u32,
    /// Pasos que se ejecutan en orden en cada iteración.
    #[schemars(length(min = 1))]
    pub steps: Vec<Step>,
}

fn default_weight() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Nombre estable y único en todo el escenario (`[A-Za-z0-9_.-]+`). Las métricas se agrupan por este nombre, no por la URL.
    #[schemars(regex(pattern = r"^[A-Za-z0-9_.-]+$"))]
    pub name: String,
    pub request: RequestSpec,
    /// Etiquetas libres. Usá `write` en los pasos que modifican datos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Dónde está el código que atiende este paso. Completalo siempre que lo conozcas: el diagnóstico lo usa para ir al código.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Valores de la respuesta que pasan a ser variables (`{{nombre}}`) para los pasos siguientes.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub extract: IndexMap<String, Extract>,
    /// Verificaciones de la respuesta. No cortan la iteración: se cuentan en `checks.pass_rate`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
}

/// Una verificación. Elegí un sujeto (`status`, `status_in`, `json`, `header`, `body_contains`, `body_matches`); `json` y `header` llevan además un predicado (`exists`, `equals`, `contains`, `matches`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Código de estado esperado. Si un paso tiene checks de status, reemplazan la expectativa de 2xx en `lt check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Alguno de estos códigos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_in: Option<Vec<u16>>,
    /// JSONPath sobre el body, como en `extract`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<String>,
    /// Nombre de un header de la respuesta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// El body contiene este texto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_contains: Option<String>,
    /// El body coincide con esta regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_matches: Option<String>,
    /// Con `json` o `header`: existe (`true`) o no existe (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// Con `json` o `header`: igual a este valor (admite `{{variables}}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    /// Con `header`: contiene este texto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Con `json` o `header`: coincide con esta regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

/// Criterio sobre una métrica, por ejemplo `{ metric: http.latency.p95, lt: 300ms }`. Usá exactamente uno de `lt`, `lte`, `gt`, `gte`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Threshold {
    /// `http.latency.p50` … `http.latency.p99.9`, `http.latency.avg`, `http.latency.max` (duración, ej. `300ms`); `http.error_rate` y `checks.pass_rate` (porcentaje, ej. `1%`); `http.rps` (número).
    pub metric: String,
    /// Solo este paso.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Solo los pasos con este tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<Value>,
}

/// De dónde sale un valor extraído. Usá exactamente uno de los tres.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Extract {
    /// JSONPath simple sobre el body: `$.token`, `$.items[0].id`, `$['a-b']`. Conserva el tipo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<String>,
    /// Nombre de un header de la respuesta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Regex sobre el body: se toma el primer grupo de captura, o todo el match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    /// GET, POST, PUT, PATCH, DELETE, HEAD u OPTIONS.
    pub method: String,
    /// Relativa a la `base_url` del entorno (empieza con `/`) o absoluta (`https://...`). El host tiene que estar en `allowed_hosts`. Admite `{{variables}}` en el path y la query, nunca en el host.
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Parámetros de query; se agregan a la URL codificados.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, Value>,
    /// Timeout del request. Por defecto 30s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<HumanDuration>,
    /// Body JSON; agrega `content-type: application/json`. Excluye a `body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    /// Body de texto tal cual. Excluye a `json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// Dónde está el código que atiende este paso. El motor no lo usa: lo copia en los resultados.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Ruta del archivo, relativa a la raíz del repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Función, método o handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// `operationId` de OpenAPI, si existe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

/// Bloque `load`. Es un struct plano (no un enum etiquetado) para que los errores
/// conserven ruta y línea; qué campos aplican a cada executor lo valida `validate`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Load {
    /// `constant_rate`: inicia `rate` iteraciones por segundo durante `duration`, sin esperar respuestas. Requiere `rate`, `duration` y `max_vus`.
    pub executor: Executor,
    /// Iteraciones por segundo, por ejemplo `50/s` o `3000/m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<Rate>,
    /// Cuánto dura la carga, por ejemplo `30s` o `5m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<HumanDuration>,
    /// Máximo de iteraciones en vuelo. Si no hay lugar, la iteración se descarta y se reporta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_vus: Option<u32>,
    /// Cuánto se espera a las iteraciones en curso al terminar. Por defecto 30s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graceful_stop: Option<HumanDuration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
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

/// JSON Schema del escenario, generado desde los tipos (`lt schema`).
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Scenario)).expect("el schema siempre serializa")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_strict_and_documented() {
        let schema = json_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert!(
            schema["properties"]["flows"]["description"]
                .as_str()
                .unwrap()
                .contains("weight")
        );
        let step = &schema["$defs"]["Step"];
        assert_eq!(step["additionalProperties"], false);
        assert_eq!(schema["$defs"]["Duration"]["type"], "string");
    }
}
