//! Configuración del proyecto: `.crabfall/config.yaml` (ADR-002).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::units::HumanDuration;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub default_environment: Option<String>,
    pub environments: BTreeMap<String, Environment>,
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Language of what Crabfall prints for this project (`en` or `es`). `--lang` and
    /// `CRABFALL_LANG` override it; without any of them, English (ADR-016).
    #[serde(default)]
    pub language: Option<crate::i18n::Lang>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub base_url: String,
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, Value>,
    #[serde(default)]
    pub limits: Limits,
}

/// Límites duros. Ni una aprobación los supera.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Requests por segundo (no iteraciones).
    pub max_rps: Option<f64>,
    pub max_vus: Option<u32>,
    pub max_duration: Option<HumanDuration>,
}

impl Config {
    /// Devuelve el entorno pedido, o el `default_environment`, o el único que haya.
    pub fn environment(&self, name: Option<&str>) -> Result<(String, &Environment), String> {
        let name = match name.or(self.default_environment.as_deref()) {
            Some(n) => n.to_string(),
            None if self.environments.len() == 1 => {
                self.environments.keys().next().unwrap().clone()
            }
            None => {
                return Err(crate::tr!(
                    "there are several environments and no default: use --env",
                    "hay varios entornos y ninguno por defecto: usá --env"
                ));
            }
        };
        let env = self.environments.get(&name).ok_or_else(|| {
            let known: Vec<_> = self.environments.keys().map(String::as_str).collect();
            crate::tr!(
                "environment `{name}` does not exist (defined: {})",
                "no existe el entorno `{name}` (definidos: {})",
                known.join(", ")
            )
        })?;
        Ok((name, env))
    }
}
