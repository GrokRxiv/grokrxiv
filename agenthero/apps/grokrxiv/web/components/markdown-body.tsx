import DOMPurify from "isomorphic-dompurify";
import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import rehypeRaw from "rehype-raw";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import type { Element, Root, Text } from "hast";
import type { Plugin } from "unified";
import { visit } from "unist-util-visit";

import { renderMathInString } from "@/lib/render-math";

// Auto-generate `id` attributes on h2/h3 elements so the TOC anchors can
// scroll to them. This is the same job `rehype-slug` does but we keep the
// dependency surface small.
const slugifyHeadings: Plugin<[], Root> = () => {
  const used = new Map<string, number>();
  const slugify = (s: string) =>
    s
      .toLowerCase()
      .replace(/[^\w\s-]/g, "")
      .replace(/\s+/g, "-")
      .replace(/-+/g, "-")
      .replace(/^-|-$/g, "");
  return (tree) => {
    used.clear();
    visit(tree, "element", (node: Element) => {
      if (node.tagName !== "h2" && node.tagName !== "h3") return;
      if (node.properties?.id) return;
      const text = collectText(node);
      const base = slugify(text);
      if (!base) return;
      const n = used.get(base) ?? 0;
      used.set(base, n + 1);
      const id = n === 0 ? base : `${base}-${n}`;
      node.properties = { ...(node.properties ?? {}), id };
    });
  };
};

function collectText(node: Element): string {
  let out = "";
  visit(node, "text", (t: Text) => {
    out += t.value;
  });
  return out.trim();
}

// DOMPurify defaults strip the MathML / SVG tags that KaTeX emits. We expand
// the allow-lists so the rendered math survives sanitization intact.
const KATEX_TAGS = [
  "math",
  "annotation",
  "semantics",
  "mtext",
  "mn",
  "mo",
  "mi",
  "mspace",
  "mover",
  "munder",
  "munderover",
  "msup",
  "msub",
  "msubsup",
  "mfrac",
  "mroot",
  "msqrt",
  "mtable",
  "mtr",
  "mtd",
  "mlabeledtr",
  "mrow",
  "menclose",
  "mstyle",
  "mpadded",
  "mphantom",
  "mglyph",
  "svg",
  "path",
  "line",
  "rect",
];

const KATEX_ATTRS = [
  "accent",
  "accentunder",
  "align",
  "bevelled",
  "close",
  "columnalign",
  "columnlines",
  "columnspacing",
  "denomalign",
  "depth",
  "dir",
  "display",
  "displaystyle",
  "encoding",
  "fence",
  "frame",
  "height",
  "linethickness",
  "lspace",
  "lquote",
  "mathbackground",
  "mathcolor",
  "mathsize",
  "mathvariant",
  "maxsize",
  "minsize",
  "movablelimits",
  "notation",
  "numalign",
  "open",
  "rowalign",
  "rowlines",
  "rowspacing",
  "rquote",
  "rspace",
  "scriptlevel",
  "scriptminsize",
  "scriptsizemultiplier",
  "selection",
  "separator",
  "separators",
  "stretchy",
  "subscriptshift",
  "supscriptshift",
  "symmetric",
  "voffset",
  "width",
  "aria-hidden",
  "viewbox",
  "preserveaspectratio",
  "d",
  "x",
  "y",
  "x1",
  "x2",
  "y1",
  "y2",
  "fill",
  "stroke",
  "stroke-width",
];

function sanitizeKatex(html: string): string {
  return DOMPurify.sanitize(html, {
    ADD_TAGS: KATEX_TAGS,
    ADD_ATTR: KATEX_ATTRS,
  });
}

export function MarkdownBody({
  children,
  macros,
}: {
  children: string;
  macros?: Record<string, string>;
}) {
  // Step 1: pre-render LaTeX delimiters inside the markdown body to KaTeX
  // HTML. Doing this BEFORE react-markdown means the resulting
  // <span class="katex"> markup is parsed as raw HTML by rehype-raw rather
  // than escaped as plain text.
  const withMath = renderMathInString(children, macros);
  // Step 2: sanitize the pre-rendered HTML. The markdown source itself is
  // trusted (server-side from our own DB) but defense-in-depth is cheap and
  // keeps KaTeX output well-formed before react-markdown consumes it.
  const safe = sanitizeKatex(withMath);

  return (
    <div className="prose-review max-w-none">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[rehypeRaw, slugifyHeadings, rehypeKatex]}
      >
        {safe}
      </ReactMarkdown>
    </div>
  );
}
