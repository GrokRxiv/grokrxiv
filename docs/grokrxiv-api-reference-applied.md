# `grokrxiv` HTTP API reference — applied

GrokRxiv's public HTTP API lives behind the Next.js web tier at `/api/v1/*`.
The public contract is read-only. Review creation, moderation, publishing, and
rerendering remain CLI/operator workflows.

## Authentication

Read endpoints (`GET /api/v1/reviews`, etc.) are unauthenticated and talk to
Supabase directly. They only return rows whose review is public visibility and
in a public status.

## Endpoints

- `GET /api/v1/reviews?status=published&page=1&limit=20&field=cs.AI`
- `GET /api/v1/reviews/:id`  (also accepts an arXiv id)
- `GET /api/v1/papers/:arxiv`

`/api/v1/reviews` accepts `status=pr_open|published|corrected|rejected`; the
default is `published`.

## Error shapes

| Status | Body                                                            | When |
|--------|-----------------------------------------------------------------|------|
| 400    | `{"error":"bad_query","detail":{...}}` | query validation failure |
| 404    | `{"error":"not_found"}` | no matching public row |
| 500    | `{"error":"supabase_query_failed","detail":"..."}` | Supabase query failure |
| 503    | `{"error":"not_configured"}` | web tier is missing Supabase config |

## Operator notes

- Use first-party CLI commands such as `grokrxiv review`, `grokrxiv approve`,
  and `grokrxiv batch run` for write workflows.
- The orchestrator's `/internal/v1/*` routes are private-network plumbing. Do
  not expose them publicly.
