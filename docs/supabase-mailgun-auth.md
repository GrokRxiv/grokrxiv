# Supabase Auth + Mailgun Magic Links

GrokRxiv uses Supabase Auth for email magic links. The web app calls
`supabase.auth.signInWithOtp(...)`; Supabase generates the one-time token,
builds the callback URL, and sends the email. Do not add EmailJS for login.

## Production SMTP

Configure SMTP in Supabase Auth with Mailgun:

| Field | Value |
|-------|-------|
| Host | `smtp.mailgun.org` |
| Port | `587` |
| User | `postmaster@appmail.magnetonlabs.com` |
| Password | Mailgun SMTP password for the domain |
| Sender email | `no-reply@appmail.magnetonlabs.com` |
| Sender name | `GrokRxiv` |

Use the US Mailgun host for `appmail.magnetonlabs.com`. If the Mailgun key or
SMTP password was pasted into chat or logs, rotate it before production use.

## Required Supabase Auth URLs

Set the deployed site URL and allow the callback route:

- Site URL: deployed GrokRxiv web URL
- Redirect URL: `<site-url>/auth/callback`
- Redirect URL for dashboard login: `<site-url>/auth/callback?next=%2Fdashboard`

Local development already allows `http://localhost:3000/auth/callback` and
`http://127.0.0.1:3000/auth/callback` in `supabase/config.toml`.

## Local Development

Local `supabase start` uses the `[auth.email.smtp]` block in
`supabase/config.toml`, but that block points to the local Supabase
Mailpit/Inbucket container, not Mailgun. Magic-link emails are captured in
Mailpit:

```bash
open http://127.0.0.1:54324
```

The E2E suite reads Mailpit directly and verifies that a magic link lands on
`/dashboard`.

## Do Not Expose Secrets

Mailgun credentials belong only in Supabase Cloud secrets, a self-hosted
Supabase environment, or a local untracked `.env`. Never add them as
`NEXT_PUBLIC_*` variables and never send login emails from browser code.
