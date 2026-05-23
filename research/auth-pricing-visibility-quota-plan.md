# GrokRxiv Auth, Pricing, Visibility, and Quota Plan

## Summary

GrokRxiv should keep the public corpus healthy while protecting operator compute
and provider spend:

- Anonymous upload remains a sample review only.
- Free logged-in full reviews are capped and public.
- Paid accounts add quota and optional private reviews.
- Provider API billing is never an automatic fallback from CLI/local execution.
- Private reviews are dashboard-first; a private GitHub archive can exist later,
  but should not be the primary customer experience.

## Product Tiers

| Tier | Full reviews | Visibility | Compute | Notes |
|---|---:|---|---|---|
| Anonymous | 0 | sample only | `sample_preview` | PDF sample preview only, rate-limited, no saved full review |
| Free account | 3 lifetime | public only | `public_free` | User consents that approved/rejected output may become public |
| Supporter | 10 public/month, 2 private/month | public or private | `paid_standard` / `paid_private` | Low monthly price target, higher queue priority |
| Researcher | 30 public/month, 10 private/month | public or private | `paid_standard` / `paid_private` | Optional API rerun credits |
| Credit packs | extra credits | public or private | profile-specific | Public credits should cost less than private credits |

Pricing should stay credit-based and inexpensive. Private reviews cost more
because they consume compute without contributing to the public corpus.

## Compute Profiles

- `sample_preview`
  - deterministic PDF normalization plus one small sample meta-review;
  - no six-agent DAG;
  - strict timeout and token cap;
  - never creates a full review or quota event.
- `public_free`
  - `GROKRXIV_RUNNER=cli`;
  - `GROKRXIV_EXTRACTOR=cli`;
  - `GROKRXIV_ALLOW_PROVIDER_API=0`;
  - deterministic/local extraction default;
  - no API fallback.
- `paid_standard`
  - same local/CLI default as free;
  - higher queue priority;
  - higher monthly quota.
- `paid_private`
  - same local/CLI default;
  - private visibility;
  - user dashboard and admin console only by default;
  - optional private repo archival later.
- `premium_api`
  - explicit opt-in only;
  - requires a per-job cost cap;
  - uses `GROKRXIV_MAX_COST_USD`;
  - bills API credits separately.

## Cost Guards

Global controls:

- `GROKRXIV_DAILY_FULL_REVIEW_LIMIT`
- `GROKRXIV_MONTHLY_FULL_REVIEW_LIMIT`
- `GROKRXIV_MAX_COST_USD`
- `GROKRXIV_ALLOW_PROVIDER_API=0` by default

Per-user controls:

- lifetime free public review quota;
- monthly public review quota;
- monthly private review quota;
- admin override.

Per-job controls:

- `compute_profile`;
- `visibility`;
- maximum runtime;
- maximum retries;
- maximum cost;
- no provider API fallback unless the profile allows it.

If CLI subscription quota is exhausted, jobs should pause or queue with a clear
capacity message. They must not silently spend provider API credits.

## Visibility Model

Add `visibility` to full-review data:

- `public`: visible through public review pages, public API, sitemap, RSS/search,
  and public GitHub repo after the normal moderation/PR lifecycle.
- `private`: visible to the submitting user and admins/moderators only.

Public statuses remain:

- `pr_open`
- `published`
- `corrected`
- `rejected`

Public site queries must require both:

- `visibility = 'public'`;
- `status in ('pr_open','published','corrected','rejected')`.

Private reviews can reuse internal statuses, but Supabase RLS must keep them out
of anonymous public surfaces.

## Repository Strategy

- `GrokRxiv/grokrxiv-reviews` remains the public canonical review repo.
- `GrokRxiv/grokrxiv-private-reviews` can be added as an optional archive target
  for paid private reviews.
- Private GitHub access is not the customer UI. Private reviews should primarily
  live in the logged-in dashboard backed by Supabase RLS.

Approval behavior:

- Public approval opens a PR to `GrokRxiv/grokrxiv-reviews`.
- Human merge plus webhook marks the review published and revalidates the site.
- Private approval releases the review in dashboard; optional archive PR targets
  the private repo, not the public site.

