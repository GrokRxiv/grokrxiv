import DOMPurify from "isomorphic-dompurify";
import { normalizeDisplayMathText, renderMathInString } from "@/lib/render-math";

/**
 * Inline-math-aware text renderer for short strings (titles, captions,
 * card previews). The full review body uses `MarkdownBody`; this is purpose
 * built for one-line content where we want `$\rho$` to render as the Greek
 * letter instead of literal dollar signs.
 *
 * XSS defense in depth:
 *   1. HTML-escape the input string. KaTeX's math regex only consumes
 *      `$...$` / `\(...\)` / `\[...\]` delimiters and those escapers don't
 *      transform inside text, so escaping first is lossless for the math.
 *   2. Run KaTeX. Its output is well-formed HTML.
 *   3. Pass the rendered string through DOMPurify (same sanitizer the
 *      review body uses) before inserting via `dangerouslySetInnerHTML`.
 */
export function MathText({
  children,
  as: Tag = "span",
  className,
}: {
  children: string;
  as?: "span" | "h1" | "h2" | "h3" | "p";
  className?: string;
}) {
  const escaped = escapeHtml(normalizeDisplayMathText(children));
  const rendered = renderMathInString(escaped);
  const safe = DOMPurify.sanitize(rendered, {
    USE_PROFILES: { html: true, mathMl: true, svg: true },
  });
  return (
    <Tag
      className={className}
      dangerouslySetInnerHTML={{ __html: safe }}
    />
  );
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
