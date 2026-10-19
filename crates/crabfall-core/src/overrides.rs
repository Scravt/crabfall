//! Ajustes de una corrida que no están en el YAML: carga, pausas y datasets (ADR-008).
//!
//! Son lo que tendría un panel de personalización. Entran por flags (`--rate 100/s`) y se
//! aplican al escenario antes de armar el plan, así que la aprobación los firma como
//! cualquier cambio del archivo: `approve` y `run` tienen que recibir los mismos.

use std::str::FromStr;

use crate::{
    scenario::{Executor, Pick, Scenario, Stage, Threshold, VuRefresh},
    threshold::to_flag,
    units::{HumanDuration, Rate, Think},
    validate::{Issue, check_scenario, load_fields},
};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Overrides {
    pub executor: Option<Executor>,
    pub rate: Option<Rate>,
    pub duration: Option<HumanDuration>,
    pub vus: Option<u32>,
    pub max_vus: Option<u32>,
    pub start_rate: Option<Rate>,
    pub stages: Option<Vec<Stage>>,
    pub graceful_stop: Option<HumanDuration>,
    /// Pausa para todos los pasos de los flows; reemplaza la de cada paso y la de `defaults`.
    pub think: Option<Think>,
    /// Cada cuánto renueva la sesión cada VU. Sin `vu_refresh` en el escenario, crea uno que repite `vu_setup`.
    pub refresh_every: Option<HumanDuration>,
    /// Otro archivo para un dataset: (nombre, archivo).
    pub data: Vec<(String, String)>,
    /// Otro `pick` para un dataset.
    pub pick: Vec<(String, Pick)>,
    /// Thresholds que reemplazan a los del escenario. No cambian el tráfico: no afectan la firma.
    pub thresholds: Vec<Threshold>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Aplica los ajustes y vuelve a validar. Los errores dicen qué flag corrige cada campo.
    pub fn apply(&self, s: &mut Scenario) -> Vec<Issue> {
        let mut issues = Vec::new();
        let load = &mut s.load;
        if let Some(executor) = self.executor {
            load.executor = executor;
            // Lo que el YAML tenía para otro executor ya no aplica.
            let (required, allowed) = load_fields(executor);
            let keeps = |f: &str| required.contains(&f) || allowed.contains(&f);
            if !keeps("rate") {
                load.rate = None;
            }
            if !keeps("duration") {
                load.duration = None;
            }
            if !keeps("max_vus") {
                load.max_vus = None;
            }
            if !keeps("start_rate") {
                load.start_rate = None;
            }
            if !keeps("stages") {
                load.stages = None;
            }
            if !keeps("vus") {
                load.vus = None;
            }
        }
        set(&mut load.rate, self.rate);
        set(&mut load.duration, self.duration);
        set(&mut load.vus, self.vus);
        set(&mut load.max_vus, self.max_vus);
        set(&mut load.start_rate, self.start_rate);
        set(&mut load.stages, self.stages.clone());
        set(&mut load.graceful_stop, self.graceful_stop);

        if let Some(think) = self.think {
            s.defaults.get_or_insert_default().think = Some(think);
            for step in s.flows.values_mut().flat_map(|f| &mut f.steps) {
                step.think = None;
            }
        }

        if let Some(every) = self.refresh_every {
            match &mut s.vu_refresh {
                Some(r) => r.every = every,
                None => {
                    s.vu_refresh = Some(VuRefresh {
                        every,
                        steps: vec![],
                    })
                }
            }
        }

        let available = s.data.keys().cloned().collect::<Vec<_>>().join(", ");
        for (name, file) in &self.data {
            match s.data.get_mut(name) {
                Some(spec) => spec.file = file.clone(),
                None => issues.push(no_dataset("--data", name, &available)),
            }
        }
        for (name, pick) in &self.pick {
            match s.data.get_mut(name) {
                Some(spec) => spec.pick = *pick,
                None => issues.push(no_dataset("--pick", name, &available)),
            }
        }
        if !self.thresholds.is_empty() {
            s.thresholds = self.thresholds.clone();
        }

        let given = self.describe();
        issues.extend(check_scenario(s).into_iter().map(|mut issue| {
            if let Some(flag) = flag_for(&issue.path) {
                let from_flag = given.iter().any(|g| g.split(' ').next() == Some(flag));
                if issue.message.starts_with("falta") {
                    issue.message = format!("{} (usá {flag})", issue.message);
                } else if from_flag {
                    issue.message = format!("{} (sacá {flag})", issue.message);
                }
            }
            issue
        }));
        issues
    }

    /// Los ajustes como flags, en orden fijo: para el resumen de aprobación y `results.json`.
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |flag: &str, value: Option<String>| {
            if let Some(v) = value {
                out.push(format!("--{flag} {v}"));
            }
        };
        push(
            "executor",
            self.executor.map(|e| executor_name(e).to_string()),
        );
        push("rate", self.rate.map(|r| r.to_string()));
        push("start-rate", self.start_rate.map(|r| r.to_string()));
        push("stages", self.stages.as_deref().map(stages_to_string));
        push("duration", self.duration.map(|d| d.to_string()));
        push("vus", self.vus.map(|v| v.to_string()));
        push("max-vus", self.max_vus.map(|v| v.to_string()));
        push("graceful-stop", self.graceful_stop.map(|d| d.to_string()));
        push("think", self.think.map(|t| t.to_string()));
        push("refresh-every", self.refresh_every.map(|d| d.to_string()));
        for (name, file) in &self.data {
            push("data", Some(format!("{name}={file}")));
        }
        for (name, pick) in &self.pick {
            push("pick", Some(format!("{name}={}", pick.as_str())));
        }
        for t in &self.thresholds {
            push("threshold", Some(to_flag(t)));
        }
        out
    }
}

