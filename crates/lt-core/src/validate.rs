//! Parseo y validación de escenarios. Los errores señalan la ruta del campo y,
//! cuando se conoce, la línea y la columna (ADR-002 §7, §8 y §10).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    assertion::Assertion,
    extract::Extractor,
    scenario::{Executor, FORMAT_VERSION, Load, Scenario, Step},
    threshold::ThresholdPlan,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
    pub message: String,
}

impl Issue {
    fn at(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line: None,
            column: None,
            message: message.into(),
        }
    }
}

/// Resultado de validar: errores y valores pendientes (`todo`).
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub errors: Vec<Issue>,
    pub todos: Vec<Issue>,
}

impl Report {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty() && self.todos.is_empty()
    }
}

/// Parsea un documento YAML a `T` con ruta, línea y columna en el error.
pub fn parse_yaml<'de, T: Deserialize<'de>>(text: &'de str) -> Result<T, Issue> {
    let mut path = String::new();
    serde_saphyr::with_deserializer_from_str(text, |de| {
        serde_path_to_error::deserialize(de).map_err(|e| {
            path = e.path().to_string();
            e.into_inner()
        })
    })
    .map_err(|e| {
        let location = e.location();
        Issue {
            path: if path == "." || path == "?" {
                String::new()
            } else {
                path
            },
            line: location.as_ref().map(|l| l.line()),
            column: location.as_ref().map(|l| l.column()),
            message: plain_message(&e.without_snippet().to_string()),
        }
    })
}

/// Quita la ubicación que serde-saphyr agrega al mensaje (`error: line N column M:`
/// al principio o `at line N, column M` al final); va en campos aparte.
fn plain_message(rendered: &str) -> String {
    let s = rendered.trim().trim_start_matches("error:").trim();
    let s = match s.strip_prefix("line ") {
        Some(rest) => rest.split_once(": ").map_or(s, |(_, msg)| msg),
        None => s,
    };
    let s = s.lines().next().unwrap_or(s);
    s.rsplit_once(" at line ")
        .map_or(s, |(msg, _)| msg)
        .to_string()
}

/// Parsea y valida un escenario. Devuelve el modelo solo si no hay errores ni `todo`.
pub fn parse_scenario(text: &str) -> Result<Scenario, Report> {
    let mut report = Report::default();

    // 1. YAML genérico: errores de sintaxis y valores `todo`.
    let generic: Value = parse_yaml(text).map_err(|issue| Report {
        errors: vec![issue],
        todos: vec![],
    })?;
    find_todos(&generic, &mut String::new(), &mut report.todos);
    if !report.todos.is_empty() {
        return Err(report);
    }

    // 2. Modelo tipado: campos desconocidos, tipos y unidades.
    let scenario: Scenario = parse_yaml(text).map_err(|issue| Report {
        errors: vec![issue],
        todos: vec![],
    })?;

    // 3. Reglas que el tipo no expresa.
    check_rules(&scenario, &mut report.errors);
    if report.errors.is_empty() {
        Ok(scenario)
    } else {
        Err(report)
    }
}

fn find_todos(value: &Value, path: &mut String, todos: &mut Vec<Issue>) {
    match value {
        Value::Object(map) => {
            if let (1, Some(Value::String(what))) = (map.len(), map.get("todo")) {
                todos.push(Issue::at(path.clone(), format!("valor pendiente: {what}")));
                return;
            }
            for (key, child) in map {
                let len = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
                find_todos(child, path, todos);
                path.truncate(len);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                find_todos(child, path, todos);
                path.truncate(len);
            }
        }
        _ => {}
    }
}

const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

fn check_rules(s: &Scenario, errors: &mut Vec<Issue>) {
    if s.version != FORMAT_VERSION {
        errors.push(Issue::at(
            "version",
            format!(
                "versión de formato {} no soportada; esta versión de lt soporta {FORMAT_VERSION}",
                s.version
            ),
        ));
    }
    if !valid_name(&s.name) {
        errors.push(Issue::at(
            "name",
            "usá solo letras, números, `_`, `.` y `-`",
        ));
    }
    if s.flows.is_empty() {
        errors.push(Issue::at("flows", "tiene que haber al menos un flow"));
    }

    let mut seen = HashSet::new();
    for (i, step) in s.setup.iter().enumerate() {
        check_step(step, &format!("setup[{i}]"), &mut seen, errors);
        if step.think.is_some() {
            errors.push(Issue::at(
                format!("setup[{i}].think"),
                "`setup` corre una vez antes de la carga: no lleva pausas",
            ));
        }
    }
    for (flow_name, flow) in &s.flows {
        let flow_path = format!("flows.{flow_name}");
        if flow.weight == 0 {
            errors.push(Issue::at(
                format!("{flow_path}.weight"),
                "tiene que ser mayor que 0",
            ));
        }
        if flow.steps.is_empty() {
            errors.push(Issue::at(
                format!("{flow_path}.steps"),
                "el flow no tiene pasos",
            ));
        }
        for (i, step) in flow.steps.iter().enumerate() {
            check_step(step, &format!("{flow_path}.steps[{i}]"), &mut seen, errors);
        }
    }

    for (name, spec) in &s.data {
        let path = format!("data.{name}");
        if !valid_var_name(name) || name == "env" {
            errors.push(Issue::at(
                &path,
                "el nombre usa letras, números y `_`, no empieza con un número y no puede ser `env`",
            ));
        }
        let file = spec.file.to_ascii_lowercase();
        if !(file.ends_with(".csv") || file.ends_with(".json")) {
            errors.push(Issue::at(
                format!("{path}.file"),
                "usá un archivo .csv o .json",
            ));
        }
    }

    check_load(&s.load, errors);
    for (i, t) in s.thresholds.iter().enumerate() {
        if let Err(e) = ThresholdPlan::parse(t) {
            errors.push(Issue::at(format!("thresholds[{i}]"), e));
        }
    }
}

