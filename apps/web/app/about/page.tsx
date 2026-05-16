import type { Metadata } from "next";
import Link from "next/link";

export const metadata: Metadata = {
  title: "About & methodology",
  description:
    "How GrokRxiv generates verifier-gated AI peer reviews and how they are moderated before publication.",
};

const STAGES = [
  {
    n: "01",
    name: "Ingest",
    body: "The arXiv puller fetches metadata, PDF, and LaTeX source under a single-connection / 3 s rate gate. Text and figures are normalized into a typed paper artifact.",
  },
  {
    n: "02",
    name: "Review DAG",
    body: "Six specialist reviewers run in parallel — summary, technical correctness, novelty, reproducibility, citation, and meta-reviewer. Every agent input and output is a JSON-schema artifact.",
  },
  {
    n: "03",
    name: "Verifier ladder",
    body: "JSON-schema validation, citation existence checks (Crossref + Semantic Scholar), tone classifier, HTML/LaTeX compile checks, and cross-agent consistency. Failed artifacts trigger retries with corrective prompts.",
  },
  {
    n: "04",
    name: "Private artifacts",
    body: "The pipeline writes private artifacts to Supabase. Nothing is public yet — the first surface is internal moderation, not a PR.",
  },
  {
    n: "05",
    name: "Human moderation",
    body: "A moderator reviews the bundle. On approval, the publisher opens a PR to github.com/GrokRxiv/reviews. Reviews become publicly visible only after a human moderator merges the corresponding PR on GitHub. This is the human-moderation gate: approving a review opens the PR but does NOT publish it. The merge webhook flips the review to published, revalidates the Vercel page, and unlocks outreach drafts.",
  },
];

export default function AboutPage() {
  return (
    <article className="mx-auto flex max-w-3xl flex-col gap-12 py-12">
      <header className="flex flex-col gap-4">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          About &middot; Methodology
        </p>
        <h1 className="text-balance text-4xl font-bold tracking-tight md:text-5xl">
          How GrokRxiv works.
        </h1>
        <p className="max-w-2xl text-balance text-lg text-[color:var(--color-muted-foreground)]">
          GrokRxiv produces structured, verifier-gated AI peer reviews of arXiv
          papers.
        </p>
      </header>

      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Pipeline
        </h2>
        <ol className="flex flex-col gap-0 divide-y divide-[color:var(--color-border)] rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)]">
          {STAGES.map((s) => (
            <li
              key={s.n}
              className="flex flex-wrap items-start gap-3 px-4 py-4 sm:flex-nowrap sm:gap-4 sm:px-5"
            >
              <span className="flex-shrink-0 font-mono text-xs text-[color:var(--color-muted-foreground)]">
                {s.n}
              </span>
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                <span className="font-semibold">{s.name}</span>
                <span className="break-words text-sm text-[color:var(--color-muted-foreground)]">
                  {s.body}
                </span>
              </div>
            </li>
          ))}
        </ol>
      </section>

      <section className="grid gap-6 md:grid-cols-2">
        <div className="rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-5">
          <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
            Providers
          </h2>
          <p className="mt-3 text-sm">
            Multi-provider from day one: <strong>Claude</strong> (Anthropic),
            {" "}<strong>Gemini</strong> (Google), <strong>OpenAI</strong>{" "}
            API models, and open-source models via <strong>vLLM</strong> against
            an external endpoint. Provider selection is per-agent in YAML — swap
            models without redeploying.
          </p>
        </div>
        <div className="rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-5">
          <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
            Moderation
          </h2>
          <p className="mt-3 text-sm">
            Every published review has been merged by a human moderator. The
            queue lives in Supabase; the GitHub PR is the audit trail, not the
            first public surface. Corrections re-issue with status{" "}
            <code className="font-mono text-xs">corrected</code>; withdrawn
            reviews are hidden from anon readers and the public JSON API.
          </p>
        </div>
      </section>

      <section className="flex flex-col gap-2 rounded-lg border border-dashed border-[color:var(--color-border)] p-5">
        <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Author disputes / contact
        </h2>
        <p className="text-sm">
          If you are an author of a paper we reviewed and want to dispute,
          request a correction, or ask for a withdrawal,{" "}
          <Link
            href="mailto:disputes@grokrxiv.org"
            className="underline underline-offset-4"
          >
            disputes@grokrxiv.org
          </Link>
          .
        </p>
      </section>
    </article>
  );
}
