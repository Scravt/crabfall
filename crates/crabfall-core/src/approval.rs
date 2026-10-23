//! Aprobaciones firmadas (ADR-004 §1-§3).
//!
//! Una aprobación firma con HMAC-SHA256 el hash del plan canónico (escenario + entorno
//! resuelto). La clave vive en el keyring del sistema, nunca en el repo. Cualquier cambio
//! en el tráfico cambia el hash, y la aprobación deja de valer.

use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{config::Environment, engine::plan::Plan};

pub const FORMAT: u32 = 1;
const KEYRING_SERVICE: &str = "crabfall";
const KEYRING_USER: &str = "approval-key";

/// Dónde vive la clave de firma.
pub trait KeyStore {
    /// La clave, si ya existe.
    fn get(&self) -> Result<Option<Vec<u8>>, String>;
    /// La clave; la crea si no existe.
    fn get_or_create(&self) -> Result<Vec<u8>, String>;
}

/// Keyring del sistema: Windows Credential Manager, Keychain o Secret Service.
pub struct OsKeyring;

impl OsKeyring {
    fn entry() -> Result<keyring::Entry, String> {
        Self::entry_for(KEYRING_SERVICE)
    }

    fn entry_for(service: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(service, KEYRING_USER).map_err(|e| format!("keyring: {e}"))
    }
}

impl KeyStore for OsKeyring {
    fn get(&self) -> Result<Option<Vec<u8>>, String> {
        match Self::entry()?.get_password() {
            Ok(hex_key) => from_hex(&hex_key).map(Some).ok_or(crate::tr!(
                "the keyring key is damaged",
                "la clave del keyring está dañada"
            )),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("keyring: {e}")),
        }
    }

    fn get_or_create(&self) -> Result<Vec<u8>, String> {
        if let Some(key) = self.get()? {
            return Ok(key);
        }
        let mut key = vec![0u8; 32];
        getrandom::fill(&mut key).map_err(|e| {
            crate::tr!(
                "could not generate the key: {e}",
                "no se pudo generar la clave: {e}"
            )
        })?;
        Self::entry()?
            .set_password(&to_hex(&key))
            .map_err(|e| format!("keyring: {e}"))?;
        Ok(key)
    }
}

/// Clave en memoria, para tests.
pub struct MemoryKey(pub Vec<u8>);

impl KeyStore for MemoryKey {
    fn get(&self) -> Result<Option<Vec<u8>>, String> {
        Ok(Some(self.0.clone()))
    }
    fn get_or_create(&self) -> Result<Vec<u8>, String> {
        Ok(self.0.clone())
    }
}

/// Archivo `.crabfall/approvals/<escenario>.<entorno>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    pub format: u32,
    pub scenario: String,
    pub environment: String,
    pub hash: String,
    pub approved_at: String,
    pub approved_by: String,
    /// El plan canónico aprobado, para mostrar qué cambió después.
    pub plan: Value,
    /// Flags con los que se aprobó (`--var`, `--rate`...), para sugerir el comando de `run`.
    /// No se firma: es una ayuda; lo que autoriza es el hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    pub signature: String,
}

#[derive(Debug, PartialEq)]
pub enum Rejection {
    /// No hay aprobación para este escenario y entorno.
    Missing,
    /// La aprobación no es auténtica: editada, copiada de otro escenario o firmada con otra clave.
    Tampered(String),
    /// El escenario o el entorno cambiaron desde la aprobación. Trae el diff.
    Changed(String),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str(&crate::tr!("it is not approved", "no está aprobado")),
            Self::Tampered(why) => f.write_str(&crate::tr!(
                "the approval is not valid: {why}",
                "la aprobación no es válida: {why}"
            )),
            Self::Changed(_) => f.write_str(&crate::tr!(
                "it changed since the last approval",
                "cambió desde la última aprobación"
            )),
        }
    }
}

/// Hash SHA-256 (hex) del plan canónico.
pub fn fingerprint(canonical: &Value) -> String {
    to_hex(&Sha256::digest(
        serde_json::to_vec(canonical).expect("JSON siempre serializa"),
    ))
}

pub fn path(approvals_dir: &Path, scenario: &str, environment: &str) -> PathBuf {
    approvals_dir.join(format!("{scenario}.{environment}.json"))
}

/// Firma el plan actual. Quien llama es responsable de haber pedido la confirmación humana.
pub fn approve(
    plan: &Plan,
    env: &Environment,
    environment: &str,
    approved_by: &str,
    approved_at: &str,
    keys: &dyn KeyStore,
) -> Result<Approval, String> {
    let key = keys.get_or_create()?;
    let canonical = plan.canonical(env);
    let mut approval = Approval {
        format: FORMAT,
        scenario: plan.scenario.clone(),
        environment: environment.to_string(),
        hash: fingerprint(&canonical),
        approved_at: approved_at.to_string(),
        approved_by: approved_by.to_string(),
        plan: canonical,
        flags: Vec::new(),
        signature: String::new(),
    };
    approval.signature = to_hex(&sign(&key, &approval));
    Ok(approval)
}