fn check_step<'a>(
    step: &'a Step,
    path: &str,
    seen: &mut HashSet<&'a str>,
    errors: &mut Vec<Issue>,
) {
    if !valid_name(&step.name) {
        errors.push(Issue::at(
            format!("{path}.name"),
            "usá solo letras, números, `_`, `.` y `-`",
        ));
    } else if !seen.insert(step.name.as_str()) {
        errors.push(Issue::at(
            format!("{path}.name"),
            format!(
                "`{}` está repetido: el nombre de cada paso tiene que ser único",
                step.name
            ),
        ));
    }
    let r = &step.request;
    if !METHODS.contains(&r.method.as_str()) {
        errors.push(Issue::at(
            format!("{path}.request.method"),
            format!(
                "método `{}` no soportado (usá uno de {})",
                r.method,
                METHODS.join(", ")
            ),
        ));
    }
    let url_ok =
        r.url.starts_with('/') || r.url.starts_with("http://") || r.url.starts_with("https://");
    if !url_ok {
        errors.push(Issue::at(
            format!("{path}.request.url"),
            "tiene que empezar con `/` (relativa a base_url) o con http:// o https://",
        ));
    }
    if r.json.is_some() && r.body.is_some() {
        errors.push(Issue::at(
            format!("{path}.request"),
            "usá `json` o `body`, no los dos",
        ));
    }
    if r.timeout.is_some_and(|t| t.0.is_zero()) {
        errors.push(Issue::at(
            format!("{path}.request.timeout"),
            "tiene que ser mayor que 0",
        ));
    }
    for (name, spec) in &step.extract {
        if !valid_var_name(name) {
            errors.push(Issue::at(
                format!("{path}.extract.{name}"),
                "el nombre de la variable usa letras, números y `_`, y no empieza con un número",
            ));
        }
        if let Err(e) = Extractor::parse(spec) {
            errors.push(Issue::at(format!("{path}.extract.{name}"), e));
        }
    }
    for (i, check) in step.checks.iter().enumerate() {
        if let Err(e) = Assertion::parse(check) {
            errors.push(Issue::at(format!("{path}.checks[{i}]"), e));
        }
    }
}

fn valid_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Reglas que el tipo no expresa, sobre un escenario ya parseado (por ejemplo, después
/// de aplicarle ajustes de la CLI).
pub fn check_scenario(s: &Scenario) -> Vec<Issue> {
    let mut errors = Vec::new();
    check_rules(s, &mut errors);
    errors
}

/// Campos de `load` (obligatorios, opcionales) de cada executor.
pub fn load_fields(executor: Executor) -> (&'static [&'static str], &'static [&'static str]) {
    match executor {
        Executor::ConstantRate => (&["rate", "duration", "max_vus"], &["graceful_stop"]),
        Executor::RampingRate => (&["stages", "max_vus"], &["start_rate", "graceful_stop"]),
        Executor::ConstantVus => (&["vus", "duration"], &["graceful_stop"]),
    }
}

