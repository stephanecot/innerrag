import { useEffect, useMemo, useState } from "react";
import { api, type DocumentContent, type FileInfo, type PassagePage } from "../api";
import { go, href } from "../App";
import { Blocks, parse, type Block } from "../markdown";
import { useT, type T } from "../i18n";
import { num } from "../util";

interface Section {
  title: string;
  blocks: Block[];
  pages: number[];
}

/** Splits the document into chapters (its top heading level), so long books stay light. */
function sectionize(blocks: Block[], t: T): Section[] {
  const levels = blocks.filter((b) => b.kind === "heading").map((b) => (b as { level: number }).level);
  const top = levels.length ? Math.min(...levels) : 0;
  const sections: Section[] = [];
  let current: Section = { title: t("reader.start"), blocks: [], pages: [] };
  for (const b of blocks) {
    if (b.kind === "heading" && b.level === top && current.blocks.some((x) => x.kind !== "page")) {
      sections.push(current);
      current = { title: b.text, blocks: [], pages: [] };
    } else if (b.kind === "heading" && b.level === top) {
      current.title = b.text;
    }
    if (b.kind === "page") current.pages.push(b.page);
    current.blocks.push(b);
  }
  if (current.blocks.length) sections.push(current);
  // Short documents read in one go.
  const size = blocks.reduce((n, b) => n + ("text" in b ? b.text.length : 0), 0);
  if (size < 60_000 || sections.length < 2) {
    return [{ title: t("reader.document"), blocks, pages: sections.flatMap((s) => s.pages) }];
  }
  return sections;
}

export default function ReaderView({ project, params }: { project: string; params: URLSearchParams }) {
  const docId = params.get("doc") ?? "";
  const targetPage = Number(params.get("page") ?? "") || undefined;
  const p = useMemo(() => api.project(project), [project]);
  const t = useT();
  const [doc, setDoc] = useState<DocumentContent | null>(null);
  const [error, setError] = useState("");
  const [tab, setTab] = useState<"text" | "passages">(params.get("tab") === "passages" ? "passages" : "text");
  const [index, setIndex] = useState(0);

  useEffect(() => {
    setDoc(null);
    p.content(docId).then(setDoc).catch((e) => setError((e as Error).message));
  }, [p, docId]);

  const sections = useMemo(() => (doc ? sectionize(parse(doc.content), t) : []), [doc, t]);
  // Documents indexed before the reader existed have no stored text: show their passages.
  const noText = doc !== null && !doc.content.trim();
  useEffect(() => {
    if (noText) setTab("passages");
  }, [noText]);
  const file = (doc?.metadata as { file?: FileInfo } | null)?.file;
  const original = file?.stored ? p.fileUrl(docId) : null;
  const isPdf = file?.format === "pdf";

  // Open the chapter holding the requested page, then scroll to it.
  useEffect(() => {
    if (!targetPage || !sections.length) return;
    const i = sections.findIndex((s) => s.pages.includes(targetPage));
    if (i >= 0) setIndex(i);
    setTimeout(() => document.getElementById(`page-${targetPage}`)?.scrollIntoView({ block: "start" }), 50);
  }, [targetPage, sections]);

  const outline = useMemo(
    () =>
      sections.flatMap((s, si) =>
        s.blocks
          .filter((b): b is Extract<Block, { kind: "heading" }> => b.kind === "heading" && b.level <= 2)
          .map((b) => ({ ...b, section: si })),
      ),
    [sections],
  );

  const openPage = (page: number) => original && window.open(`${original}#page=${page}`, "_blank", "noopener");
  const section = sections[Math.min(index, sections.length - 1)];

  if (error) return <div className="page"><div className="error-banner" role="alert">{error}</div></div>;
  if (!doc) return <div className="page"><p className="muted">{t("reader.loadingDoc")}</p></div>;

  return (
    <div className="reader">
      <header className="reader-head">
        <div style={{ minWidth: 0 }}>
          <a href={href("documents", { doc: docId })} className="muted" style={{ fontSize: 14 }}>{t("reader.back")}</a>
          <h1 style={{ fontSize: 30, marginTop: 4 }}>{doc.title}</h1>
          {file && (
            <p className="muted" style={{ fontSize: 14 }}>
              {file.format_label}
              {file.pages ? `, ${t(isPdf ? "common.pages" : "docs.slides", { n: file.pages })}` : ""}
              {sections.length > 1 ? `, ${t("reader.chapters", { n: sections.length })}` : ""}
            </p>
          )}
        </div>
        <div className="toolbar">
          <div className="segmented" role="group" aria-label={t("reader.display")}>
            <button type="button" aria-pressed={tab === "text"} disabled={noText} onClick={() => setTab("text")}>{t("reader.text")}</button>
            <button type="button" aria-pressed={tab === "passages"} onClick={() => setTab("passages")}>{t("reader.passages")}</button>
          </div>
          {original && (
            <a className="btn" href={original} target="_blank" rel="noopener">
              {t("reader.openOriginal")}{file ? ` (${file.format_label})` : ""}
            </a>
          )}
        </div>
      </header>

      {noText && (
        <div className="note" style={{ margin: "24px 32px 0" }}>
          {t("reader.noText")}
        </div>
      )}
      {tab === "passages" ? (
        <Passages project={project} docId={docId} onPage={isPdf && original ? openPage : undefined} />
      ) : (
        <div className="reader-body">
          {outline.length > 1 && (
            <nav className="reader-outline" aria-label={t("reader.outline")}>
              <ol>
                {outline.map((h) => (
                  <li key={`${h.section}-${h.id}`} className={`lvl-${h.level}${h.section === index ? " here" : ""}`}>
                    <a
                      href={`#${h.id}`}
                      onClick={(e) => {
                        e.preventDefault();
                        setIndex(h.section);
                        setTimeout(() => document.getElementById(h.id)?.scrollIntoView({ block: "start" }), 30);
                      }}
                    >
                      {h.text}
                    </a>
                  </li>
                ))}
              </ol>
            </nav>
          )}
          <article className="reader-text" aria-label={section?.title}>
            {section && <Blocks blocks={section.blocks} onPage={isPdf && original ? openPage : undefined} highlightPage={targetPage} project={project} />}
            {sections.length > 1 && (
              <div className="reader-nav">
                <button type="button" className="btn" disabled={index === 0} onClick={() => { setIndex(index - 1); window.scrollTo(0, 0); }}>
                  ← {sections[index - 1]?.title ?? t("reader.previous")}
                </button>
                <span className="muted">{index + 1} / {sections.length}</span>
                <button type="button" className="btn" disabled={index >= sections.length - 1} onClick={() => { setIndex(index + 1); window.scrollTo(0, 0); }}>
                  {sections[index + 1]?.title ?? t("reader.next")} →
                </button>
              </div>
            )}
          </article>
        </div>
      )}
    </div>
  );
}

