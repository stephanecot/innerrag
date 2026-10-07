// Rich rendering of the assistant's answers: GitHub-flavoured Markdown, inline HTML and SVG, and
// ```html / ```svg blocks shown as a preview with their source. Everything goes through
// DOMPurify: no script, no event handler, no form or frame can come out of an answer.
import DOMPurify, { type Config } from "dompurify";
import { marked, type Tokens } from "marked";
import { useMemo } from "react";
import { href } from "./App";
import { useT } from "./i18n";

export interface CitedDocument {
  id: string;
  title: string;
}

const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Turns "Pro Android 5, page 486" into a link to the reader, outside code and existing links. */
function linkCitations(md: string, docs: CitedDocument[]): string {
  const titled = docs.filter((d) => d.title.trim().length >= 3).sort((a, b) => b.title.length - a.title.length);
  if (!titled.length) return md;
  const byTitle = new Map(titled.map((d) => [d.title.toLowerCase(), d.id]));
  const cite = new RegExp(`(${titled.map((d) => escapeRe(d.title)).join("|")})(\\*?,?\\s*(?:pages?|p\\.)\\s*(\\d+))?`, "gi");
  // Code fences, inline code and existing links are left untouched.
  const protectedParts = /(```[\s\S]*?```|`[^`\n]+`|\[[^\]]*\]\([^)]*\))/g;
  return md
    .split(protectedParts)
    .map((part, i) => {
      if (i % 2 === 1) return part;
      return part.replace(cite, (m, title: string, _rest: string, page?: string) => {
        const doc = byTitle.get(title.toLowerCase());
        if (!doc) return m;
        return `[${m}](${href("lire", page ? { doc, page } : { doc })})`;
      });
    })
    .join("");
}

const PURIFY: Config = {
  USE_PROFILES: { html: true, svg: true, svgFilters: true },
  FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select", "iframe", "frame", "object", "embed", "link", "meta", "base"],
  ADD_ATTR: ["target"],
};

let hooked = false;
function purify(html: string): string {
  if (!hooked) {
    hooked = true;
    // External links open in a new tab, without access to this page.
    DOMPurify.addHook("afterSanitizeAttributes", (node) => {
      if (node.tagName === "A" && /^https?:/i.test(node.getAttribute("href") ?? "")) {
        node.setAttribute("target", "_blank");
        node.setAttribute("rel", "noopener noreferrer");
      }
    });
  }
  return String(DOMPurify.sanitize(html, PURIFY));
}

function renderMarkdown(md: string, labels: { preview: string; source: string }): string {
  const renderer = new marked.Renderer();
  renderer.code = ({ text, lang }: Tokens.Code) => {
    const language = (lang ?? "").trim().split(/\s+/)[0].toLowerCase();
    const source = `<pre class="rich-code"><code>${escapeHtml(text)}</code></pre>`;
    if (language === "svg" || language === "html") {
      // The preview is sanitized again with the whole answer; the source stays one click away.
      return `<figure class="rich-preview" aria-label="${labels.preview}">${text}</figure>`
        + `<details class="rich-source"><summary>${labels.source}</summary>${source}</details>`;
    }
    return language ? source.replace("<code>", `<code class="language-${escapeHtml(language)}">`) : source;
  };
  return marked.parse(md, { gfm: true, breaks: false, renderer, async: false }) as string;
}

export default function RichText({ text, docs = [] }: { text: string; docs?: CitedDocument[] }) {
  const t = useT();
  const html = useMemo(
    () => purify(renderMarkdown(linkCitations(text, docs), { preview: t("rich.preview"), source: t("rich.source") })),
    [text, docs, t],
  );
  return <div className="rich" dangerouslySetInnerHTML={{ __html: html }} />;
}
