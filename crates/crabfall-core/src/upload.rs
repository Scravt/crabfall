//! Archivos de los bodies multipart (ADR-012): se leen una vez, antes de armar el plan, y la
//! aprobación firma su contenido.

use std::{collections::BTreeSet, path::Path};

use bytes::Bytes;
use sha2::{Digest, Sha256};

use crate::scenario::{MultipartPart, Scenario};

/// Tamaño máximo de un archivo: se manda entero en cada request y vive en memoria.
pub const MAX_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Upload {
    /// Ruta tal como está en el escenario.
    pub file: String,
    pub bytes: Bytes,
    /// SHA-256 del contenido: lo firma la aprobación.
    pub sha256: String,
}

impl Upload {
    /// Lee `file`, relativo a `dir` (la carpeta `.crabfall/`).
    pub fn load(dir: &Path, file: &str) -> Result<Self, String> {
        let path = dir.join(file);
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("multipart: no se pudo leer {}: {e}", path.display()))?;
        Self::new(file, bytes)
    }

    pub fn new(file: &str, bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err(format!(
                "multipart: {file} pesa {}; el máximo es {}",
                size(bytes.len()),
                size(MAX_BYTES)
            ));
        }
        Ok(Self {
            file: file.to_string(),
            sha256: Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            bytes: Bytes::from(bytes),
        })
    }

    /// `files/foto.jpg (120 KB)`, para los resúmenes.
    pub fn describe(&self) -> String {
        format!("{} ({})", self.file, size(self.bytes.len()))
    }
}

/// Archivos que usan los bodies multipart del escenario, sin repetir.
pub fn files(s: &Scenario) -> BTreeSet<&str> {
    let refresh = s.vu_refresh.iter().flat_map(|r| &r.steps);
    s.setup
        .iter()
        .chain(&s.vu_setup)
        .chain(refresh)
        .chain(s.flows.values().flat_map(|f| &f.steps))
        .flat_map(|step| step.request.multipart.values())
        .filter_map(|part| match part {
            MultipartPart::File(f) => Some(f.file.as_str()),
            MultipartPart::Text(_) => None,
        })
        .collect()
}

/// Tipo por la extensión; los comunes en APIs. El resto, `application/octet-stream`.
pub fn content_type(file: &str) -> &'static str {
    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("txt") => "text/plain",
        Some("xml") => "application/xml",
        Some("zip") => "application/zip",
        Some("gz") => "application/gzip",
        Some("mp4") => "video/mp4",
        Some("mp3") => "audio/mpeg",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

/// `950 B`, `120 KB`, `2.4 MB`.
pub fn size(bytes: usize) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_type_and_formats_size() {
        assert_eq!(content_type("files/Foto.JPG"), "image/jpeg");
        assert_eq!(content_type("files/datos"), "application/octet-stream");
        assert_eq!(size(900), "900 B");
        assert_eq!(size(120 * 1024), "120 KB");
        assert_eq!(size(2_516_582), "2.4 MB");
    }

    #[test]
    fn rejects_files_over_the_limit() {
        let e = Upload::new("grande.bin", vec![0; MAX_BYTES + 1]).unwrap_err();
        assert!(e.contains("el máximo es 100.0 MB"), "{e}");
        let ok = Upload::new("a.txt", b"hola".to_vec()).unwrap();
        assert_eq!(ok.sha256.len(), 64);
    }
}