fn set<T>(field: &mut Option<T>, value: Option<T>) {
    if value.is_some() {
        *field = value;
    }
}

fn no_dataset(flag: &str, name: &str, available: &str) -> Issue {
    let available = if available.is_empty() {
        "el escenario no tiene `data`".to_string()
    } else {
        format!("hay: {available}")
    };
    Issue {
        path: String::new(),
        line: None,
        column: None,
        message: format!("{flag} {name}=…: no hay un dataset `{name}` ({available})"),
    }
}

/// Qué flag corrige un campo.
fn flag_for(path: &str) -> Option<&'static str> {
    Some(match path {
        "load.rate" => "--rate",
        "load.duration" => "--duration",
        "load.max_vus" => "--max-vus",
        "load.start_rate" => "--start-rate",
        "load.stages" => "--stages",
        "load.vus" => "--vus",
        "load.graceful_stop" => "--graceful-stop",
        "load.executor" => "--executor",
        "vu_refresh" | "vu_refresh.every" => "--refresh-every",
        _ => return None,
    })
}

pub fn executor_name(e: Executor) -> &'static str {
    match e {
        Executor::ConstantRate => "constant_rate",
        Executor::RampingRate => "ramping_rate",
        Executor::ConstantVus => "constant_vus",
    }
}

impl FromStr for Executor {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "constant_rate" => Ok(Self::ConstantRate),
            "ramping_rate" => Ok(Self::RampingRate),
            "constant_vus" => Ok(Self::ConstantVus),
            _ => Err(format!(
                "executor `{s}` desconocido (usá constant_rate, ramping_rate o constant_vus)"
            )),
        }
    }
}

impl FromStr for Pick {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "sequential" => Ok(Self::Sequential),
            "random" => Ok(Self::Random),
            _ => Err(format!("pick `{s}` desconocido (usá sequential o random)")),
        }
    }
}

/// Etapas de `--stages`. Es un alias para que clap lo tome como un solo valor, no como una lista.
pub type Stages = Vec<Stage>;

/// `100/s:1m,0/s:30s` → etapas de `ramping_rate`.
pub fn parse_stages(s: &str) -> Result<Vec<Stage>, String> {
    let invalid = || {
        format!("etapas inválidas `{s}`: usá tasa:duración separadas por comas (100/s:1m,0/s:30s)")
    };
    s.split(',')
        .map(|stage| {
            let (target, duration) = stage.trim().split_once(':').ok_or_else(invalid)?;
            Ok(Stage {
                target: target.parse()?,
                duration: duration.parse()?,
            })
        })
        .collect()
}

fn stages_to_string(stages: &[Stage]) -> String {
    stages
        .iter()
        .map(|s| format!("{}:{}", s.target, s.duration))
        .collect::<Vec<_>>()
        .join(",")
}

