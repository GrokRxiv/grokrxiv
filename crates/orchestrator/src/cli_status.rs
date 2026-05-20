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

/// Short operator-facing status marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMark {
    /// Work is currently running.
    Run,
    /// Work completed successfully.
    Ok,
    /// Work completed with a non-blocking warning.
    Warn,
    /// Work failed or produced blocking output.
    Fail,
}

impl StatusMark {
    /// Return the compact display label used in CLI status output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "[RUN]",
            Self::Ok => "[OK]",
            Self::Warn => "[WARN]",
            Self::Fail => "[FAIL]",
        }
    }
}

/// Render the compact header for a foreground command.
pub fn render_header(command: &str, subject: &str, pairs: &[(&str, &str)]) -> Vec<String> {
    let mut lines = vec![format!("GrokRxiv {command} {subject}")];
    if !pairs.is_empty() {
        lines.push(
            pairs
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    lines.push(String::new());
    lines
}

/// Render a top-level stage line.
pub fn render_stage_line(
    index: usize,
    total: usize,
    stage: &str,
    mark: StatusMark,
    detail: &str,
) -> String {
    if detail.is_empty() {
        format!("[{index}/{total}] {stage:<12} {}", mark.label())
    } else {
        format!("[{index}/{total}] {stage:<12} {} {detail}", mark.label())
    }
}

/// Render a nested detail line under a stage.
pub fn render_detail_line(label: &str, mark: StatusMark, detail: &str) -> String {
    if detail.is_empty() {
        format!("      {label:<24} {}", mark.label())
    } else {
        format!("      {label:<24} {} {detail}", mark.label())
    }
}

/// Emit a compact command header to stderr when status output is enabled.
pub fn emit_header(command: &str, subject: &str, pairs: &[(&str, &str)]) {
    if enabled() {
        for line in render_header(command, subject, pairs) {
            eprintln!("{line}");
        }
    }
}

/// Emit a top-level stage line to stderr when status output is enabled.
pub fn emit_stage(index: usize, total: usize, stage: &str, mark: StatusMark, detail: &str) {
    if enabled() {
        eprintln!("{}", render_stage_line(index, total, stage, mark, detail));
    }
}

/// Emit a nested detail line to stderr when status output is enabled.
pub fn emit_detail(label: &str, mark: StatusMark, detail: &str) {
    if enabled() {
        eprintln!("{}", render_detail_line(label, mark, detail));
    }
}

/// Emit a single status line to stderr when status output is enabled.
pub fn emit(message: impl AsRef<str>) {
    if enabled() {
        eprintln!("{}", message.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_stage_lines_are_professional_and_copy_paste_safe() {
        let line = render_stage_line(1, 6, "Fetch", StatusMark::Ok, "arXiv source and metadata");

        assert_eq!(line, "[1/6] Fetch        [OK] arXiv source and metadata");
        assert!(!line.starts_with("status:"));
        assert!(!line.starts_with('{'));
    }

    #[test]
    fn detail_lines_align_under_review_dag() {
        let line = render_detail_line("technical correctness", StatusMark::Warn, "");

        assert_eq!(line, "      technical correctness    [WARN]");
        assert!(!line.starts_with("status:"));
    }

    #[test]
    fn header_summarizes_runtime_without_debug_noise() {
        let lines = render_header(
            "review",
            "2602.17480",
            &[
                ("runner", "cli"),
                ("extractor", "cli"),
                ("cache", "off"),
                ("provider_api", "disabled"),
            ],
        );

        assert_eq!(
            lines,
            vec![
                "GrokRxiv review 2602.17480".to_string(),
                "runner=cli extractor=cli cache=off provider_api=disabled".to_string(),
                "".to_string(),
            ]
        );
    }
}
