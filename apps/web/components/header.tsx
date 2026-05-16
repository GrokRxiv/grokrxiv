import Link from "next/link";
import { ThemeToggle } from "@/components/theme-toggle";

const NAV_LINKS = [
  { href: "/reviews", label: "Reviews" },
  { href: "/about", label: "About" },
  { href: "/api-docs", label: "API" },
];

export function Header() {
  return (
    <header className="sticky top-0 z-40 w-full border-b border-[color:var(--color-border)] bg-[color:var(--color-background)]/80 backdrop-blur">
      <div className="mx-auto flex h-16 max-w-6xl items-center justify-between gap-2 px-4">
        <Link
          href="/"
          className="font-mono text-lg font-bold tracking-tight"
        >
          GrokRxiv
        </Link>

        {/* Desktop + tablet nav (>= sm). Hidden on narrow phones. */}
        <nav className="hidden items-center gap-1 text-sm sm:flex">
          {NAV_LINKS.map((l) => (
            <Link
              key={l.label}
              href={l.href}
              className="rounded-md px-3 py-1.5 hover:bg-[color:var(--color-accent)]"
            >
              {l.label}
            </Link>
          ))}
          <Link
            href="/#upload"
            className="rounded-md bg-[color:var(--color-primary)] px-3 py-1.5 text-[color:var(--color-primary-foreground)] hover:opacity-90"
          >
            Upload
          </Link>
          <ThemeToggle />
        </nav>

        {/* Mobile nav (< sm). <details>-based disclosure so it works without JS. */}
        <div className="flex items-center gap-2 sm:hidden">
          <Link
            href="/#upload"
            className="rounded-md bg-[color:var(--color-primary)] px-3 py-1.5 text-sm text-[color:var(--color-primary-foreground)] hover:opacity-90"
          >
            Upload
          </Link>
          <ThemeToggle />
          <details className="relative">
            <summary
              aria-label="Open navigation menu"
              className="flex h-9 w-9 cursor-pointer list-none items-center justify-center rounded-md border border-[color:var(--color-border)] [&::-webkit-details-marker]:hidden"
            >
              <svg
                xmlns="http://www.w3.org/2000/svg"
                width="18"
                height="18"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
              >
                <line x1="3" y1="6" x2="21" y2="6" />
                <line x1="3" y1="12" x2="21" y2="12" />
                <line x1="3" y1="18" x2="21" y2="18" />
              </svg>
            </summary>
            <nav className="absolute right-0 top-full mt-2 flex w-44 flex-col rounded-md border border-[color:var(--color-border)] bg-[color:var(--color-background)] py-1 text-sm shadow-md">
              {NAV_LINKS.map((l) => (
                <Link
                  key={l.label}
                  href={l.href}
                  className="px-3 py-2 hover:bg-[color:var(--color-accent)]"
                >
                  {l.label}
                </Link>
              ))}
            </nav>
          </details>
        </div>
      </div>
    </header>
  );
}
