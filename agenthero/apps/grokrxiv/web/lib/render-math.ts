import katex from "katex";

// Base macro set — operator names and LaTeX commands not in KaTeX core.
// Per-paper macros (extracted from \newcommand in source TeX) should be merged
// on top via the second argument to renderMathInString.
const BASE_MACROS: Record<string, string> = {
  "\\slashed": "\\not{#1}",
  "\\Tr": "\\operatorname{Tr}",
  "\\tr": "\\operatorname{tr}",
  "\\diag": "\\operatorname{diag}",
  "\\sgn": "\\operatorname{sgn}",
  "\\Hom": "\\operatorname{Hom}",
  "\\Mor": "\\operatorname{Mor}",
  "\\End": "\\operatorname{End}",
};

/**
 * Server-side KaTeX renderer. Normalizes `$...$` / `$$...$$` to
 * `\(...\)` / `\[...\]` then renders each delimiter pair to KaTeX HTML.
 *
 * Errors are swallowed (throwOnError: false) so a single malformed expression
 * never blanks the whole review body — they fall back to an inline <code> tag.
 */
export function renderMathInString(
  input: string,
  customMacros: Record<string, string> = {},
): string {
  const macros = { ...BASE_MACROS, ...customMacros };

  // Pre-pass: $$...$$ → \[...\] and $...$ → \(...\).
  // Order matters: do display first so the inline regex doesn't eat half a
  // display delimiter. The display regex is greedy across newlines; inline is
  // restricted to a single line to avoid swallowing currency-style $ usage.
  let text = input
    .replace(/\$\$([\s\S]+?)\$\$/g, (_, m) => `\\[${m}\\]`)
    .replace(/\$([^$\n]+?)\$/g, (_, m) => `\\(${m}\\)`);

  // Display math \[...\]
  text = text.replace(/\\\[([\s\S]+?)\\\]/g, (_, expr) => {
    try {
      return katex.renderToString(expr, {
        displayMode: true,
        macros,
        throwOnError: false,
        strict: false,
      });
    } catch {
      return `<code>${expr}</code>`;
    }
  });

  // Inline math \(...\)
  text = text.replace(/\\\(([\s\S]+?)\\\)/g, (_, expr) => {
    try {
      return katex.renderToString(expr, {
        displayMode: false,
        macros,
        throwOnError: false,
        strict: false,
      });
    } catch {
      return `<code>${expr}</code>`;
    }
  });

  return text;
}

/**
 * Stub for the future per-paper macro extraction step. The eventual
 * implementation should scan the TeX source for
 * `\newcommand{\name}[args]{definition}` and emit KaTeX-compatible macros.
 */
export function extractMacrosFromTexSource(tex: string): Record<string, string> {
  void tex;
  return {};
}
