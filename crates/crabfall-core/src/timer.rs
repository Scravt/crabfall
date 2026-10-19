//! Resolución del timer del sistema (ADR-003 §3).

/// En Windows el timer tiene ~15,6 ms de resolución por defecto y todo `sleep`
/// se redondea hacia arriba. Pedimos 1 ms para todo el proceso. En otros sistemas no hace nada.
pub fn high_resolution() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);
    }
}
