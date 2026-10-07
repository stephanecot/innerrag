import { useEffect, useMemo, useState } from "react";
import Figures from "../Figures";
import { api, type SearchResponse } from "../api";
import { href } from "../App";
import { translate, useT, type Key } from "../i18n";
import { labelVar, num, num2, relevance, splitHighlights, statusLabel } from "../util";

const VIA: { key: "query" | "vector" | "neighbour"; title: Key; className: string }[] = [
  { key: "query", title: "search.viaQuery", className: "entity-chip strong" },
  { key: "vector", title: "search.viaVector", className: "entity-chip" },
  { key: "neighbour", title: "search.viaNeighbour", className: "entity-chip dashed" },
];

export default function SearchView({ project }: { project: string }) {
  const p = useMemo(() => api.project(project), [project]);
  const t = useT();
  const [query, setQuery] = useState("");
  const [k, setK] = useState(8);
  const [useGraph, setUseGraph] = useState(true);
  const [useKeywords, setUseKeywords] = useState(true);
  const [drafts, setDrafts] = useState(false);
  /** null: server default (INNERRAG_MIN_SCORE). */
  const [minScore, setMinScore] = useState<number | null>(null);
  const [tag, setTag] = useState("");
  const [tags, setTags] = useState<{ tag: string; count: number }[]>([]);
  const [result, setResult] = useState<SearchResponse | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [added, setAdded] = useState("");

  /** Adds the current question to the project's reference set (evaluation page). */
  const addReference = async (expect: { doc?: string; pages?: number[]; contains?: string[]; none?: boolean }) => {
    try {
      const set = await p.eval();
      await p.saveEval({ questions: [...set.questions, { id: "", question: query.trim(), expect }] });
      setAdded(translate(expect.none ? "search.addedNone" : "search.addedAnswer"));
    } catch (e) {
      setError((e as Error).message);
    }
  };

  useEffect(() => {
    p.tags().then(setTags).catch(() => setTags([]));
  }, [p]);

  const run = async (e?: React.FormEvent, threshold: number | null = minScore) => {
    e?.preventDefault();
    if (!query.trim()) return;
    setBusy(true);
    setError("");
    try {
      setResult(
        await p.search({
          query, k, use_graph: useGraph, use_keywords: useKeywords, include_drafts: drafts, tags: tag ? [tag] : [],
          min_score: threshold ?? undefined,
        }),
      );
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const highlight = result?.entities.filter((e) => e.via !== "vector").map((e) => e.name) ?? [];

  return (
    <section className="page" aria-label={t("search.aria")}>
      <form role="search" onSubmit={run} style={{ display: "flex", flexDirection: "column", gap: 12 }}>
        <label htmlFor="question"><h1>{t("search.title")}</h1></label>
        <div className="big-search">
          <input id="question" type="search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("search.placeholder")} />
          <button type="submit" className="btn btn-primary" disabled={busy || !query.trim()}>{busy ? t("search.searching") : t("search.submit")}</button>
        </div>
        <div className="toolbar">
          <label className="field-box">
            {t("search.passages")}
            <select value={k} onChange={(e) => setK(Number(e.target.value))}>
              {[3, 5, 8, 12, 20].map((n) => <option key={n} value={n}>{n}</option>)}
            </select>
          </label>
          <label className="field-box">
            <input type="checkbox" checked={useGraph} onChange={(e) => setUseGraph(e.target.checked)} />
            {t("search.useGraph")}
          </label>
          <label className="field-box" title={t("search.keywordsTitle")}>
            <input type="checkbox" checked={useKeywords} onChange={(e) => setUseKeywords(e.target.checked)} />
            {t("search.keywords")}
          </label>
          <label className="field-box">
            <input type="checkbox" checked={drafts} onChange={(e) => setDrafts(e.target.checked)} />
            {t("search.drafts")}
          </label>
          <label className="field-box" title={t("search.thresholdTitle")}>
            {t("search.threshold")}
            <select value={minScore ?? ""} onChange={(e) => setMinScore(e.target.value === "" ? null : Number(e.target.value))}>
              <option value="">{t("search.default")}</option>
              <option value={0.85}>{t("search.strict", { n: num2(0.85) })}</option>
              <option value={0.8}>{num2(0.8)}</option>
              <option value={0.75}>{t("search.loose", { n: num2(0.75) })}</option>
              <option value={0}>{t("search.noThreshold")}</option>
            </select>
          </label>
          {tags.length > 0 && (
            <label className="field-box">
              {t("search.tag")}
              <select value={tag} onChange={(e) => setTag(e.target.value)}>
                <option value="">{t("search.allTags")}</option>
                {tags.map((tg) => <option key={tg.tag} value={tg.tag}>{tg.tag}</option>)}
              </select>
            </label>
          )}
          {result && <span className="muted">{t("search.summary", { passages: t("common.passages", { n: result.chunks.length }), ms: String(result.millis) })}</span>}
        </div>
      </form>

      {error && <div className="error-banner" role="alert">{error}</div>}
      {added && <div className="success" role="status">{added} <a href={href("evaluation")}>{t("search.seeEval")}</a></div>}

      {result && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 24, alignItems: "flex-start" }}>
          <section aria-label={t("search.keptAria")} style={{ flex: "999 1 460px", minWidth: 0, display: "flex", flexDirection: "column", gap: 12 }}>
            {result.chunks.length === 0 && (
              <div className="panel empty">
                {result.best_similarity !== null ? (
                  <>
                    <p>
                      {t("search.noneRelevant", { best: num2(result.best_similarity), min: num2(result.min_score) })}
                    </p>
                    <button type="button" className="btn" onClick={() => addReference({ none: true })}>
                      {t("search.markOffTopic")}
                    </button>
                    {result.min_score > 0.7 && (
                      <button type="button" className="btn" onClick={() => { setMinScore(0.7); run(undefined, 0.7); }}>
                        {t("search.retry", { n: num2(0.7) })}
                      </button>
                    )}
                  </>
                ) : (
                  <p>{t("search.nothing")} {drafts ? "" : t("search.draftsExcluded")}</p>
                )}
              </div>
            )}
            {result.chunks.length > 0 && result.below_threshold > 0 && (
              <p className="muted" style={{ fontSize: 14 }}>
                {t("search.below", { count: t("search.belowCount", { n: result.below_threshold }), min: num2(result.min_score) })}
              </p>
            )}
            {result.chunks.map((c, i) => (
              <article key={c.id} className="hit">
                <span className="hit-rank">{i + 1}</span>
                <div className="hit-body">
                  <div className="hit-head">
                    <a
                      href={href("lire", c.page ? { doc: c.doc_id, page: String(c.page) } : { doc: c.doc_id, tab: "passages" })}
                      style={{ fontWeight: 600, fontSize: 16 }}
                    >
                      {c.doc_title}, {c.page ? t("common.page", { n: String(c.page) }) : t("common.passage", { n: String(c.idx + 1) })}
                    </a>
                    <div className="meters">
                      {c.doc_status === "DRAFT" && <span className={`status status-DRAFT`}>{statusLabel("DRAFT")}</span>}
                      <span className={`score score-${relevance(c.similarity).tone}`} title={t("search.simTitle")}>
                        {t("search.relevance", { score: num2(c.similarity), label: relevance(c.similarity).label })}
                      </span>
                      {c.keyword_boost > 0.001 && (
                        <span className="score score-keyword" title={t("search.kwTitle", { max: num2(0.04) })}>
                          {t("search.kwBoost", { n: num2(c.keyword_boost) })}
                        </span>
                      )}
                      {c.graph_boost > 0.001 && (
                        <span className="score score-graph" title={t("search.graphTitle", { max: num2(0.05) })}>
                          {t("search.graphBoost", { n: num2(c.graph_boost) })}
                        </span>
                      )}
                    </div>
                  </div>
                  <p style={{ color: "var(--ink-2)" }}>
                    {splitHighlights(c.text, highlight).map((part, j) =>
                      part.hit ? <mark key={j}>{part.text}</mark> : <span key={j}>{part.text}</span>,
                    )}
                  </p>
                  {c.images && c.images.length > 0 && <Figures project={project} images={c.images} />}
                  <div>
                    <button
                      type="button"
                      className="btn"
                      style={{ minHeight: 36 }}
                      onClick={() =>
                        addReference(
                          c.page
                            ? { doc: c.doc_id, pages: [c.page] }
                            : { doc: c.doc_id, contains: [c.text.split("\n").slice(-1)[0].slice(0, 60)] },
                        )
                      }
                    >
                      {t("search.goodAnswer")}
                    </button>
                  </div>
                  {c.via_keywords && (
                    <p className="graph-found" style={{ color: "var(--c-event)" }}>{t("search.viaKeywords")}</p>
                  )}
                  {c.via_graph && (
                    <p className="graph-found">{t("search.viaGraph")}</p>
                  )}
                </div>
              </article>
            ))}
            <details className="panel" style={{ padding: "0 18px" }}>
              <summary style={{ minHeight: 48, display: "flex", alignItems: "center", cursor: "pointer", fontWeight: 600 }}>
                {t("search.context", { n: num(Math.ceil(result.context.length / 4)) })}
              </summary>
              <pre className="code" style={{ marginBottom: 18 }}>{result.context}</pre>
            </details>
          </section>

          {result.entities.length > 0 && (
            <section className="panel" aria-label={t("search.entitiesAria")} style={{ flex: "1 1 300px", minWidth: 0, display: "flex", flexDirection: "column", gap: 16, padding: 20 }}>
              <h2>{t("search.graphBrought")}</h2>
              {VIA.map(({ key, title, className }) => {
                const list = result.entities.filter((e) => e.via === key);
                if (!list.length) return null;
                return (
                  <div key={key} style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                    <h3 style={{ fontSize: 14, fontFamily: "var(--font)", color: "var(--ink-soft)" }}>{t(title)}</h3>
                    <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                      {list.map((e) => (
                        <a key={e.id} className={className} href={href("carte", { entity: e.id })} style={{ color: "var(--ink)", textDecoration: "none" }}>
                          <span className="dot" style={{ background: labelVar(e.label), width: 8, height: 8 }} />
                          {e.name}
                        </a>
                      ))}
                    </div>
                  </div>
                );
              })}
            </section>
          )}
        </div>
      )}
    </section>
  );
}
