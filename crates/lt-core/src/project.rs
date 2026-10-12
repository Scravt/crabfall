//! Ubicación de `.loadtest/` en el repo del usuario.

use std::path::{Path, PathBuf};

use crate::{
    config::Config,
    validate::{Issue, parse_yaml},
};

pub const DIR: &str = ".loadtest";

pub struct Project {
    /// Carpeta `.loadtest/`.
    pub dir: PathBuf,
}

impl Project {
    /// Busca `.loadtest/` desde `start` hacia arriba.
    pub fn discover(start: &Path) -> Option<Self> {
        start
            .ancestors()
            .map(|p| p.join(DIR))
            .find(|d| d.is_dir())
            .map(|dir| Self { dir })
    }

    /// `checkout` → `.loadtest/scenarios/checkout.yaml`. Una ruta se usa tal cual.
    pub fn scenario_path(project: Option<&Self>, arg: &str) -> PathBuf {
        let looks_like_path = arg.contains(['/', '\\'])
            || [".yaml", ".yml", ".json"].iter().any(|e| arg.ends_with(e));
        match project {
            Some(p) if !looks_like_path => p.dir.join("scenarios").join(format!("{arg}.yaml")),
            _ => PathBuf::from(arg),
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.yaml")
    }

    pub fn load_config(&self) -> Result<Config, String> {
        let path = self.config_path();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("no se pudo leer {}: {e}", path.display()))?;
        parse_yaml::<Config>(&text).map_err(|i| format!("{}: {}", path.display(), describe(&i)))
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.dir.join("runs")
    }
}

/// `ruta (línea L, columna C): mensaje`, para mostrar en terminal.
pub fn describe(issue: &Issue) -> String {
    let at = match (issue.line, issue.column) {
        (Some(l), Some(c)) => format!(" (línea {l}, columna {c})"),
        _ => String::new(),
    };
    let path = if issue.path.is_empty() {
        "(raíz)"
    } else {
        &issue.path
    };
    format!("{path}{at}: {}", issue.message)
}