## Data Model

Core auth and quota tables:

- `profiles`
  - `user_id`, display fields, `billing_tier`, `review_limit_override`.
- `user_roles`
  - `user_id`, role `user|moderator|admin`.
- `billing_plans`
  - public/private quotas, `allow_private`, `allow_api_addon`, queue priority.
- `user_billing`
  - current plan and period.
- `review_credits`
  - public, private, and API credit balances.
- `submissions`
  - user, source, visibility, compute profile, state, review id, quota flag,
    cost cap.
- `quota_events`
  - accepted, blocked, charged, refunded, overridden decisions.
- `quota_snapshots`
  - future subscription quota observer output.

Review tables:

- `reviews.visibility text not null default 'public'`
- `reviews.submitted_by uuid null`
- `moderation_queue.moderator_user_id uuid null`

## User Workflow

Anonymous:

- upload PDF from homepage;
- receive sample preview only;
- no full DAG;
- no quota event;
- no public page.

Free logged-in user:

- dashboard shows `used / 3` full reviews;
- can start a full public review only after consent that the output may become
  public if approved/rejected;
- fourth full review is blocked before extraction starts;
- sample preview still works after quota is exhausted.

Paid user:

- can choose public or private full review;
- sees separate public/private quota counts;
- private reviews stay out of public pages, public API, sitemap, RSS, and public
  GitHub URLs.

Admin:

- sees public/private/all moderation filters;
- can grant extra reviews or unlimited internal quota;
- can approve, reject, request changes, and force approve;
- approval UI must clearly show the target destination.

## Web Surface Rules

- Homepage upload copy must say "sample review".
- Public `/reviews`, `/papers/:arxiv`, sitemap, and `/api/v1/*` list only public
  visibility rows in public statuses.
- Private reviews appear only in the submitting user's dashboard and admin
  console.
- Public rejected reviews can be visible with moderator rationale.
- Private rejected reviews remain private.

## Automation Test Plan

Use the Chrome/computer-use plugin for the browser paths and direct API/SQL
checks for state assertions:

- Anonymous sample upload succeeds and never creates a full review/quota event.
- Free user can run 3 public full reviews and is blocked before pipeline work on
  the 4th.
- Free user cannot select private visibility.
- Paid user can select private visibility.
- Public review appears in `/reviews`, `/papers/:arxiv`, sitemap, and public API
  when a public status is reached.
- Private review never appears in public surfaces.
- Private review is visible in the submitting user dashboard and admin console.
- Admin override grants extra quota.
- System-failed extraction is refunded or not charged.
- Public approve targets the public repo.
- Private approve targets the private repo/archive or dashboard-only release.
- API-backed job requires `premium_api` and a positive cost cap.

## Future Work: Subscription Quota Observer

This is not required for the initial auth/pricing launch. Goal: avoid starting
paid/private/full review jobs when Codex, Claude, or Gemini subscription capacity
is low.

Candidate approach:

- Investigate CodexBar as a reference implementation.
- Do not rely directly on the macOS menu bar app for server automation because
  Keychain prompts and GUI permissions are brittle.
- Preferred path:
  - read the CodexBar source;
  - extract or reimplement only the provider quota readers needed by GrokRxiv;
  - run them as a headless quota probe service;
  - store snapshots in `quota_snapshots`.

Product use:

- quota observer informs scheduling only;
- it must never unlock provider API fallback;
- if subscription quota is low, pause free jobs, allow admin override, prefer
  local models, and show "capacity limited, queued" in the dashboard;
- paid/private jobs still need hard per-job and per-user caps.

Data to track:

- provider: `codex`, `claude`, `gemini`;
- auth mode: `cli_oauth`, `app_server`, `cookie`, `api`;
- quota window used/remaining;
- reset time;
- confidence/source;
- last successful check;
- error text, if unavailable.

References:

- CodexBar site: https://codexbar.app/
- CodexBar GitHub: https://github.com/steipete/CodexBar
