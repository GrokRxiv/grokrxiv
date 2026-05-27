# arXiv metadata — prefer `/abs/` HTML page over the API (2026-05-15)

## What

`fetch_metadata()` in `crates/ingest/src/arxiv.rs` now tries `https://arxiv.org/abs/<id>` first (parsing citation_* `<meta>` tags out of the HTML) and falls back to `https://export.arxiv.org/api/query?id_list=<id>` only if the abs path fails. Two new functions: `fetch_metadata_from_abs()` and `parse_abs_html()`.

## Why

During RPT1 parallel ingest, the `export.arxiv.org/api/query` endpoint started returning **HTTP 429 "Rate exceeded"** with no Retry-After hint. The 62-second tarpit + 4-attempt backoff in `rate_limited_get` (Track A4 of this same pass) was not enough — arXiv's per-IP API quota is much stricter than the abs-page quota.

Concrete failures observed:

```
{"level":"WARN","fields":{"message":"arxiv transient failure; backing off","url":"https://export.arxiv.org/api/query?id_list=2605.00403","attempt":4}}
{"level":"ERROR","fields":{"message":"command failed","err":"ingest: fetch arxiv metadata for 2605.00403"}}
```

Whereas a direct `curl` to `https://arxiv.org/abs/2605.00403` returned HTTP 200 in under 0.5s during the same window. The abs page is on a different Fastly pool with a much more generous rate limit. The metadata we need (title, authors, abstract, category, pdf_url, submitted_date, DOI) is all there in `<meta name="citation_*">` tags.

## How

| Change | Location |
|---|---|
| `ARXIV_ABS` constant | `crates/ingest/src/arxiv.rs` |
| `fetch_metadata_from_abs(arxiv_id)` — HTTP GET + parse | `arxiv.rs` |
| `parse_abs_html(arxiv_id, html)` — regex-scrape citation_* meta tags + `<span class="primary-subject">` | `arxiv.rs` |
| `fetch_metadata` rewritten to try abs first, fall back to API | `arxiv.rs` |
| Helper functions: `scrape_meta`, `scrape_meta_all`, `scrape_primary_subject`, `decode_html_entities`, `normalize_author_name` (last-comma-first → first-space-last) | `arxiv.rs` |

The API path is preserved as a fallback so `parse_atom` and existing fixtures stay valid.

## Risk

| Risk | Mitigation |
|---|---|
| arXiv changes the abs-page HTML structure | The fallback to the API kicks in automatically (`fetch_metadata` tries abs first, then API). Worst case we revert to the pre-FP6.5 behavior. |
| `<meta name="citation_author">` parsing misses unusual name formats | `normalize_author_name` handles last-comma-first; other formats pass through. Verifier ladder + meta_reviewer would surface obviously broken author lists. |
| Categories: we extract only the primary subject via `<span class="primary-subject">`. Secondary subjects on the abs page have a different markup. | Acceptable — primary is what every downstream needs. Easy follow-up. |
| HTML entity decoding is hand-rolled (no `htmlentity` crate) | Covers the 8 most common entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&#34;`, `&#39;`, `&apos;`, `&nbsp;`). Anything beyond stays as literal. |

## Reversal

```sh
git checkout HEAD~1 -- crates/ingest/src/arxiv.rs
```

The `parse_atom` function and `ARXIV_API` constant remain — no breaking change downstream.

## Verification

```sh
cargo build -p grokrxiv-ingest        # clean
cargo test -p grokrxiv-ingest         # 3 integration + 5 unit tests pass
cargo run --quiet -- ingest 2605.00403 2605.13993 2605.15132
# All three succeed in parallel; logs show "ingest source=tex" for each.
```

Measured outcome: 3 papers ingested in 76s wall (≈3× speedup vs serial), 18/18 agents `verifier_status=pass` (RPT1 review_ids `c5155ecf-…`, `72aebae7-…`, `2d15dcff-…`).
