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

const LAYOUT_COMMAND_RE =
  /\\(?:vspace|hspace|kern|mkern|mskip|hskip|vskip|raisebox|phantom|hphantom|vphantom)\*?(?:\s*\[[^\]]*\])?(?:\s*\{[^{}\n]*\}){1,2}/g;
const TEX_EXPR_RE =
  /\\[A-Za-z]+(?:\*|\{[^{}\n]*\}|\[[^\]\n]*\]|[_^][A-Za-z0-9{}\\]+|\([A-Za-z0-9{}\\_^,+\-*/=. ]+\)|[A-Za-z0-9{}\\_^,+\-*/=.])*/g;
const ALGEBRA_EXPR_RE =
  /[A-Za-z][A-Za-z0-9]*(?:[_^](?:\{[^{}\s]+\}|\([A-Za-z0-9_^\{\}\\,+\-=.*()]+\)|[A-Za-z0-9]+)|\([A-Za-z0-9_^\{\}\\,+\-=.*()]+\)|[A-Za-z0-9])*(?:=[A-Za-z0-9_^\{\}\\,+\-*/=.()]+)?/g;

type MathCandidate = { start: number; end: number };

export function normalizeDisplayMathText(input: string): string {
  const stripped = input.replace(LAYOUT_COMMAND_RE, " ").replace(/[ \t]{2,}/g, " ");
  return transformProtectedSpans(stripped, wrapMathCandidates).trim();
}

function transformProtectedSpans(
  input: string,
  transform: (segment: string) => string,
): string {
  let out = "";
  let cursor = 0;
  while (cursor < input.length) {
    const protectedEnd = protectedSpanEnd(input, cursor);
    if (protectedEnd !== null) {
      out += input.slice(cursor, protectedEnd);
      cursor = protectedEnd;
      continue;
    }
    const next = nextProtectedStart(input, cursor) ?? input.length;
    out += transform(input.slice(cursor, next));
    cursor = next;
  }
  return out;
}

function protectedSpanEnd(input: string, cursor: number): number | null {
  const rest = input.slice(cursor);
  if (rest.startsWith("`")) return findAfter(input, cursor, "`");
  if (rest.startsWith("$")) return findAfter(input, cursor, "$");
  if (rest.startsWith("\\(")) return findAfter(input, cursor, "\\)");
  if (rest.startsWith("\\[")) return findAfter(input, cursor, "\\]");
  return null;
}

function findAfter(input: string, cursor: number, delimiter: string): number | null {
  const start = cursor + delimiter.length;
  const index = input.indexOf(delimiter, start);
  return index === -1 ? null : index + delimiter.length;
}

function nextProtectedStart(input: string, cursor: number): number | null {
  const starts = ["`", "$", "\\(", "\\["]
    .map((needle) => input.indexOf(needle, cursor))
    .filter((index) => index >= 0);
  return starts.length ? Math.min(...starts) : null;
}

function wrapMathCandidates(segment: string): string {
  const candidates = [
    ...collectCandidates(TEX_EXPR_RE, segment),
    ...collectCandidates(ALGEBRA_EXPR_RE, segment),
  ].sort((a, b) => a.start - b.start || b.end - b.start - (a.end - a.start));

  let out = "";
  let cursor = 0;
  for (const candidate of candidates) {
    if (candidate.start < cursor) continue;
    const end = trimTrailingSentencePunctuation(segment, candidate.start, candidate.end);
    const expr = segment.slice(candidate.start, end);
    if (!isWrappableMath(segment, candidate.start, end, expr)) continue;
    out += segment.slice(cursor, candidate.start);
    out += `$${expr}$`;
    cursor = end;
  }
  out += segment.slice(cursor);
  return out;
}

function trimTrailingSentencePunctuation(
  segment: string,
  start: number,
  end: number,
): number {
  while (end > start && [".", ",", ";", ":"].includes(segment[end - 1])) {
    end -= 1;
  }
  return end;
}

function collectCandidates(re: RegExp, segment: string): MathCandidate[] {
  re.lastIndex = 0;
  const out: MathCandidate[] = [];
  for (const match of segment.matchAll(re)) {
    if (match.index === undefined) continue;
    out.push({ start: match.index, end: match.index + match[0].length });
  }
  return out;
}

function isWrappableMath(
  segment: string,
  start: number,
  end: number,
  expr: string,
): boolean {
  if (expr.length < 3 || expr.length > 180) return false;
  if (expr.includes("$") || expr.includes("://") || expr.includes("@")) return false;
  if (isPlainSnakeCaseIdentifier(expr)) return false;
  const prev = start > 0 ? segment[start - 1] : "";
  const next = end < segment.length ? segment[end] : "";
  if (prev === "/" || prev === "`" || prev === "$" || prev === "#" || prev === "&") {
    return false;
  }
  if (next === "/" || next === "`" || next === "$") return false;
  return expr.startsWith("\\") || expr.includes("_") || expr.includes("^");
}

function isPlainSnakeCaseIdentifier(expr: string): boolean {
  if (/[\\^{}]/.test(expr)) return false;
  if (/[A-Z0-9]/.test(expr)) return false;
  const [base, ...rest] = expr.split("_");
  if (!base || rest.length === 0 || base.length <= 1) return false;
  return rest.join("_").split("").every((ch) => ch === "_" || /[a-z]/.test(ch));
}

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
