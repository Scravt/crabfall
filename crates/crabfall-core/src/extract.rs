//! `extract`: valores de una respuesta que pasan a ser variables (ADR-002 §6).
//!
//! - `{ json: "$.data.items[0].id" }`: JSONPath simple (`.clave`, `['clave']`, `[n]`). Conserva el tipo.
//! - `{ header: "location" }`: valor de un header.
//! - `{ regex: "id=(\\d+)" }`: primer grupo de captura (o todo el match) sobre el body.

use hyper::{HeaderMap, header::HeaderName};
use regex::Regex;
use serde_json::Value;

use crate::scenario::Extract;

#[derive(Debug, Clone)]
pub enum Extractor {
    Json(JsonPath),
    Header(HeaderName),
    Regex(Regex),
}

impl Extractor {
    pub fn parse(spec: &Extract) -> Result<Self, String> {
        match (&spec.json, &spec.header, &spec.regex) {
            (Some(path), None, None) => JsonPath::parse(path).map(Self::Json),
            (None, Some(name), None) => HeaderName::try_from(name.as_str())
                .map(Self::Header)
                .map_err(|e| format!("header `{name}` inválido: {e}")),
            (None, None, Some(pattern)) => Regex::new(pattern)
                .map(Self::Regex)
                .map_err(|e| format!("regex inválida: {e}")),
            _ => Err("usá exactamente uno de `json`, `header` o `regex`".into()),
        }
    }

    /// Cómo se describe en mensajes y en la firma.
    pub fn source(&self) -> String {
        match self {
            Self::Json(p) => format!("json {}", p.source),
            Self::Header(h) => format!("header {h}"),
            Self::Regex(r) => format!("regex {}", r.as_str()),
        }
    }

    pub fn apply(&self, headers: &HeaderMap, body: &[u8]) -> Result<Value, String> {
        match self {
            Self::Json(path) => {
                let json: Value = serde_json::from_slice(body)
                    .map_err(|_| "la respuesta no es JSON".to_string())?;
                path.get(&json)
                    .cloned()
                    .ok_or_else(|| format!("{} no existe en la respuesta", path.source))
            }
            Self::Header(name) => headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| Value::String(v.to_string()))
                .ok_or_else(|| format!("la respuesta no tiene el header `{name}`")),
            Self::Regex(re) => {
                let text = String::from_utf8_lossy(body);
                let caps = re
                    .captures(&text)
                    .ok_or_else(|| format!("la regex `{}` no encontró nada", re.as_str()))?;
                let m = caps
                    .get(1)
                    .or_else(|| caps.get(0))
                    .expect("un match tiene grupo 0");
                Ok(Value::String(m.as_str().to_string()))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonPath {
    source: String,
    segments: Vec<Segment>,
}

impl JsonPath {
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = |why: &str| {
            format!("JSONPath `{s}` inválido: {why} (ejemplos: $.id, $.items[0].name, $['a-b'])")
        };
        let mut rest = s
            .strip_prefix('$')
            .ok_or_else(|| bad("tiene que empezar con `$`"))?;
        let mut segments = Vec::new();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('.') {
                let end = after.find(['.', '[']).unwrap_or(after.len());
                let key = &after[..end];
                if key.is_empty() {
                    return Err(bad("falta el nombre después de `.`"));
                }
                segments.push(Segment::Key(key.to_string()));
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix("['") {
                let end = after.find("']").ok_or_else(|| bad("falta cerrar `']`"))?;
                segments.push(Segment::Key(after[..end].to_string()));
                rest = &after[end + 2..];
            } else if let Some(after) = rest.strip_prefix('[') {
                let end = after.find(']').ok_or_else(|| bad("falta cerrar `]`"))?;
                let index = after[..end]
                    .trim()
                    .parse()
                    .map_err(|_| bad("el índice tiene que ser un entero"))?;
                segments.push(Segment::Index(index));
                rest = &after[end + 1..];
            } else {
                return Err(bad("se esperaba `.` o `[`"));
            }
        }
        Ok(Self {
            source: s.to_string(),
            segments,
        })
    }

    pub fn get<'a>(&self, mut value: &'a Value) -> Option<&'a Value> {
        for segment in &self.segments {
            value = match segment {
                Segment::Key(k) => value.get(k)?,
                Segment::Index(i) => value.get(i)?,
            };
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_paths() {
        let v = json!({ "data": { "items": [{ "id": 7 }], "a-b": true } });
        let get = |p: &str| JsonPath::parse(p).unwrap().get(&v).cloned();
        assert_eq!(get("$.data.items[0].id"), Some(json!(7)));
        assert_eq!(get("$['data']['a-b']"), Some(json!(true)));
        assert_eq!(get("$.data.items[3]"), None);
        assert_eq!(get("$"), Some(v.clone()));
        assert!(JsonPath::parse("data.id").is_err());
        assert!(JsonPath::parse("$.items[x]").is_err());
        assert!(JsonPath::parse("$.").is_err());
    }

    #[test]
    fn extractors() {
        let mut headers = HeaderMap::new();
        headers.insert("location", "/orders/9".parse().unwrap());
        let body = br#"{"token":"abc","n":3}"#;
        let spec = |json: Option<&str>, header: Option<&str>, regex: Option<&str>| Extract {
            json: json.map(String::from),
            header: header.map(String::from),
            regex: regex.map(String::from),
        };
        let run = |e: Extract| Extractor::parse(&e).unwrap().apply(&headers, body);

        assert_eq!(run(spec(Some("$.n"), None, None)), Ok(json!(3)));
        assert_eq!(
            run(spec(None, Some("location"), None)),
            Ok(json!("/orders/9"))
        );
        assert_eq!(
            run(spec(None, None, Some(r#""token":"(\w+)""#))),
            Ok(json!("abc"))
        );
        assert!(
            run(spec(Some("$.missing"), None, None))
                .unwrap_err()
                .contains("no existe")
        );
        assert!(Extractor::parse(&spec(Some("$.a"), Some("x"), None)).is_err());
    }
}
