# Follow-up — match arXiv pulling/searching patterns from `agustif/llm-arxiv`

Captured 2026-05-15 during RPT1. The operator asked us to look at the patterns in <https://github.com/agustif/llm-arxiv> and make sure we follow them in Rust. This doc enumerates the gap and proposes a small follow-up pass (not part of RPT1's main goal — the goal is real PRs, and we have all 3 papers ingested via the bare-ID happy path).

## Patterns observed in `llm-arxiv` (Python)

The reference uses the Python `arxiv` library (which itself talks to `export.arxiv.org/api/query`). The notable patterns:

1. **Flexible identifier acceptance** — `extract_arxiv_id(arg)` accepts:
   - `https://arxiv.org/abs/<id>[v<n>]`
   - `https://arxiv.org/pdf/<id>[v<n>][.pdf]`
   - Bare modern ID: `<YYMM.NNNNN>[v<n>]`
   - Old-format ID: `<archive>/<7-digit>` (e.g. `math-ph/0506010`)
2. **Search by query** — `arxiv.Search(query="...", sort_by=…, max_results=N)` with three sort orders: Relevance / SubmittedDate / LastUpdatedDate.
3. **Per-paper attributes** — `paper.entry_id`, `paper.title`, `paper.authors[].name`, `paper.published`, `paper.updated`, `paper.primary_category`, `paper.categories[]`, `paper.summary`, `paper.pdf_url`.
4. **PDF download** — `paper.download_pdf(dirpath=...)` to a temp dir.
5. **Standalone CLI** — `llm arxiv <id-or-url>` and `llm arxiv search <query>` as separate subcommands.

## Where we stand today

| Pattern | GrokRxiv state | Gap |
|---|---|---|
| Bare modern ID input | ✓ `cargo run -- ingest 2605.00403` | — |
| URL input (`/abs/...`, `/pdf/...`) | ✗ would fail because we pass the arg verbatim to the metadata fetcher | Add `extract_arxiv_id()` helper in `crates/ingest/src/arxiv.rs` |
| Old-format ID (`math-ph/0506010`) | ✗ untested; the abs-page URL pattern would handle it, but the regex in `tex.rs::sniff_identifiers` only matches modern format | Extend regex in citation sniffer; verify abs-page URL works |
| Search by free-text query | ✗ no command | New `cargo run -- search "<query>" [--sort relevance\|submitted\|updated] [--limit N]` |
| Paper attributes parity | mostly ✓ (we capture title/authors/abstract/category/pdf_url/submitted_date); missing `updated_date`, `entry_id`, full `categories[]` | `ArxivMeta` already has `categories: Vec<String>`; only ever populated to size 1 via abs page; expand the primary-subject regex to scrape secondary subjects too |
| PDF download to disk | ✗ we download in-memory and never persist (paper_assets row not written) | Optional. The publisher attaches `bundle.zip` from the renderer, so the PDF on disk isn't actually needed for the PR flow. Could add `--save-pdf <path>` flag if a future user wants it. |
| Standalone CLI command | ~ we have `cargo run -- ingest <id>...` and the rest of the orchestrator's CLI; no `search` yet | Add `Command::Search { query, sort, limit }` |

## Suggested follow-up pass (out of scope for RPT1)

Estimated 2 hours of work. New crate-level changes only — no Rust deps needed beyond `regex` which we already use.

1. **`extract_arxiv_id(input: &str) -> Option<String>`** in `crates/ingest/src/arxiv.rs`:
   ```rust
   // Modern URL
   r"https?://arxiv\.org/(?:abs|pdf)/(\d{4,}\.\d{4,}(?:v\d+)?)(?:\.pdf)?$"
   // Bare modern ID
   r"^(\d{4,}\.\d{4,}(?:v\d+)?)$"
   // Old-format
   r"^([a-z-]+(?:\.[A-Z]{2})?/\d{7})$"
   ```
   Wire into `ingest_many` so any URL or bare ID works.

2. **`search(query, sort, limit) -> Vec<ArxivMeta>`** in `crates/ingest/src/arxiv.rs`:
   - Calls `https://export.arxiv.org/api/query?search_query=<urlencode>&sortBy=<...>&sortOrder=descending&max_results=<n>`
   - Reuses `parse_atom()` (the API returns an Atom feed for search results too)
   - Backoff path already exists in `rate_limited_get`

3. **`Command::Search { query, sort, limit }`** in `crates/orchestrator/src/cli.rs`:
   - Wired through to `search()` in ingest
   - Print results as a table: `[i] arxiv_id  primary_cat  title (truncated)`
   - Print a copy-paste-ready `cargo run -- ingest <id>` line per result (matches `llm-arxiv`'s "Command:" output)

4. **Tests**: add a captured `search_results_fixture.xml` and a `parses_search_results_fixture` test in `crates/ingest/tests/integration.rs`.

## Decision for RPT1

The three papers we needed for the parallel ingest test (2605.00403, 2605.13993, 2605.15132) are bare IDs and already ingested successfully. This follow-up does not block the goal of "land a real PR end-to-end" and would dilute the RPT1 commit. **Deferring to a separate, focused pass** — likely RPT2 or absorbed into FP7's CLI polish.

## Why this is worth doing

The URL-acceptance change is a 30-minute ergonomics win. The `search` command is a substantive feature that turns the orchestrator from "feed me an ID" into "discover and review", which is closer to a usable tool for human reviewers.
