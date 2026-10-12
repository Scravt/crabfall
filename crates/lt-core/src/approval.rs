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
const KEYRING_SERVICE: &str = "lt-loadtest";
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
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| format!("keyring: {e}"))
    }
}

impl KeyStore for OsKeyring {
    fn get(&self) -> Result<Option<Vec<u8>>, String> {
        match Self::entry()?.get_password() {
            Ok(hex_key) => from_hex(&hex_key)
                .map(Some)
                .ok_or("la clave del keyring está dañada".into()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("keyring: {e}")),
        }
    }

    fn get_or_create(&self) -> Result<Vec<u8>, String> {
        if let Some(key) = self.get()? {
            return Ok(key);
        }
        let mut key = vec![0u8; 32];
        getrandom::fill(&mut key).map_err(|e| format!("no se pudo generar la clave: {e}"))?;
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

/// Archivo `.loadtest/approvals/<escenario>.<entorno>.json`.
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
            Self::Missing => write!(f, "no está aprobado"),
            Self::Tampered(why) => write!(f, "la aprobación no es válida: {why}"),
            Self::Changed(_) => write!(f, "cambió desde la última aprobación"),
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
        return Err(Rejection::Tampered(format!(
            "formato {} desconocido",
            approval.format
        )));
    }
    if approval.scenario != plan.scenario || approval.environment != environment {
        return Err(Rejection::Tampered(format!(
            "es de `{}` en `{}`, no de `{}` en `{environment}`",
            approval.scenario, approval.environment, plan.scenario
        )));
    }
    let key = keys
        .get()
        .map_err(Rejection::Tampered)?
        .ok_or_else(|| Rejection::Tampered("no hay clave de firma en este equipo".into()))?;
    let signature = from_hex(&approval.signature)
        .ok_or_else(|| Rejection::Tampered("firma ilegible".into()))?;
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key)
        .expect("HMAC acepta cualquier largo de clave");
    mac.update(&signed_message(approval));
    if mac.verify_slice(&signature).is_err() || fingerprint(&approval.plan) != approval.hash {
        return Err(Rejection::Tampered(
            "la firma no coincide (¿se editó el archivo?)".into(),
        ));
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

fn signed_message(a: &Approval) -> Vec<u8> {
    format!(
        "lt-approval/{}\n{}\n{}\n{}\n{}\n{}",
        a.format, a.scenario, a.environment, a.hash, a.approved_at, a.approved_by
    )
    .into_bytes()
}

fn sign(key: &[u8], approval: &Approval) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .expect("HMAC acepta cualquier largo de clave");
    mac.update(&signed_message(approval));
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
