//! Interpolación `{{...}}` (ADR-002 §6).
//!
//! - `{{nombre}}`: variable (de `vars`, del entorno, de `--var` o de un `extract`).
//! - `{{env.NOMBRE}}`: secreto declarado en `secrets` de `config.yaml`.
//! - `{{dataset.columna}}`: columna de la fila que tomó la iteración (ADR-007).
//! - `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt(min,max)}}`: un valor nuevo por uso.
//!
//! Si un valor JSON es *exactamente* `"{{x}}"`, conserva el tipo de `x`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Lit(String),
    Var(String),
    Secret(String),
    Uuid,
    Timestamp,
    RandomInt(i64, i64),
}

/// De dónde salen los valores al interpolar.
pub trait Scope {
    fn var(&self, name: &str) -> Option<&Value>;
    fn secret(&self, name: &str) -> Option<&str>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    source: String,
    parts: Vec<Part>,
}

impl Template {
    pub fn literal(s: impl Into<String>) -> Self {
        let s = s.into();
        Self {
            parts: vec![Part::Lit(s.clone())],
            source: s,
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        let mut parts = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find("{{") {
            if start > 0 {
                parts.push(Part::Lit(rest[..start].to_string()));
            }
            let after = &rest[start + 2..];
            let end = after.find("}}").ok_or_else(|| {
                crate::tr!(
                    "missing closing `}}}}` in `{s}`",
                    "falta cerrar `}}}}` en `{s}`"
                )
            })?;
            parts.push(parse_expr(after[..end].trim())?);
            rest = &after[end + 2..];
        }
        if !rest.is_empty() {
            parts.push(Part::Lit(rest.to_string()));
        }
        Ok(Self {
            source: s.to_string(),
            parts,
        })
    }

    /// El texto original, con los `{{...}}` sin resolver.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn is_literal(&self) -> bool {
        self.parts.iter().all(|p| matches!(p, Part::Lit(_)))
    }

    pub fn vars(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match p {
            Part::Var(n) => Some(n.as_str()),
            _ => None,
        })
    }

    pub fn secrets(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match p {
            Part::Secret(n) => Some(n.as_str()),
            _ => None,
        })
    }

    pub fn render(&self, scope: &dyn Scope) -> Result<String, String> {
        self.render_with(scope, |s| s.to_string())
    }

    /// Como `render`, pero pasa por `encode` lo interpolado (no el texto literal).
    pub fn render_with(
        &self,
        scope: &dyn Scope,
        encode: impl Fn(&str) -> String,
    ) -> Result<String, String> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Lit(s) => out.push_str(s),
                other => out.push_str(&encode(&to_text(&resolve(other, scope)?))),
            }
        }
        Ok(out)
    }

    /// Valor con tipo: si el template es exactamente un `{{...}}`, conserva su tipo.
    pub fn render_value(&self, scope: &dyn Scope) -> Result<Value, String> {
        match self.parts.as_slice() {
            [part] if !matches!(part, Part::Lit(_)) => resolve(part, scope),
            _ => self.render(scope).map(Value::String),
        }
    }
}

