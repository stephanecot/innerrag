// Minimal markdown renderer for the reader: headings, paragraphs, lists, code fences,
// pipe tables, page markers, **bold** and `code`. Produces React elements (no HTML injection).
import type { ReactNode } from "react";
import { useT } from "./i18n";

export type Block =
  | { kind: "heading"; level: number; text: string; id: string }
  | { kind: "para"; text: string }
  | { kind: "quote"; text: string }
  | { kind: "list"; items: string[] }
  | { kind: "code"; text: string }
  | { kind: "table"; rows: string[][] }
  | { kind: "page"; page: number };

const PAGE = /^<!--\s*page\s+(\d+)\s*-->$/;

export function slug(text: string, used: Map<string, number>): string {
  const base = text.toLowerCase().normalize("NFD").replace(/[̀-ͯ]/g, "").replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "section";
  const n = used.get(base) ?? 0;
  used.set(base, n + 1);
  return n ? `${base}-${n}` : base;
}

export function parse(md: string): Block[] {
  const blocks: Block[] = [];
  const lines = md.split("\n");
  const used = new Map<string, number>();
  let para: string[] = [];
  let list: string[] = [];
  let table: string[][] = [];
  let quote: string[] = [];
  const flush = () => {
    if (quote.length) blocks.push({ kind: "quote", text: quote.join(" ") });
    if (para.length) blocks.push({ kind: "para", text: para.join(" ") });
    if (list.length) blocks.push({ kind: "list", items: list });
    if (table.length) blocks.push({ kind: "table", rows: table });
    para = [];
    list = [];
    table = [];
    quote = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();
    if (trimmed.startsWith("```")) {
      flush();
      const code: string[] = [];
      while (++i < lines.length && !lines[i].trim().startsWith("```")) code.push(lines[i]);
      blocks.push({ kind: "code", text: code.join("\n") });
      continue;
    }
    const page = PAGE.exec(trimmed);
    if (page) {
      flush();
      blocks.push({ kind: "page", page: Number(page[1]) });
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flush();
      const text = heading[2].replace(/#+$/, "").trim();
      blocks.push({ kind: "heading", level: heading[1].length, text, id: slug(text, used) });
      continue;
    }
    if (!trimmed) {
      flush();
      continue;
    }
    if (trimmed.startsWith(">")) {
      if (para.length || list.length || table.length) flush();
      quote.push(trimmed.replace(/^>\s?/, ""));
      continue;
    }
    if (quote.length) flush();
    const item = /^(?:[-*•]|\d+[.)])\s+(.*)$/.exec(trimmed);
    if (item) {
      if (para.length) flush();
      list.push(item[1]);
      continue;
    }
    if (trimmed.startsWith("|") && trimmed.endsWith("|")) {
      if (para.length || list.length) flush();
      const cells = trimmed.slice(1, -1).split("|").map((c) => c.trim());
      if (!cells.every((c) => /^:?-{2,}:?$/.test(c))) table.push(cells);
      continue;
    }
    if (list.length) flush();
    para.push(trimmed.replace(/^\\#/, "#"));
  }
  flush();
  return blocks;
}

/** Turns plain text into nodes, for instance links on document citations. */
export type Linker = (text: string, key: string) => ReactNode[];

/** **bold**, *italic* and `code` spans; `linker` handles the plain text in between. */
export function inline(text: string, linker?: Linker, prefix = ""): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /(`[^`]+`|\*\*[^*]+\*\*|(?<![\w*])\*[^*\s][^*]*?(?<!\s)\*(?![\w*]))/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let k = 0;
  const plain = (t: string) => {
    const key = `${prefix}${k++}`;
    if (linker) out.push(...linker(t, key));
    else out.push(t);
  };
  while ((m = re.exec(text))) {
    if (m.index > last) plain(text.slice(last, m.index));
    const token = m[0];
    const key = `${prefix}${k++}`;
    out.push(
      token.startsWith("`") ? <code key={key}>{token.slice(1, -1)}</code>
        : token.startsWith("**") ? <strong key={key}>{inline(token.slice(2, -2), linker, `${key}.`)}</strong>
        : <em key={key}>{inline(token.slice(1, -1), linker, `${key}.`)}</em>,
    );
    last = m.index + token.length;
  }
  if (last < text.length) plain(text.slice(last));
  return out;
}

export function Blocks({
  blocks, onPage, highlightPage, linker,
}: { blocks: Block[]; onPage?: (page: number) => void; highlightPage?: number; linker?: Linker }) {
  const t = useT();
  return (
    <>
      {blocks.map((b, i) => {
        switch (b.kind) {
          case "heading": {
            const Tag = (`h${Math.min(b.level + 1, 6)}`) as "h2";
            return <Tag key={i} id={b.id} className={`md-h md-h${b.level}`}>{inline(b.text, linker)}</Tag>;
          }
          case "para":
            return <p key={i} className="md-p">{inline(b.text, linker)}</p>;
          case "quote":
            return <blockquote key={i} className="md-quote">{inline(b.text, linker)}</blockquote>;
          case "list":
            return <ul key={i} className="md-list">{b.items.map((it, j) => <li key={j}>{inline(it, linker)}</li>)}</ul>;
          case "code":
            return <pre key={i} className="md-code"><code>{b.text}</code></pre>;
          case "table":
            return (
              <div key={i} className="md-table">
                <table>
                  <tbody>{b.rows.map((r, j) => <tr key={j}>{r.map((c, k) => <td key={k}>{inline(c, linker)}</td>)}</tr>)}</tbody>
                </table>
              </div>
            );
          case "page":
            return (
              <div key={i} id={`page-${b.page}`} className={`md-page${highlightPage === b.page ? " current" : ""}`}>
                {onPage ? (
                  <button type="button" onClick={() => onPage(b.page)} title={t("markdown.openPage")}>
                    {t("common.page", { n: String(b.page) })}
                  </button>
                ) : (
                  <span>{t("common.page", { n: String(b.page) })}</span>
                )}
              </div>
            );
        }
      })}
    </>
  );
}