/// Verifica que `approval` autorice exactamente este plan en este entorno.
pub fn verify(
    approval: Option<&Approval>,
    plan: &Plan,
    env: &Environment,
    environment: &str,
    keys: &dyn KeyStore,
) -> Result<(), Rejection> {
    let approval = approval.ok_or(Rejection::Missing)?;
    if approval.format != FORMAT {
        return Err(Rejection::Tampered(crate::tr!(
            "unknown format {}",
            "formato {} desconocido",
            approval.format
        )));
    }
    if approval.scenario != plan.scenario || approval.environment != environment {
        return Err(Rejection::Tampered(crate::tr!(
            "it is for `{}` in `{}`, not for `{}` in `{environment}`",
            "es de `{}` en `{}`, no de `{}` en `{environment}`",
            approval.scenario,
            approval.environment,
            plan.scenario
        )));
    }
    let key = keys.get().map_err(Rejection::Tampered)?.ok_or_else(|| {
        Rejection::Tampered(crate::tr!(
            "there is no signing key on this machine",
            "no hay clave de firma en este equipo"
        ))
    })?;
    let signature = from_hex(&approval.signature)
        .ok_or_else(|| Rejection::Tampered(crate::tr!("unreadable signature", "firma ilegible")))?;
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key)
        .expect("HMAC acepta cualquier largo de clave");
    mac.update(&signed_message(approval));
    if mac.verify_slice(&signature).is_err() || fingerprint(&approval.plan) != approval.hash {
        return Err(Rejection::Tampered(crate::tr!(
            "the signature does not match (was the file edited?)",
            "la firma no coincide (¿se editó el archivo?)"
        )));
    }

    let canonical = plan.canonical(env);
    if fingerprint(&canonical) != approval.hash {
        return Err(Rejection::Changed(diff(&approval.plan, &canonical)));
    }
    Ok(())
}

/// Diff unificado entre el plan aprobado y el actual.
pub fn diff(before: &Value, after: &Value) -> String {
    let before = serde_json::to_string_pretty(before).unwrap();
    let after = serde_json::to_string_pretty(after).unwrap();
    similar::TextDiff::from_lines(&before, &after)
        .unified_diff()
        .context_radius(2)
        .header("aprobado", "actual")
        .to_string()
}

/// Permiso para correr de nuevo un plan aprobado sin volver a preguntar, hasta un momento
/// (ADR-017). Archivo `.crabfall/approvals/<escenario>.<entorno>.allow.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allowance {
    pub format: u32,
    pub scenario: String,
    pub environment: String,
    /// El hash del plan aprobado: si el plan cambia, el permiso deja de valer.
    pub hash: String,
    /// Hasta cuándo, en segundos Unix.
    pub until: u64,
    pub signature: String,
}

pub fn allowance_path(approvals_dir: &Path, scenario: &str, environment: &str) -> PathBuf {
    approvals_dir.join(format!("{scenario}.{environment}.allow.json"))
}

/// Firma un permiso para `approval` hasta `until`. Quien llama pidió la confirmación humana.
pub fn allow(approval: &Approval, until: u64, keys: &dyn KeyStore) -> Result<Allowance, String> {
    let key = keys.get_or_create()?;
    let mut allowance = Allowance {
        format: FORMAT,
        scenario: approval.scenario.clone(),
        environment: approval.environment.clone(),
        hash: approval.hash.clone(),
        until,
        signature: String::new(),
    };
    allowance.signature = to_hex(&mac(&key, &allowance_message(&allowance)));
    Ok(allowance)
}

/// Si `allowance` deja correr `approval` (ya verificada contra el plan actual) en `now`.
pub fn allows(allowance: &Allowance, approval: &Approval, now: u64, keys: &dyn KeyStore) -> bool {
    let Ok(Some(key)) = keys.get() else {
        return false;
    };
    let Some(signature) = from_hex(&allowance.signature) else {
        return false;
    };
    let mut check = <Hmac<Sha256> as KeyInit>::new_from_slice(&key)
        .expect("HMAC acepta cualquier largo de clave");
    check.update(&allowance_message(allowance));
    check.verify_slice(&signature).is_ok()
        && allowance.format == FORMAT
        && allowance.scenario == approval.scenario
        && allowance.environment == approval.environment
        && allowance.hash == approval.hash
        && now < allowance.until
}

fn allowance_message(a: &Allowance) -> Vec<u8> {
    format!(
        "crabfall-allowance/{}\n{}\n{}\n{}\n{}",
        a.format, a.scenario, a.environment, a.hash, a.until
    )
    .into_bytes()
}

fn signed_message(a: &Approval) -> Vec<u8> {
    format!(
        "crabfall-approval/{}\n{}\n{}\n{}\n{}\n{}",
        a.format, a.scenario, a.environment, a.hash, a.approved_at, a.approved_by
    )
    .into_bytes()
}

fn sign(key: &[u8], approval: &Approval) -> Vec<u8> {
    mac(key, &signed_message(approval))
}

fn mac(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .expect("HMAC acepta cualquier largo de clave");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Usa el keyring real con un servicio aparte y lo borra al terminar.
    /// `cargo test -p crabfall-core -- --ignored os_keyring`
    #[test]
    #[ignore = "toca el keyring del sistema"]
    fn os_keyring_roundtrip() {
        let entry = OsKeyring::entry_for("crabfall-test").unwrap();
        let _ = entry.delete_credential();
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));

        let key = to_hex(&[7u8; 32]);
        entry.set_password(&key).unwrap();
        assert_eq!(
            from_hex(&entry.get_password().unwrap()),
            Some(vec![7u8; 32])
        );
        entry.delete_credential().unwrap();
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(from_hex(&to_hex(&[0, 15, 255])), Some(vec![0, 15, 255]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }
}
