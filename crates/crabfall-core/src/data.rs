//! Datasets: filas de un CSV o JSON que se reparten entre las iteraciones (ADR-007).
//!
//! Cada iteración toma una fila de cada dataset que usa su flow y la ve como
//! `{{dataset.columna}}` en todos sus pasos.

use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::scenario::{DataSpec, Pick};

/// Un dataset cargado. Las filas tienen una celda por columna, en el orden de `columns`.
#[derive(Debug, Clone)]
pub struct Dataset {
    pub name: String,
    /// Ruta tal como está en el escenario.
    pub file: String,
    pub pick: Pick,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// SHA-256 del archivo: lo firma la aprobación.
    pub sha256: String,
}

impl Dataset {
    /// Lee `spec.file`, relativo a `dir` (la carpeta `.crabfall/`).
    pub fn load(dir: &Path, name: &str, spec: &DataSpec) -> Result<Self, String> {
        let path = dir.join(&spec.file);
        let bytes = std::fs::read(&path).map_err(|e| {
            crate::tr!(
                "data.{name}: could not read {}: {e}",
                "data.{name}: no se pudo leer {}: {e}",
                path.display()
            )
        })?;
        Self::parse(name, spec, &bytes).map_err(|e| format!("data.{name} ({}): {e}", spec.file))
    }

    pub fn parse(name: &str, spec: &DataSpec, bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            crate::tr!("the file is not UTF-8", "el archivo no es UTF-8").to_string()
        })?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let (columns, rows) = match extension(&spec.file).as_deref() {
            Some("csv") => parse_csv(text)?,
            Some("json") => parse_json(text)?,
            _ => {
                return Err(crate::tr!(
                    "use a .csv or .json file",
                    "usá un archivo .csv o .json"
                ));
            }
        };
        if rows.is_empty() {
            return Err(crate::tr!("it has no rows", "no tiene filas"));
        }
        if let Some(bad) = columns.iter().find(|c| !valid_column(c)) {
            return Err(crate::tr!(
                "column `{bad}` cannot be used as a name: use letters, digits and `_`, not starting with a digit",
                "la columna `{bad}` no sirve como nombre: usá letras, números y `_`, sin empezar con un número"
            ));
        }
        Ok(Self {
            name: name.to_string(),
            file: spec.file.clone(),
            pick: spec.pick,
            columns,
            rows,
            sha256: Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        })
    }

    /// Variables de una fila: `dataset.columna` → valor.
    pub fn vars(&self, row: usize) -> impl Iterator<Item = (String, Value)> + '_ {
        self.columns
            .iter()
            .zip(&self.rows[row])
            .map(|(c, v)| (format!("{}.{c}", self.name), v.clone()))
    }

    /// `users  data/users.csv · 120 filas · sequential`, para los resúmenes.
    pub fn describe(&self) -> String {
        crate::tr!(
            "{}  {} · {} rows · {}",
            "{}  {} · {} filas · {}",
            self.name,
            self.file,
            self.rows.len(),
            self.pick.as_str()
        )
    }
}

fn extension(file: &str) -> Option<String> {
    Path::new(file)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

fn valid_column(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// CSV con encabezado. Todas las celdas son strings: para otros tipos, JSON.
fn parse_csv(text: &str) -> Result<(Vec<String>, Vec<Vec<Value>>), String> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::Headers)
        .from_reader(text.as_bytes());
    let columns: Vec<String> = reader
        .headers()
        .map_err(|e| crate::tr!("invalid header row: {e}", "encabezado inválido: {e}"))?
        .iter()
        .map(str::to_string)
        .collect();
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = columns.iter().find(|c| !seen.insert(c.as_str())) {
        return Err(crate::tr!(
            "column `{dup}` is repeated",
            "la columna `{dup}` está repetida"
        ));
    }
    let mut rows = Vec::new();
    for (i, record) in reader.records().enumerate() {
        // La fila 1 es el encabezado.
        let record = record.map_err(|e| crate::tr!("row {}: {e}", "fila {}: {e}", i + 2))?;
        rows.push(
            record
                .iter()
                .map(|cell| Value::String(cell.to_string()))
                .collect(),
        );
    }
    Ok((columns, rows))
}

