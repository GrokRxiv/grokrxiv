//! Lightweight stderr progress output for foreground CLI runs.
//!
//! The review pipeline still uses structured tracing for diagnostics. This
//! module is deliberately narrower: short operator-facing status lines that
//! never write to stdout, so `--json` remains machine-readable.

/// Environment variable used to enable CLI status output in deep pipeline code.
pub const ENV: &str = "GROKRXIV_STATUS";

/// Configure whether status output is enabled for this process.
pub fn set_enabled(enabled: bool) {
    if enabled {
        std::env::set_var(ENV, "1");
    } else {
        std::env::remove_var(ENV);
    }
}

/// Return whether status output is enabled for this process.
pub fn enabled() -> bool {
    matches!(std::env::var(ENV).as_deref(), Ok("1"))
}

/// Emit a single status line to stderr when status output is enabled.
pub fn emit(message: impl AsRef<str>) {
    if enabled() {
        eprintln!("status: {}", message.as_ref());
    }
}