/// `nombre=valor`, para `--data` y `--pick`.
pub fn parse_pair<T: FromStr<Err: ToString>>(s: &str) -> Result<(String, T), String> {
    let (name, value) = s
        .split_once('=')
        .ok_or_else(|| format!("`{s}`: usá nombre=valor"))?;
    let value = value.trim().parse().map_err(|e: T::Err| e.to_string())?;
    Ok((name.trim().to_string(), value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::parse_scenario;

    const YAML: &str = "version: 1
name: s
data:
  users: { file: data/users.csv }
defaults: { think: 5s }
flows:
  main:
    steps:
      - name: a
        request: { method: GET, url: /a }
        think: 1s
      - name: b
        request: { method: GET, url: /b }
load:
  executor: constant_rate
  rate: 10/s
  duration: 30s
  max_vus: 20
";

    fn scenario() -> Scenario {
        parse_scenario(YAML).unwrap()
    }

    #[test]
    fn changes_load_think_and_data() {
        let mut s = scenario();
        let o = Overrides {
            rate: Some("100/s".parse().unwrap()),
            duration: Some("2m".parse().unwrap()),
            think: Some("1s-3s".parse().unwrap()),
            data: vec![("users".into(), "data/otros.csv".into())],
            pick: vec![("users".into(), Pick::Random)],
            thresholds: vec!["http.latency.p95<300ms".parse().unwrap()],
            ..Default::default()
        };
        assert!(o.apply(&mut s).is_empty());
        assert_eq!(s.load.rate.unwrap().to_string(), "100/s");
        assert_eq!(s.load.duration.unwrap().to_string(), "2m");
        assert_eq!(s.load.max_vus, Some(20));
        // --think pisa la pausa de cada paso y la de defaults.
        assert_eq!(s.defaults.unwrap().think.unwrap().to_string(), "1s-3s");
        assert!(s.flows["main"].steps.iter().all(|st| st.think.is_none()));
        assert_eq!(s.data["users"].file, "data/otros.csv");
        assert_eq!(s.data["users"].pick, Pick::Random);
        assert_eq!(s.thresholds.len(), 1);
        assert_eq!(
            o.describe(),
            [
                "--rate 100/s",
                "--duration 2m",
                "--think 1s-3s",
                "--data users=data/otros.csv",
                "--pick users=random",
                "--threshold http.latency.p95<300ms"
            ]
        );
    }

    #[test]
    fn refresh_every_creates_or_changes_vu_refresh() {
        let with_setup = YAML.replacen(
            "flows:",
            "vu_setup:\n  - name: login\n    request: { method: POST, url: /login }\nflows:",
            1,
        );
        let o = Overrides {
            refresh_every: Some("4m".parse().unwrap()),
            ..Default::default()
        };
        let mut s = parse_scenario(&with_setup).unwrap();
        assert!(o.apply(&mut s).is_empty());
        let r = s.vu_refresh.unwrap();
        assert_eq!((r.every.to_string(), r.steps.len()), ("4m".into(), 0));
        assert_eq!(o.describe(), ["--refresh-every 4m"]);

        // Sin vu_setup no hay sesión que renovar: el error dice qué flag sacar.
        let mut s = scenario();
        let issues = o.apply(&mut s);
        assert!(
            issues[0].message.ends_with("(sacá --refresh-every)"),
            "{issues:?}"
        );
    }

    #[test]
    fn switching_executor_drops_fields_and_names_the_missing_flag() {
        let mut s = scenario();
        let o = Overrides {
            executor: Some(Executor::ConstantVus),
            ..Default::default()
        };
        let issues = o.apply(&mut s);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].path, "load.vus");
        assert!(issues[0].message.ends_with("(usá --vus)"), "{issues:?}");
        // `rate` y `max_vus` no aplican a constant_vus; `duration` sí.
        assert_eq!((s.load.rate, s.load.max_vus), (None, None));
        assert!(s.load.duration.is_some());

        let mut s = scenario();
        let ramp = Overrides {
            executor: Some(Executor::RampingRate),
            stages: Some(parse_stages("50/s:1m, 0/s:30s").unwrap()),
            ..Default::default()
        };
        assert!(ramp.apply(&mut s).is_empty());
        assert_eq!(s.load.duration, None);
        assert_eq!(
            ramp.describe(),
            ["--executor ramping_rate", "--stages 50/s:1m,0/s:30s"]
        );

        // Un flag que no aplica al executor se pide sacar, no agregar.
        let mut s = scenario();
        let extra = Overrides {
            stages: Some(parse_stages("50/s:1m").unwrap()),
            ..Default::default()
        };
        let issues = extra.apply(&mut s);
        assert!(issues[0].message.ends_with("(sacá --stages)"), "{issues:?}");
    }

    #[test]
    fn rejects_unknown_datasets_and_bad_values() {
        let mut s = scenario();
        let o = Overrides {
            pick: vec![("productos".into(), Pick::Random)],
            ..Default::default()
        };
        let issues = o.apply(&mut s);
        assert!(
            issues[0]
                .message
                .contains("no hay un dataset `productos` (hay: users)")
        );

        assert!(parse_stages("50/s").is_err());
        assert!(parse_pair::<Pick>("users").is_err());
        assert!(
            parse_pair::<Pick>("users=alguno")
                .unwrap_err()
                .contains("sequential")
        );
        assert!("ramp".parse::<Executor>().is_err());
        assert!(Overrides::default().is_empty());
    }
}