fn check_load(l: &Load, errors: &mut Vec<Issue>) {
    let (required, allowed) = load_fields(l.executor);
    let present = [
        ("rate", l.rate.is_some()),
        ("duration", l.duration.is_some()),
        ("max_vus", l.max_vus.is_some()),
        ("start_rate", l.start_rate.is_some()),
        ("stages", l.stages.is_some()),
        ("vus", l.vus.is_some()),
        ("graceful_stop", l.graceful_stop.is_some()),
    ];
    let executor = serde_json::to_value(l.executor).unwrap();
    let executor = executor.as_str().unwrap_or_default();
    for (field, is_present) in present {
        if required.contains(&field) && !is_present {
            errors.push(Issue::at(
                format!("load.{field}"),
                format!("falta `{field}` (obligatorio en {executor})"),
            ));
        }
        if is_present && !required.contains(&field) && !allowed.contains(&field) {
            errors.push(Issue::at(
                format!("load.{field}"),
                format!(
                    "`{field}` no aplica a {executor} (usá: {})",
                    [required, allowed].concat().join(", ")
                ),
            ));
        }
    }
    if l.rate.is_some_and(|r| r.count == 0) {
        errors.push(Issue::at("load.rate", "tiene que ser mayor que 0"));
    }
    if l.duration.is_some_and(|d| d.0.is_zero()) {
        errors.push(Issue::at("load.duration", "tiene que ser mayor que 0"));
    }
    for (field, value) in [("max_vus", l.max_vus), ("vus", l.vus)] {
        if value == Some(0) {
            errors.push(Issue::at(
                format!("load.{field}"),
                "tiene que ser mayor que 0",
            ));
        }
    }
    if let Some(stages) = &l.stages {
        if stages.is_empty() {
            errors.push(Issue::at(
                "load.stages",
                "tiene que haber al menos una etapa",
            ));
        }
        for (i, s) in stages.iter().enumerate() {
            if s.duration.0.is_zero() {
                errors.push(Issue::at(
                    format!("load.stages[{i}].duration"),
                    "tiene que ser mayor que 0",
                ));
            }
        }
        let start = l.start_rate.map_or(0, |r| r.count);
        if start == 0 && stages.iter().all(|s| s.target.count == 0) {
            errors.push(Issue::at(
                "load.stages",
                "todas las tasas son 0: no se generaría carga",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = "version: 1
name: demo
flows:
  main:
    steps:
      - name: list
        request:
          method: GET
          url: /products
load:
  executor: constant_rate
  rate: 50/s
  duration: 30s
  max_vus: 10
";

    fn errors(yaml: &str) -> Vec<Issue> {
        parse_scenario(yaml).unwrap_err().errors
    }

    #[test]
    fn accepts_minimal_scenario() {
        let s = parse_scenario(OK).unwrap();
        assert_eq!(s.flows["main"].weight, 1);
        assert_eq!(s.max_request_rate(), Some(50.0));
    }

    #[test]
    fn unknown_field_has_path_and_location() {
        let e = &errors(&OK.replace("method: GET", "metod: GET"))[0];
        assert_eq!(e.path, "flows.main.steps[0].request.metod");
        assert_eq!((e.line, e.column), (Some(8), Some(11)));
        assert!(
            e.message.starts_with("unknown field `metod`"),
            "{}",
            e.message
        );
        assert!(!e.message.contains("line"), "{}", e.message);
    }

    #[test]
    fn bad_unit_has_path() {
        let e = &errors(&OK.replace("rate: 50/s", "rate: 50"))[0];
        assert_eq!(e.path, "load.rate");
        assert!(e.message.contains("tasa inválida"), "{}", e.message);
    }

    #[test]
    fn reports_todos_with_path() {
        let yaml = OK.replace("url: /products", "url: { todo: \"ruta del listado\" }");
        let report = parse_scenario(&yaml).unwrap_err();
        assert_eq!(report.todos.len(), 1);
        assert_eq!(report.todos[0].path, "flows.main.steps[0].request.url");
        assert!(report.errors.is_empty());
    }

    #[test]
    fn rejects_duplicate_step_names_and_bad_values() {
        let yaml = OK.replace(
            "          url: /products\n",
            "          url: /products\n      - name: list\n        request:\n          method: FETCH\n          url: products\n",
        );
        let paths: Vec<_> = errors(&yaml).into_iter().map(|e| e.path).collect();
        assert_eq!(
            paths,
            [
                "flows.main.steps[1].name",
                "flows.main.steps[1].request.method",
                "flows.main.steps[1].request.url"
            ]
        );
    }

    #[test]
    fn load_errors_have_path() {
        let e = &errors(&OK.replace("  max_vus: 10\n", ""))[0];
        assert_eq!(e.path, "load.max_vus");
        let e = &errors(&OK.replace("constant_rate", "constant_arrival"))[0];
        assert_eq!((e.path.as_str(), e.line), ("load.executor", Some(11)));
        // Campos que no aplican al executor elegido.
        let paths: Vec<_> = errors(&OK.replace("constant_rate", "constant_vus"))
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths, ["load.rate", "load.max_vus", "load.vus"]);
    }

    #[test]
    fn checks_dataset_names_and_files() {
        let yaml = OK.replacen(
            "flows:",
            "data:
  env: { file: a.csv }
  users: { file: u.txt, pick: random }
flows:",
            1,
        );
        let paths: Vec<_> = errors(&yaml).into_iter().map(|e| e.path).collect();
        assert_eq!(paths, ["data.env", "data.users.file"]);
    }

    #[test]
    fn ramping_rate_needs_stages() {
        let ramp = OK.replace(
            "  rate: 50/s
  duration: 30s
",
            "  start_rate: 0/s
  stages:
    - { target: 100/s, duration: 1m }
    - { target: 0/s, duration: 10s }
",
        );
        let ramp = ramp.replace("constant_rate", "ramping_rate");
        let s = parse_scenario(&ramp).unwrap();
        assert_eq!(s.max_request_rate(), Some(100.0));
        let e = &errors(&ramp.replace("100/s", "0/s"))[0];
        assert!(e.message.contains("todas las tasas son 0"), "{}", e.message);
    }

    #[test]
    fn rejects_unknown_version() {
        assert_eq!(
            errors(&OK.replace("version: 1", "version: 2"))[0].path,
            "version"
        );
    }
}
