"use client";

import { useEffect, useState } from "react";

import { cn } from "@/lib/utils";
import type { TocItem } from "@/lib/toc";

export type { TocItem } from "@/lib/toc";

/**
 * Sticky sidebar TOC. We accept an `items` prop (server-built, so SSR is
 * happy) and only use the client for the scroll-spy active-section
 * highlighting. IntersectionObserver covers the common case; the rootMargin
 * is tuned to fire as a heading approaches the top of the viewport.
 */
export function ReviewToc({ items }: { items: TocItem[] }) {
  const [activeId, setActiveId] = useState<string | null>(items[0]?.id ?? null);

  useEffect(() => {
    if (items.length === 0) return;
    const targets = items
      .map((i) => document.getElementById(i.id))
      .filter((el): el is HTMLElement => el !== null);
    if (targets.length === 0) return;

    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]?.target.id) {
          setActiveId(visible[0].target.id);
        }
      },
      {
        rootMargin: "-80px 0px -65% 0px",
        threshold: [0, 1],
      },
    );

    for (const t of targets) observer.observe(t);
    return () => observer.disconnect();
  }, [items]);

  if (items.length === 0) return null;

  return (
    <nav
      aria-label="Table of contents"
      className="hidden lg:block"
    >
      <p className="mb-3 text-xs font-semibold uppercase tracking-wider text-[color:var(--color-muted-foreground)]">
        On this page
      </p>
      <ul className="flex flex-col gap-1 border-l border-[color:var(--color-border)] text-sm">
        {items.map((item) => (
          <li key={item.id}>
            <a
              href={`#${item.id}`}
              className={cn(
                "block border-l-2 py-1 pl-3 -ml-px transition-colors",
                item.level === 3 && "pl-6",
                activeId === item.id
                  ? "border-[color:var(--color-foreground)] text-[color:var(--color-foreground)] font-medium"
                  : "border-transparent text-[color:var(--color-muted-foreground)] hover:text-[color:var(--color-foreground)]",
              )}
            >
              {item.text}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}

