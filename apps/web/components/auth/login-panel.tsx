"use client";

import { useMemo, useState } from "react";
import type { FormEvent } from "react";
import { createSupabaseBrowserClient } from "@/lib/supabase/client";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

export function LoginPanel({ nextPath }: { nextPath: string }) {
  const supabase = useMemo(() => createSupabaseBrowserClient(), []);
  const [email, setEmail] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const callbackUrl = () => {
    const next = encodeURIComponent(nextPath);
    return `${window.location.origin}/auth/callback?next=${next}`;
  };

  async function signInWithGithub() {
    setBusy(true);
    setMessage(null);
    const { error } = await supabase.auth.signInWithOAuth({
      provider: "github",
      options: { redirectTo: callbackUrl() },
    });
    if (error) {
      setMessage(error.message);
      setBusy(false);
    }
  }

  async function signInWithEmail(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setMessage(null);
    const { error } = await supabase.auth.signInWithOtp({
      email,
      options: { emailRedirectTo: callbackUrl() },
    });
    setMessage(error ? error.message : "Check your email for the login link.");
    setBusy(false);
  }

  return (
    <Card className="mx-auto w-full max-w-md">
      <CardHeader>
        <CardTitle>Sign in to GrokRxiv</CardTitle>
        <CardDescription>
          Full paper reviews, quotas, and private review access require an
          account. Homepage PDF upload remains a sample preview.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <Button type="button" onClick={signInWithGithub} disabled={busy}>
          Continue with GitHub
        </Button>
        <form onSubmit={signInWithEmail} className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Email</span>
            <input
              type="email"
              required
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              className="rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-background)] px-3 py-2"
              placeholder="you@example.com"
            />
          </label>
          <Button type="submit" variant="outline" disabled={busy}>
            Send magic link
          </Button>
        </form>
        {message ? (
          <p className="text-sm text-[color:var(--color-muted-foreground)]">
            {message}
          </p>
        ) : null}
      </CardContent>
    </Card>
  );
}
