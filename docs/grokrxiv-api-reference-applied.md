# `grokrxiv` HTTP API reference — applied

GrokRxiv's public HTTP API lives behind the Next.js web tier at `/api/v1/*`.
The web tier forwards write requests to the orchestrator's internal HTTP
API at `/internal/v1/*` after enforcing bearer authentication.

```
client → POST /api/v1/<action>          (Bearer GROKRXIV_SERVICE_TOKEN)
       ↓
       → POST <ORCHESTRATOR_INTERNAL_URL>/internal/v1/<action>
       ↓
       → orchestrator runtime
```

## Authentication

Every `/api/v1/*` write endpoint requires:

```
Authorization: Bearer ${GROKRXIV_SERVICE_TOKEN}
```

The token is configured server-side via `GROKRXIV_SERVICE_TOKEN`. When the
env var is unset the endpoint returns `503 service_unconfigured`. When the
token is set but the request bearer doesn't match it returns `401 unauthorized`.

Read endpoints (`GET /api/v1/reviews`, etc.) are unauthenticated and talk to
Supabase directly.

## Endpoints

### `POST /api/v1/review`

Enqueue an end-to-end review run.

```sh
curl -X POST https://grokrxiv.org/api/v1/review \
     -H "Authorization: Bearer $GROKRXIV_SERVICE_TOKEN" \
     -H "content-type: application/json" \
     -d '{"source": "2605.12484"}'
```

Body:
```json
{
  "source": "2605.12484",                 // arxiv id | URL | path | "-" | "@file"
  "type":   "arxiv",                       // optional; "arxiv" | "pdf" | "tex" | "mixed"
  "mode":   "review_only",                 // optional
  "runner": "api"                          // optional default-runner override
}
```

Response (202): `{"job_id": "...", "status": "queued", "note": "..."}`.

> RPT2: this is a *stub* enqueue. The full async dispatch to the supervisor
> is a Track I follow-up. Use `grokrxiv review` for the synchronous path.

### `POST /api/v1/reviews/:id/approve`

```sh
curl -X POST https://grokrxiv.org/api/v1/reviews/$REVIEW_ID/approve \
     -H "Authorization: Bearer $GROKRXIV_SERVICE_TOKEN"
```

### `POST /api/v1/reviews/:id/reject`

```json
{"reason": "Insufficient experimental validation."}
```

### `POST /api/v1/reviews/:id/render`

```json
{"format": "html"}   // html | md | tex | pdf | zip
```

### `POST /api/v1/reviews/:id/apply-revisions`

Body forwarded verbatim — schema TBD by Track F.

### `POST /api/v1/reviews/:id/verify`

Re-run the verifier ladder against a persisted review. No body.

### `GET /api/v1/doctor`

Proxies the orchestrator's preflight summary.

```sh
curl -H "Authorization: Bearer $GROKRXIV_SERVICE_TOKEN" \
     https://grokrxiv.org/api/v1/doctor
```

### `GET /api/v1/search`

Reserved for the cross-corpus search endpoint. Currently returns
`501 not_implemented` (Track I follow-up).

## Read endpoints (unauthenticated)

- `GET /api/v1/reviews?status=published&page=1&limit=20&field=cs.AI`
- `GET /api/v1/reviews/:id`  (also accepts an arXiv id)
- `GET /api/v1/papers/:arxiv`

These were not modified by Track I.

## Error shapes

| Status | Body                                                            | When |
|--------|-----------------------------------------------------------------|------|
| 400    | `{"error":"bad_body","detail":{...}}`                           | zod validation fail |
| 401    | `{"error":"unauthorized"}`                                       | bearer mismatch |
| 501    | `{"error":"not_implemented","detail":"..."}`                    | search route |
| 502    | `{"error":"orchestrator_unreachable","detail":"..."}`           | orchestrator down |
| 503    | `{"error":"service_unconfigured","detail":"GROKRXIV_SERVICE_TOKEN is unset"}` | env not set |

## Operator notes

- The internal `/internal/v1/*` routes have no auth. They live on the
  orchestrator's private network. Don't expose them publicly.
- Set `GROKRXIV_SERVICE_TOKEN` on **both** the web and orchestrator deployments
  if you want callers to talk via the proxy.
- For local dev: `export GROKRXIV_SERVICE_TOKEN=dev-token` in `.env.local`
  before `just dev`.
