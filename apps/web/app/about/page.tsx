import type { Metadata } from "next";
import Link from "next/link";

export const metadata: Metadata = {
  title: "About & methodology",
  description:
    "How GrokRxiv creates structured review reports and moderates public reviews before publication.",
};

const STAGES = [
  {
    n: "01",
    name: "Prepare the paper",
    body: "GrokRxiv gathers the paper metadata and prepares the text, equations, references, and figures for review.",
  },
  {
    n: "02",
    name: "Review the work",
    body: "The report covers summary, technical correctness, novelty, reproducibility, citation quality, and an overall recommendation.",
  },
  {
    n: "03",
    name: "Check the report",
    body: "Before moderation, GrokRxiv checks that the report is complete, readable, citation-aware, and suitable for publication.",
  },
  {
    n: "04",
    name: "Moderate",
    body: "A human moderator reviews the report before it becomes public. Moderators can approve, reject, or request changes.",
  },
  {
    n: "05",
    name: "Publish",
    body: "Approved public reviews appear on GrokRxiv and are archived in the public review repository. Private reviews stay visible only to the account owner and moderators.",
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
          GrokRxiv produces structured review reports for arXiv papers, with
          automated checks and human moderation before public publication.
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
            Review coverage
          </h2>
          <p className="mt-3 text-sm">
            Each full report includes a plain-language summary, technical
            correctness review, novelty assessment, reproducibility notes,
            citation review, and final recommendation.
          </p>
        </div>
        <div className="rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-5">
          <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
            Moderation
          </h2>
          <p className="mt-3 text-sm">
            Every published review has passed human moderation. Corrections
            are marked with status{" "}
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
