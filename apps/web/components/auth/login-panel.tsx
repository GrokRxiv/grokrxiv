"use client";

import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";
import { createSupabaseBrowserClient } from "@/lib/supabase/client";
import {
  SUPABASE_ANON_KEY,
  SUPABASE_BROWSER_URL,
  isSupabaseBrowserConfigured,
} from "@/lib/env-public";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

type AuthStatus =
  | { state: "checking"; message: string }
  | { state: "ready"; emailEnabled: boolean; githubEnabled: boolean }
  | { state: "unconfigured"; message: string }
  | { state: "unreachable"; message: string };

type AuthSettings = {
  external?: {
    email?: boolean;
    github?: boolean;
  };
};

export function LoginPanel({
  nextPath,
  initialMessage = null,
}: {
  nextPath: string;
  initialMessage?: string | null;
}) {
  const supabase = useMemo(() => createSupabaseBrowserClient(), []);
  const [email, setEmail] = useState("");
  const [message, setMessage] = useState<string | null>(initialMessage);
  const [busy, setBusy] = useState(false);
  const [authStatus, setAuthStatus] = useState<AuthStatus>({
    state: "checking",
    message: "Checking sign-in...",
  });

  useEffect(() => {
    let cancelled = false;

    async function loadAuthSettings() {
      if (!isSupabaseBrowserConfigured()) {
        setAuthStatus({
          state: "unconfigured",
          message: "Sign-in is not available right now. Please try again later.",
        });
        return;
      }

      try {
        const response = await fetch(
          `${SUPABASE_BROWSER_URL.replace(/\/$/, "")}/auth/v1/settings`,
          {
            cache: "no-store",
            headers: { apikey: SUPABASE_ANON_KEY },
          },
        );
        if (!response.ok) {
          throw new Error("Sign-in service is unavailable.");
        }
        const settings = (await response.json()) as AuthSettings;
        if (cancelled) return;
        setAuthStatus({
          state: "ready",
          emailEnabled: settings.external?.email ?? true,
          githubEnabled: settings.external?.github ?? false,
        });
      } catch (error) {
        if (cancelled) return;
        setAuthStatus({
          state: "unreachable",
          message: formatAuthError(error),
        });
      }
    }

    void loadAuthSettings();
    return () => {
      cancelled = true;
    };
  }, []);

  const callbackUrl = () => {
    const next = encodeURIComponent(nextPath);
    return `${window.location.origin}/auth/callback?next=${next}`;
  };

  async function signInWithGithub() {
    if (authStatus.state !== "ready" || !authStatus.githubEnabled) {
      setMessage("GitHub sign-in is not available yet. Use email for now.");
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const { error } = await supabase.auth.signInWithOAuth({
        provider: "github",
        options: { redirectTo: callbackUrl() },
      });
      if (error) {
        setMessage(error.message);
        setBusy(false);
      }
    } catch (error) {
      setMessage(formatAuthError(error));
      setBusy(false);
    }
  }

  async function signInWithEmail(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (authStatus.state !== "ready" || !authStatus.emailEnabled) {
      setMessage("Email sign-in is not available right now.");
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const { error } = await supabase.auth.signInWithOtp({
        email,
        options: { emailRedirectTo: callbackUrl() },
      });
      setMessage(
        error
          ? error.message
          : "Check your email for the login link.",
      );
    } catch (error) {
      setMessage(formatAuthError(error));
    } finally {
      setBusy(false);
    }
  }

  const authReady = authStatus.state === "ready";
  const emailEnabled = authReady && authStatus.emailEnabled;
  const githubEnabled = authReady && authStatus.githubEnabled;

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
        {authStatus.state !== "ready" ? (
          <p className="rounded-md border border-amber-600 bg-amber-950/20 px-3 py-2 text-sm text-amber-100">
            {authStatus.message}
          </p>
        ) : !authStatus.githubEnabled ? (
          <p className="rounded-md border border-[color:var(--color-border)] px-3 py-2 text-sm text-[color:var(--color-muted-foreground)]">
            GitHub sign-in is not available yet. Use email for now.
          </p>
        ) : null}
        <Button
          type="button"
          onClick={signInWithGithub}
          disabled={busy || !githubEnabled}
        >
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
          <Button
            type="submit"
            variant="outline"
            disabled={busy || !emailEnabled}
          >
            Send magic link
          </Button>
        </form>
        {message ? (
          <p
            className="text-sm text-[color:var(--color-muted-foreground)]"
            aria-live="polite"
          >
            {message}
          </p>
        ) : null}
      </CardContent>
    </Card>
  );
}

function formatAuthError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (message.toLowerCase().includes("failed to fetch")) {
    return "Sign-in service is unavailable from this browser. Please try again later.";
  }
  return message;
}