/// Arreglo de objetos. Las columnas son las claves del primero; las demás filas
/// tienen que tener las mismas. Conserva los tipos.
fn parse_json(text: &str) -> Result<(Vec<String>, Vec<Vec<Value>>), String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| crate::tr!("invalid JSON: {e}", "JSON inválido: {e}"))?;
    let Value::Array(items) = value else {
        return Err(crate::tr!(
            "it must be an array of objects: [{{...}}, {{...}}]",
            "tiene que ser un arreglo de objetos: [{{...}}, {{...}}]"
        ));
    };
    let mut columns: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        let Value::Object(mut object) = item else {
            return Err(crate::tr!(
                "[{i}] is not an object",
                "[{i}] no es un objeto"
            ));
        };
        if i == 0 {
            columns = object.keys().cloned().collect();
        }
        let row = columns
            .iter()
            .map(|c| {
                object.remove(c).ok_or_else(|| {
                    crate::tr!(
                        "[{i}] has no `{c}` (columns come from [0])",
                        "[{i}] no tiene `{c}` (las columnas salen de [0])"
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(extra) = object.keys().next() {
            return Err(crate::tr!(
                "[{i}] has `{extra}`, which is not in [0]: every row has the same keys",
                "[{i}] tiene `{extra}`, que no está en [0]: todas las filas llevan las mismas claves"
            ));
        }
        rows.push(row);
    }
    Ok((columns, rows))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn spec(file: &str) -> DataSpec {
        DataSpec {
            file: file.into(),
            pick: Pick::Sequential,
        }
    }

    #[test]
    fn csv_with_header_gives_strings() {
        let d = Dataset::parse(
            "users",
            &spec("data/users.CSV"),
            b"\xef\xbb\xbfemail, password\nana@x.com,\"a,b\"\nbeto@x.com,42\n",
        )
        .unwrap();
        assert_eq!(d.columns, ["email", "password"]);
        assert_eq!(d.rows[0], [json!("ana@x.com"), json!("a,b")]);
        assert_eq!(d.rows[1][1], json!("42"));
        assert_eq!(
            d.vars(1).collect::<Vec<_>>(),
            [
                ("users.email".to_string(), json!("beto@x.com")),
                ("users.password".to_string(), json!("42"))
            ]
        );
        assert_eq!(d.sha256.len(), 64);
    }

    #[test]
    fn json_keeps_types() {
        let d = Dataset::parse(
            "products",
            &spec("p.json"),
            br#"[{"id": 1, "tags": ["a"]}, {"tags": [], "id": 2}]"#,
        )
        .unwrap();
        assert_eq!(d.columns, ["id", "tags"]);
        assert_eq!(d.rows[1], [json!(2), json!([])]);
    }

    #[test]
    fn rejects_bad_files() {
        let err =
            |file: &str, text: &str| Dataset::parse("d", &spec(file), text.as_bytes()).unwrap_err();
        assert!(err("d.txt", "a\n1").contains(".csv o .json"));
        assert!(err("d.csv", "a\n").contains("no tiene filas"));
        assert!(err("d.csv", "first name\nana").contains("`first name`"));
        assert!(err("d.csv", "a,a\n1,2").contains("repetida"));
        assert!(err("d.csv", "a,b\n1,2\n3").contains("fila 3"));
        assert!(err("d.json", r#"{"a": 1}"#).contains("arreglo"));
        assert!(err("d.json", r#"[{"a": 1}, {"b": 2}]"#).contains("[1] no tiene `a`"));
        assert!(err("d.json", r#"[{"a": 1}, {"a": 2, "b": 3}]"#).contains("`b`"));
    }
}
