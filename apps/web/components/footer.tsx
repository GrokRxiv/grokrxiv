import Link from "next/link";

export function Footer() {
  return (
    <footer className="mt-24 border-t border-[color:var(--color-border)] bg-[color:var(--color-background)]">
      <div className="mx-auto flex max-w-6xl flex-col items-start justify-between gap-3 px-4 py-8 text-sm text-[color:var(--color-muted-foreground)] md:flex-row md:items-center">
        <p className="font-mono font-semibold text-[color:var(--color-foreground)]">
          GrokRxiv
        </p>
        <div className="flex flex-wrap items-center gap-4">
          <Link
            href="https://github.com/GrokRxiv"
            target="_blank"
            rel="noopener noreferrer"
            className="hover:underline"
          >
            github.com/GrokRxiv
          </Link>
          <Link href="/llms.txt" className="hover:underline">
            llms.txt
          </Link>
          <Link href="/api-docs" className="hover:underline">
            API
          </Link>
          <Link href="/legal" className="hover:underline">
            Legal
          </Link>
          <Link
            href="mailto:disputes@grokrxiv.org"
            className="hover:underline"
          >
            Disputes
          </Link>
        </div>
      </div>
    </footer>
  );
}
