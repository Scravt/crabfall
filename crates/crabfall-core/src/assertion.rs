//! Checks: verificaciones no bloqueantes sobre cada respuesta (ADR-002).

use hyper::{HeaderMap, header::HeaderName};
use regex::Regex;
use serde_json::Value;

use crate::{
    extract::JsonPath,
    scenario::Check,
    template::{Scope, ValueTemplate, to_text},
};

#[derive(Debug, Clone)]
enum Subject {
    Status(Vec<u16>),
    Json(JsonPath, Predicate),
    Header(HeaderName, Predicate),
    BodyContains(String),
    BodyMatches(Regex),
}

#[derive(Debug, Clone)]
enum Predicate {
    Exists(bool),
    Equals(ValueTemplate),
    Contains(String),
    Matches(Regex),
}

/// Un check listo para evaluar.
#[derive(Debug, Clone)]
pub struct Assertion {
    /// Cómo se muestra: `status == 201`, `$.id exists`, `header content-type contains json`.
    pub label: String,
    subject: Subject,
}

impl Assertion {
    pub fn parse(c: &Check) -> Result<Self, String> {
        let subjects = [
            c.status.is_some(),
            c.status_in.is_some(),
            c.json.is_some(),
            c.header.is_some(),
            c.body_contains.is_some(),
            c.body_matches.is_some(),
        ];
        if subjects.iter().filter(|s| **s).count() != 1 {
            return Err(crate::tr!(
                "use exactly one of: status, status_in, json, header, body_contains, body_matches",
                "usá exactamente uno de: status, status_in, json, header, body_contains, body_matches"
            ));
        }
        let regex = |r: &str| {
            Regex::new(r).map_err(|e| crate::tr!("invalid regex: {e}", "regex inválida: {e}"))
        };

        let (label, subject) = if let Some(code) = c.status {
            no_predicate(c)?;
            (format!("status == {code}"), Subject::Status(vec![code]))
        } else if let Some(codes) = &c.status_in {
            no_predicate(c)?;
            if codes.is_empty() {
                return Err(crate::tr!(
                    "status_in cannot be empty",
                    "status_in no puede estar vacío"
                ));
            }
            (
                format!("status in {codes:?}"),
                Subject::Status(codes.clone()),
            )
        } else if let Some(text) = &c.body_contains {
            no_predicate(c)?;
            (
                format!("body contains {text:?}"),
                Subject::BodyContains(text.clone()),
            )
        } else if let Some(r) = &c.body_matches {
            no_predicate(c)?;
            (
                format!("body matches /{r}/"),
                Subject::BodyMatches(regex(r)?),
            )
        } else if let Some(path) = &c.json {
            let (desc, pred) = predicate(c, false)?;
            (
                format!("{path} {desc}"),
                Subject::Json(JsonPath::parse(path)?, pred),
            )
        } else {
            let name = c.header.as_deref().expect("hay un sujeto");
            let header = HeaderName::try_from(name).map_err(|e| {
                crate::tr!(
                    "invalid header `{name}`: {e}",
                    "header `{name}` inválido: {e}"
                )
            })?;
            let (desc, pred) = predicate(c, true)?;
            (
                format!("header {name} {desc}"),
                Subject::Header(header, pred),
            )
        };
        Ok(Self { label, subject })
    }

    /// Variables que usa (`equals: "{{x}}"`).
    pub fn vars(&self) -> Vec<&str> {
        match &self.subject {
            Subject::Json(_, Predicate::Equals(t)) | Subject::Header(_, Predicate::Equals(t)) => {
                t.templates().into_iter().flat_map(|t| t.vars()).collect()
            }
            _ => vec![],
        }
    }

    /// Verdadero si es un check de status.
    pub fn is_status(&self) -> bool {
        matches!(self.subject, Subject::Status(_))
    }

    /// `Ok(())` si pasa; si no, por qué.
    pub fn evaluate(
        &self,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
        scope: &dyn Scope,
    ) -> Result<(), String> {
        match &self.subject {
            Subject::Status(codes) => {
                if codes.contains(&status) {
                    Ok(())
                } else {
                    Err(crate::tr!("responded {status}", "respondió {status}"))
                }
            }
            Subject::BodyContains(text) => {
                if String::from_utf8_lossy(body).contains(text.as_str()) {
                    Ok(())
                } else {
                    Err(crate::tr!(
                        "the body does not contain it",
                        "el body no lo contiene"
                    ))
                }
            }
            Subject::BodyMatches(re) => {
                if re.is_match(&String::from_utf8_lossy(body)) {
                    Ok(())
                } else {
                    Err(crate::tr!("the body does not match", "el body no coincide"))
                }
            }
            Subject::Json(path, pred) => {
                let json: Option<Value> = serde_json::from_slice(body).ok();
                let found = json.as_ref().and_then(|j| path.get(j));
                compare(pred, found.cloned(), scope)
            }
            Subject::Header(name, pred) => {
                let found = headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| Value::String(v.into()));
                compare(pred, found, scope)
            }
        }
    }
}