fn parse_expr(e: &str) -> Result<Part, String> {
    if let Some(name) = e.strip_prefix("env.") {
        return if valid_name(name) {
            Ok(Part::Secret(name.to_string()))
        } else {
            Err(crate::tr!(
                "invalid secret name in `{{{{{e}}}}}`",
                "nombre de secreto inválido en `{{{{{e}}}}}`"
            ))
        };
    }
    match e {
        "$uuid" => return Ok(Part::Uuid),
        "$timestamp" => return Ok(Part::Timestamp),
        _ => {}
    }
    if let Some(args) = e
        .strip_prefix("$randomInt(")
        .and_then(|a| a.strip_suffix(')'))
    {
        let bad = || {
            crate::tr!(
                "`{{{{{e}}}}}`: use $randomInt(min,max) with integers and min <= max",
                "`{{{{{e}}}}}`: usá $randomInt(min,max) con enteros y min <= max"
            )
        };
        let (a, b) = args.split_once(',').ok_or_else(bad)?;
        let (a, b): (i64, i64) = (
            a.trim().parse().map_err(|_| bad())?,
            b.trim().parse().map_err(|_| bad())?,
        );
        return if a <= b {
            Ok(Part::RandomInt(a, b))
        } else {
            Err(bad())
        };
    }
    if e.starts_with('$') {
        return Err(crate::tr!(
            "unknown placeholder `{{{{{e}}}}}` (supported: $uuid, $timestamp, $randomInt(min,max))",
            "placeholder desconocido `{{{{{e}}}}}` (soportados: $uuid, $timestamp, $randomInt(min,max))"
        ));
    }
    // `dataset.columna` es una columna de la fila de la iteración (ADR-007).
    let is_column = e
        .split_once('.')
        .is_some_and(|(d, c)| valid_name(d) && valid_name(c));
    if valid_name(e) || is_column {
        Ok(Part::Var(e.to_string()))
    } else {
        Err(crate::tr!(
            "`{{{{{e}}}}}` is not a valid variable name (letters, digits and `_`, or `dataset.column`)",
            "`{{{{{e}}}}}` no es un nombre de variable válido (letras, números y `_`, o `dataset.columna`)"
        ))
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn resolve(part: &Part, scope: &dyn Scope) -> Result<Value, String> {
    Ok(match part {
        Part::Lit(s) => Value::String(s.clone()),
        Part::Var(n) => scope.var(n).cloned().ok_or_else(|| {
            crate::tr!(
                "variable `{n}` has no value",
                "la variable `{n}` no tiene valor"
            )
        })?,
        Part::Secret(n) => Value::String(
            scope
                .secret(n)
                .ok_or_else(|| {
                    crate::tr!(
                        "secret `{n}` has no value",
                        "el secreto `{n}` no tiene valor"
                    )
                })?
                .to_string(),
        ),
        Part::Uuid => Value::String(uuid_v4()),
        Part::Timestamp => Value::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        ),
        Part::RandomInt(a, b) => Value::from(fastrand::i64(*a..=*b)),
    })
}

/// Cómo se ve un valor dentro de un texto.
pub fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn uuid_v4() -> String {
    let mut b: [u8; 16] = std::array::from_fn(|_| fastrand::u8(..));
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// Un JSON con plantillas en sus strings.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueTemplate {
    /// Sin plantillas: se envía tal cual.
    Lit(Value),
    Str(Template),
    Arr(Vec<ValueTemplate>),
    Obj(Vec<(String, ValueTemplate)>),
}

impl ValueTemplate {
    pub fn parse(v: &Value) -> Result<Self, String> {
        let parsed = match v {
            Value::String(s) if s.contains("{{") => Self::Str(Template::parse(s)?),
            Value::Array(items) => {
                Self::Arr(items.iter().map(Self::parse).collect::<Result<_, _>>()?)
            }
            Value::Object(map) => Self::Obj(
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), Self::parse(v)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => Self::Lit(other.clone()),
        };
        Ok(match parsed {
            Self::Arr(_) | Self::Obj(_) if parsed.is_literal() => Self::Lit(v.clone()),
            p => p,
        })
    }

    fn is_literal(&self) -> bool {
        match self {
            Self::Lit(_) => true,
            Self::Str(_) => false,
            Self::Arr(items) => items.iter().all(Self::is_literal),
            Self::Obj(fields) => fields.iter().all(|(_, v)| v.is_literal()),
        }
    }

    pub fn templates(&self) -> Vec<&Template> {
        match self {
            Self::Lit(_) => vec![],
            Self::Str(t) => vec![t],
            Self::Arr(items) => items.iter().flat_map(Self::templates).collect(),
            Self::Obj(fields) => fields.iter().flat_map(|(_, v)| v.templates()).collect(),
        }
    }

    pub fn render(&self, scope: &dyn Scope) -> Result<Value, String> {
        Ok(match self {
            Self::Lit(v) => v.clone(),
            Self::Str(t) => t.render_value(scope)?,
            Self::Arr(items) => Value::Array(
                items
                    .iter()
                    .map(|i| i.render(scope))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Obj(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.render(scope)?)))
                    .collect::<Result<_, String>>()?,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    struct S(HashMap<&'static str, Value>);

    impl Scope for S {
        fn var(&self, name: &str) -> Option<&Value> {
            self.0.get(name)
        }
        fn secret(&self, name: &str) -> Option<&str> {
            (name == "PASS").then_some("s3cret")
        }
    }

    fn scope() -> S {
        S(HashMap::from([
            ("id", json!(42)),
            ("name", json!("ana")),
            ("tags", json!(["a", "b"])),
        ]))
    }

    #[test]
    fn renders_text_and_preserves_types() {
        let t = Template::parse("/orders/{{ id }}?by={{name}}").unwrap();
        assert_eq!(t.render(&scope()).unwrap(), "/orders/42?by=ana");
        assert_eq!(t.vars().collect::<Vec<_>>(), ["id", "name"]);

        let body = ValueTemplate::parse(&json!({
            "id": "{{id}}", "label": "n-{{id}}", "tags": "{{tags}}", "pass": "{{env.PASS}}", "fixed": [1, 2]
        }))
        .unwrap();
        assert_eq!(
            body.render(&scope()).unwrap(),
            json!({ "id": 42, "label": "n-42", "tags": ["a", "b"], "pass": "s3cret", "fixed": [1, 2] })
        );
    }

    #[test]
    fn dynamic_placeholders() {
        let s = scope();
        let n = Template::parse("{{$randomInt(5, 7)}}")
            .unwrap()
            .render_value(&s)
            .unwrap();
        assert!((5..=7).contains(&n.as_i64().unwrap()));
        let u = Template::parse("{{$uuid}}").unwrap().render(&s).unwrap();
        assert_eq!((u.len(), &u[14..15]), (36, "4"));
        assert!(
            Template::parse("{{$timestamp}}")
                .unwrap()
                .render_value(&s)
                .unwrap()
                .is_u64()
        );
    }

    #[test]
    fn rejects_bad_syntax() {
        assert!(Template::parse("{{id").unwrap_err().contains("cerrar"));
        assert!(
            Template::parse("{{$now}}")
                .unwrap_err()
                .contains("desconocido")
        );
        assert!(Template::parse("{{$randomInt(9,1)}}").is_err());
        assert!(Template::parse("{{mi-var}}").is_err());
        assert!(Template::parse("{{env.}}").is_err());
        assert!(Template::parse("{{a.b.c}}").is_err());
        assert_eq!(
            Template::parse("{{users.email}}")
                .unwrap()
                .vars()
                .collect::<Vec<_>>(),
            ["users.email"]
        );
    }

    #[test]
    fn literal_json_stays_literal() {
        assert_eq!(
            ValueTemplate::parse(&json!({"a": [1]})).unwrap(),
            ValueTemplate::Lit(json!({"a": [1]}))
        );
    }
}