const PAGE_SIZE = 50;

function Passages({ project, docId, onPage }: { project: string; docId: string; onPage?: (page: number) => void }) {
  const [offset, setOffset] = useState(0);
  const [data, setData] = useState<PassagePage | null>(null);
  const t = useT();
  useEffect(() => {
    api.project(project).passages(docId, offset, PAGE_SIZE).then(setData).catch(() => setData(null));
  }, [project, docId, offset]);
  if (!data) return <p className="muted reader-text">{t("common.loading")}</p>;
  const pager = (
    <div className="reader-nav">
      <button type="button" className="btn" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}>{t("reader.prevPassages")}</button>
      <span className="muted">
        {t("reader.range", { from: num(offset + 1), to: num(Math.min(offset + PAGE_SIZE, data.total)), total: num(data.total) })}
      </span>
      <button type="button" className="btn" disabled={offset + PAGE_SIZE >= data.total} onClick={() => setOffset(offset + PAGE_SIZE)}>{t("reader.nextPassages")}</button>
    </div>
  );
  return (
    <div className="reader-text" style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <p className="muted">{t("reader.passagesIntro")}</p>
      {pager}
      {data.passages.map((c) => (
        <article key={c.id} className="passage">
          <div className="passage-head">
            <strong>{t("reader.passage", { n: String(c.idx + 1) })}</strong>
            {c.page && (onPage ? <button type="button" className="link-button" onClick={() => onPage(c.page!)}>{t("common.page", { n: String(c.page) })}</button> : <span className="muted">{t("common.page", { n: String(c.page) })}</span>)}
          </div>
          <p style={{ whiteSpace: "pre-line" }}>{c.text}</p>
          {c.entities.length > 0 && (
            <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
              {c.entities.map((name) => (
                <button
                  key={name}
                  type="button"
                  className="entity-chip"
                  style={{ background: "var(--surface)" }}
                  onClick={async () => {
                    const [hit] = await api.project(project).entities({ q: name, limit: 1 }).catch(() => []);
                    go("carte", hit ? { entity: hit.id } : {});
                  }}
                >
                  {name}
                </button>
              ))}
            </div>
          )}
        </article>
      ))}
      {pager}
    </div>
  );
}
