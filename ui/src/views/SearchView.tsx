import { useEffect, useMemo, useState } from "react";
import { api, type SearchResponse } from "../api";
import { href } from "../App";
import { fr2, labelVar, plural, relevance, splitHighlights, STATUS_LABEL } from "../util";

const VIA: { key: "query" | "vector" | "neighbour"; title: string; className: string }[] = [
  { key: "query", title: "Nommées dans la question", className: "entity-chip strong" },
  { key: "vector", title: "Proches de la question", className: "entity-chip" },
  { key: "neighbour", title: "Voisines dans le graphe", className: "entity-chip dashed" },
];

export default function SearchView({ project }: { project: string }) {
  const p = useMemo(() => api.project(project), [project]);
  const [query, setQuery] = useState("");
  const [k, setK] = useState(8);
  const [useGraph, setUseGraph] = useState(true);
  const [drafts, setDrafts] = useState(false);
  /** null: server default (INNERRAG_MIN_SCORE). */
  const [minScore, setMinScore] = useState<number | null>(null);
  const [tag, setTag] = useState("");
  const [tags, setTags] = useState<{ tag: string; count: number }[]>([]);
  const [result, setResult] = useState<SearchResponse | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

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
          query, k, use_graph: useGraph, include_drafts: drafts, tags: tag ? [tag] : [],
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
    <section className="page" aria-label="Recherche">
      <form role="search" onSubmit={run} style={{ display: "flex", flexDirection: "column", gap: 12 }}>
        <label htmlFor="question"><h1>Tester une question</h1></label>
        <div className="big-search">
          <input id="question" type="search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Posez une question sur les documents du projet" />
          <button type="submit" className="btn btn-primary" disabled={busy || !query.trim()}>{busy ? "Recherche…" : "Rechercher"}</button>
        </div>
        <div className="toolbar">
          <label className="field-box">
            Passages
            <select value={k} onChange={(e) => setK(Number(e.target.value))}>
              {[3, 5, 8, 12, 20].map((n) => <option key={n} value={n}>{n}</option>)}
            </select>
          </label>
          <label className="field-box">
            <input type="checkbox" checked={useGraph} onChange={(e) => setUseGraph(e.target.checked)} />
            S'appuyer sur le graphe
          </label>
          <label className="field-box">
            <input type="checkbox" checked={drafts} onChange={(e) => setDrafts(e.target.checked)} />
            Inclure les brouillons
          </label>
          <label className="field-box" title="Similarité minimale entre la question et un passage">
            Seuil de pertinence
            <select value={minScore ?? ""} onChange={(e) => setMinScore(e.target.value === "" ? null : Number(e.target.value))}>
              <option value="">par défaut</option>
              <option value={0.85}>0,85 (strict)</option>
              <option value={0.8}>0,80</option>
              <option value={0.75}>0,75 (large)</option>
              <option value={0}>aucun</option>
            </select>
          </label>
          {tags.length > 0 && (
            <label className="field-box">
              Tag
              <select value={tag} onChange={(e) => setTag(e.target.value)}>
                <option value="">Tous</option>
                {tags.map((t) => <option key={t.tag} value={t.tag}>{t.tag}</option>)}
              </select>
            </label>
          )}
          {result && <span className="muted">{plural(result.chunks.length, "passage", "passages")} en {result.millis} ms</span>}
        </div>
      </form>

      {error && <div className="error-banner" role="alert">{error}</div>}

      {result && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 24, alignItems: "flex-start" }}>
          <section aria-label="Passages retenus" style={{ flex: "999 1 460px", minWidth: 0, display: "flex", flexDirection: "column", gap: 12 }}>
            {result.chunks.length === 0 && (
              <div className="panel empty">
                {result.best_similarity !== null ? (
                  <>
                    <p>
                      Aucun passage n'est assez pertinent : le meilleur atteint {fr2(result.best_similarity)}, sous le seuil de {fr2(result.min_score)}.
                      La base ne couvre probablement pas cette question.
                    </p>
                    {result.min_score > 0.7 && (
                      <button type="button" className="btn" onClick={() => { setMinScore(0.7); run(undefined, 0.7); }}>
                        Chercher quand même avec un seuil de 0,70
                      </button>
                    )}
                  </>
                ) : (
                  <p>Aucun passage à chercher. {drafts ? "" : "Les brouillons sont exclus : cochez « Inclure les brouillons » pour les interroger."}</p>
                )}
              </div>
            )}
            {result.chunks.length > 0 && result.below_threshold > 0 && (
              <p className="muted" style={{ fontSize: 14 }}>
                {plural(result.below_threshold, "autre passage écarté", "autres passages écartés")} sous le seuil de pertinence ({fr2(result.min_score)}).
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
                      {c.doc_title}, {c.page ? `page ${c.page}` : `passage ${c.idx + 1}`}
                    </a>
                    <div className="meters">
                      {c.doc_status === "DRAFT" && <span className={`status status-DRAFT`}>{STATUS_LABEL.DRAFT}</span>}
                      <span className={`score score-${relevance(c.similarity).tone}`} title="Similarité cosinus entre la question et le passage (0 à 1)">
                        Pertinence {fr2(c.similarity)}, {relevance(c.similarity).label}
                      </span>
                      {c.graph_boost > 0.001 && (
                        <span className="score score-graph" title="Bonus apporté par les entités de la question (jusqu'à +0,05)">
                          graphe +{fr2(c.graph_boost)}
                        </span>
                      )}
                    </div>
                  </div>
                  <p style={{ color: "var(--ink-2)" }}>
                    {splitHighlights(c.text, highlight).map((part, j) =>
                      part.hit ? <mark key={j}>{part.text}</mark> : <span key={j}>{part.text}</span>,
                    )}
                  </p>
                  {c.via_graph && (
                    <p className="graph-found">Trouvé grâce au graphe : la recherche par le sens seul ne l'aurait pas remonté.</p>
                  )}
                </div>
              </article>
            ))}
            <details className="panel" style={{ padding: "0 18px" }}>
              <summary style={{ minHeight: 48, display: "flex", alignItems: "center", cursor: "pointer", fontWeight: 600 }}>
                Contexte transmis à l'agent (≈ {Math.ceil(result.context.length / 4).toLocaleString("fr-FR")} tokens)
              </summary>
              <pre className="code" style={{ marginBottom: 18 }}>{result.context}</pre>
            </details>
          </section>

          {result.entities.length > 0 && (
            <aside className="panel" aria-label="Entités mobilisées" style={{ flex: "1 1 300px", minWidth: 0, display: "flex", flexDirection: "column", gap: 16, padding: 20 }}>
              <h2>Ce que le graphe a apporté</h2>
              {VIA.map(({ key, title, className }) => {
                const list = result.entities.filter((e) => e.via === key);
                if (!list.length) return null;
                return (
                  <div key={key} style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                    <h3 style={{ fontSize: 14, fontFamily: "var(--font)", color: "var(--ink-soft)" }}>{title}</h3>
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
            </aside>
          )}
        </div>
      )}
    </section>
  );
}