fn compare(pred: &Predicate, found: Option<Value>, scope: &dyn Scope) -> Result<(), String> {
    match (pred, found) {
        (Predicate::Exists(true), Some(_)) | (Predicate::Exists(false), None) => Ok(()),
        (Predicate::Exists(true), None) => Err(crate::tr!("does not exist", "no existe")),
        (Predicate::Exists(false), Some(_)) => Err("existe".into()),
        (_, None) => Err(crate::tr!("does not exist", "no existe")),
        (Predicate::Equals(expected), Some(v)) => {
            let expected = expected.render(scope)?;
            if v == expected {
                Ok(())
            } else {
                Err(crate::tr!("is {v}", "vale {v}"))
            }
        }
        (Predicate::Contains(text), Some(v)) => {
            if to_text(&v).contains(text.as_str()) {
                Ok(())
            } else {
                Err(crate::tr!("is {v}", "vale {v}"))
            }
        }
        (Predicate::Matches(re), Some(v)) => {
            if re.is_match(&to_text(&v)) {
                Ok(())
            } else {
                Err(crate::tr!("is {v}", "vale {v}"))
            }
        }
    }
}

fn no_predicate(c: &Check) -> Result<(), String> {
    if c.exists.is_some() || c.equals.is_some() || c.contains.is_some() || c.matches.is_some() {
        Err(crate::tr!(
            "exists, equals, contains and matches only go with `json` or `header`",
            "exists, equals, contains y matches solo van con `json` o `header`"
        ))
    } else {
        Ok(())
    }
}

fn predicate(c: &Check, header: bool) -> Result<(String, Predicate), String> {
    let count = [
        c.exists.is_some(),
        c.equals.is_some(),
        c.contains.is_some(),
        c.matches.is_some(),
    ]
    .iter()
    .filter(|p| **p)
    .count();
    if count != 1 {
        return Err(crate::tr!(
            "use exactly one of: exists, equals, contains, matches",
            "usá exactamente uno de: exists, equals, contains, matches"
        ));
    }
    if let Some(e) = c.exists {
        Ok((
            if e {
                "exists".into()
            } else {
                "not exists".into()
            },
            Predicate::Exists(e),
        ))
    } else if let Some(v) = &c.equals {
        Ok((
            format!("== {v}"),
            Predicate::Equals(ValueTemplate::parse(v)?),
        ))
    } else if let Some(text) = &c.contains {
        if !header {
            return Err(crate::tr!(
                "`contains` goes with `header`; for JSON use `equals` or `matches`",
                "`contains` va con `header`; para JSON usá `equals` o `matches`"
            ));
        }
        Ok((
            format!("contains {text:?}"),
            Predicate::Contains(text.clone()),
        ))
    } else {
        let r = c.matches.as_deref().expect("hay un predicado");
        let re =
            Regex::new(r).map_err(|e| crate::tr!("invalid regex: {e}", "regex inválida: {e}"))?;
        Ok((format!("matches /{r}/"), Predicate::Matches(re)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct S;
    impl Scope for S {
        fn var(&self, name: &str) -> Option<&Value> {
            static ID: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
            (name == "id").then(|| ID.get_or_init(|| json!(5)))
        }
        fn secret(&self, _: &str) -> Option<&str> {
            None
        }
    }

    fn check(yaml: &str) -> Assertion {
        Assertion::parse(&serde_saphyr::from_str::<Check>(yaml).unwrap()).unwrap()
    }

    #[test]
    fn evaluates_each_kind() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            "application/json; charset=utf-8".parse().unwrap(),
        );
        let body = br#"{"id":5,"status":"paid"}"#;
        let eval = |yaml: &str, status: u16| check(yaml).evaluate(status, &headers, body, &S);

        assert_eq!(eval("status: 201", 201), Ok(()));
        assert_eq!(eval("status: 201", 500), Err("respondió 500".into()));
        assert_eq!(eval("status_in: [200, 201]", 200), Ok(()));
        assert_eq!(eval("{ json: $.id, exists: true }", 200), Ok(()));
        assert_eq!(eval("{ json: $.nope, exists: false }", 200), Ok(()));
        assert_eq!(eval("{ json: $.id, equals: \"{{id}}\" }", 200), Ok(()));
        assert_eq!(
            eval("{ json: $.status, equals: refunded }", 200),
            Err("vale \"paid\"".into())
        );
        assert_eq!(eval("{ json: $.status, matches: \"^pa\" }", 200), Ok(()));
        assert_eq!(
            eval("{ header: content-type, contains: json }", 200),
            Ok(())
        );
        assert_eq!(
            eval("{ header: x-missing, exists: true }", 200),
            Err("no existe".into())
        );
        assert_eq!(eval("body_contains: paid", 200), Ok(()));
        assert_eq!(eval("body_matches: \"\\\"id\\\":\\\\d+\"", 200), Ok(()));
    }

    #[test]
    fn labels_and_rules() {
        assert_eq!(check("{ json: $.id, exists: true }").label, "$.id exists");
        assert_eq!(
            check("{ header: content-type, contains: json }").label,
            "header content-type contains \"json\""
        );
        let bad = |yaml: &str| {
            Assertion::parse(&serde_saphyr::from_str::<Check>(yaml).unwrap()).unwrap_err()
        };
        assert!(bad("{ status: 200, json: $.id }").contains("exactamente uno"));
        assert!(bad("{ json: $.id }").contains("exactamente uno de: exists"));
        assert!(bad("{ status: 200, exists: true }").contains("solo van con"));
        assert!(bad("{ json: $.id, contains: x }").contains("va con `header`"));
    }
}
