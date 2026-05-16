import type { Metadata } from "next";
import Link from "next/link";

export const metadata: Metadata = {
  title: "Legal & disclaimers — GrokRxiv",
  description:
    "Legal disclaimers and arXiv relationship statement for GrokRxiv: AI-generated peer reviews, not an official venue, not endorsed by arXiv.",
};

export default function LegalPage() {
  return (
    <article className="mx-auto flex max-w-3xl flex-col gap-10 py-12">
      <header className="flex flex-col gap-3">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Legal &middot; Disclaimers
        </p>
        <h1 className="text-balance text-4xl font-bold tracking-tight md:text-5xl">
          Legal
        </h1>
      </header>

      <section className="flex flex-col gap-3 rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-6">
        <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Disclaimer
        </h2>
        <p className="text-base">
          <strong>GrokRxiv reviews are AI-generated.</strong> They are not the
          result of formal academic peer review: no editor, no journal, and no
          human reviewer signs off on them. The reviews are produced by large
          language models running under a typed verifier ladder and are
          published only after human moderation, but the underlying judgments
          come from machine learning systems.
        </p>
        <p className="text-base">
          <strong>GrokRxiv is not endorsed by arXiv.</strong> We are not
          affiliated with, sponsored by, or partnered with{" "}
          <Link
            href="https://arxiv.org"
            className="break-words underline underline-offset-4"
          >
            arXiv.org
          </Link>
          {" "}or Cornell University. We link to arXiv as the canonical source
          of the papers we review, and we do not redistribute arXiv PDFs or
          LaTeX source.
        </p>
        <p className="text-base">
          <strong>Reviews are advisory only.</strong> A GrokRxiv review is not
          a substitute for journal peer review or any editorial process. It
          should not be cited as a peer-reviewed assessment of a paper.
        </p>
      </section>

      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Author rights
        </h2>
        <p className="text-sm text-[color:var(--color-muted-foreground)]">
          If you are an author of a paper we have reviewed and want to dispute
          a finding, request a correction, or ask for the review to be
          withdrawn, email{" "}
          <Link
            href="mailto:disputes@grokrxiv.org"
            className="break-all underline underline-offset-4"
          >
            disputes@grokrxiv.org
          </Link>
          . Corrections are issued in-place with status{" "}
          <code className="font-mono text-xs">corrected</code>; withdrawn
          reviews are hidden from anonymous readers and the public JSON API.
        </p>
      </section>

      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          arXiv compliance
        </h2>
        <p className="text-sm text-[color:var(--color-muted-foreground)]">
          The ingest pipeline uses a single connection to{" "}
          <code className="font-mono text-xs">export.arxiv.org</code> with at
          least three seconds between requests, identifies itself with a{" "}
          <code className="font-mono text-xs">User-Agent</code> containing a
          contact email, and stores arXiv PDF and LaTeX source only in a
          private bucket strictly for re-processing. Every review links back
          to{" "}
          <code className="break-all font-mono text-xs">
            https://arxiv.org/abs/&lt;id&gt;
          </code>
          {" "}for the source paper.
        </p>
      </section>
    </article>
  );
}
