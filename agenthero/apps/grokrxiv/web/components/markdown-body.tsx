import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import type { Element, Root, Text } from "hast";
import type { Plugin } from "unified";
import { visit } from "unist-util-visit";

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

export function MarkdownBody({
  children,
  macros,
}: {
  children: string;
  macros?: Record<string, string>;
}) {
  return (
    <div className="prose-review max-w-none">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[slugifyHeadings, [rehypeKatex, { macros }]]}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}
