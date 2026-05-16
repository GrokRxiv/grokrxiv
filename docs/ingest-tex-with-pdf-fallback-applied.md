# Ingest enhancement — TeX-source preferred, PDF fallback (2026-05-15)

## What

The `grokrxiv-ingest` crate now fetches the arXiv source bundle (`https://arxiv.org/e-print/<id>`) and uses it as the **primary** input to `PaperExtract`. When source is unavailable or unparseable, ingest falls back to the existing PDF-text extraction path. The PDF is always downloaded regardless of which path produced the extract (it's the human-viewable artifact).

`PaperExtract` gained one new optional field, `source_format: Option<String>` — set to `"tex"` or `"pdf"` to mark provenance.

## Why

Per operator directive (2026-05-15, RPT1 plan): "ALL arXiv papers now need TeX. Review can come from TeX; PDF is for viewing." Empirically, the PDF path loses too much on math-heavy papers — section titles get garbled by `pdf-extract`, inline math collapses, citation bibliographies are byte-soup. The TeX source preserves `\section{}` markers verbatim, `\cite{key}` references are resolvable, and inline math stays as `$E=mc^2$` rather than `Emc2`. Quality on math-ph papers like 2605.00403 should improve dramatically.

## How

| Change | Location |
|---|---|
| New module `tex` | `crates/ingest/src/tex.rs` (new) |
| `parse_bundle(&Bytes) -> Result<TexExtract>` — handles `tar.gz`, plain `tar`, `gz`-only-`.tex`, or raw `.tex` | `crates/ingest/src/tex.rs` |
| Heuristic main-file picker: file with `\documentclass` → `main.tex`/`paper.tex`/`ms.tex` → largest `.tex` | `tex.rs::pick_main` |
| Regex parser: `\title`, `\author`, `\begin{abstract}…\end{abstract}`, `\section`/`\subsection`, `\bibitem` | `tex.rs::parse_tex` |
| One-level `\input{...}`/`\include{...}` resolution | `tex.rs::resolve_inputs` |
| Comment stripping (`%` honoring `\%`) | `tex.rs::strip_comments` |
| Wrapper-command sanitiser (`\textbf{}` etc unwrap to inner text) | `tex.rs::sanitize_inline` |
| Pipeline: try TeX → fall back to PDF | `crates/ingest/src/pipeline.rs` |
| `source_format` field on `PaperExtract` | `crates/schemas/src/lib.rs` |
| Module re-exports | `crates/ingest/src/lib.rs` |

No new Rust dependencies — `tar`, `flate2`, `regex`, `bytes` were already in `Cargo.toml`.

No DB migration needed — `paper_assets` already has both `pdf_path` and `latex_source_path` from the FP1 init migration. The orchestrator does not currently write rows to `paper_assets` for any path (this was true before FP6.5 too), so disk persistence of source is deferred. The `PaperExtract.source_format` field gives us provenance without a DB change.

## Risk

| Risk | Mitigation |
|---|---|
| Complex multi-file projects miss nested `\input` chains beyond depth 2 | The regex parser captures the top-level project; depth-2 covers almost all papers. Verifier ladder flags low-confidence reviews. |
| Unusual macros (custom `\newcommand`, package-specific environments) escape the parser | `source_format` exposes this — meta_reviewer can be tuned to be more skeptical on `tex` extracts that have suspiciously short bibliography or section count. |
| Network failure on `e-print` endpoint | Falls through to PDF path (existing behavior). Logged at INFO. |
| `gz` decoder bombs on a malformed archive | Caught by `try_targz`/`try_tar`; falls through to other unpacking heuristics, then to PDF path. |
| Schema churn for downstream consumers | `source_format` is optional + has serde default, so any older JSON without it deserializes cleanly. |

## Reversal

```sh
# Revert the schema change
git -C /Users/mlong/Documents/Development/grokrxiv checkout HEAD~1 -- crates/schemas/src/lib.rs
# Drop the new ingest module
rm crates/ingest/src/tex.rs
# Revert pipeline.rs + lib.rs
git checkout HEAD~1 -- crates/ingest/src/pipeline.rs crates/ingest/src/lib.rs
```

No data loss — paper_assets rows from before are unchanged; PaperExtract JSON from before deserializes fine because `source_format` is `#[serde(default)]`.

## Verification

```sh
cargo build --workspace                        # clean
cargo test --workspace --lib                   # 15+ tests pass (includes 5 new tex.rs tests)
cargo test -p grokrxiv-ingest                  # ingest crate green
```

End-to-end check (Phase 3 of the RPT1 plan): `cargo run -- ingest 2605.00403` should log `source=tex paper=2605.00403` and produce a `PaperExtract` whose section titles match the actual TeX `\section{}` markers from the paper (which they did NOT under the old PDF-only path).
