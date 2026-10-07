import { useCallback, useEffect, useMemo, useState } from "react";
import { api, type FeedbackReport } from "../api";
import { href } from "../App";
import { fr2, num, plural } from "../util";

const when = (at: number) => new Date(at).toLocaleString("fr-FR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });

export default function GapsView({ project }: { project: string }) {
  const p = useMemo(() => api.project(project), [project]);
  const [report, setReport] = useState<FeedbackReport | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState("");

  const load = useCallback(async () => {
    try {
      setReport(await p.feedback());
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }, [p]);

  useEffect(() => {
    load();
  }, [load]);

  const act = async (key: string, fn: () => Promise<unknown>, done: string) => {
    setBusy(key);
    try {
      await fn();
      setNotice(done);
      await load();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy("");
    }
  };

  return (
    <section className="page" aria-labelledby="gaps-title">
      <div className="page-head">
        <div>
          <h1 id="gaps-title">Lacunes</h1>
          <p>
            Ce que l'usage révèle de la base : les questions qu'elle ne couvre pas, et les réponses que les agents ont
            effectivement tirées de ses passages. Les agents le signalent avec l'outil MCP <span className="mono">cite_sources</span>.
          </p>
        </div>
        {report && (
          <dl className="totals gaps-totals">
            <div><dt>recherches</dt><dd>{num(report.searches)}</dd></div>
            <div><dt>sans passage pertinent</dt><dd>{num(report.unanswered_searches)}</dd></div>
            <div><dt>réponses citées</dt><dd>{num(report.citations)}</dd></div>
          </dl>
        )}
      </div>

      {error && <div className="error-banner" role="alert">{error}</div>}
      {notice && <div className="success" role="status">{notice}</div>}

      {report && (
        <div className="gaps-layout">
          <section className="panel gaps-col" aria-labelledby="gaps-list-title">
            <div>
              <h2 id="gaps-list-title">Questions sans réponse</h2>
              <p className="muted">
                Regroupées par sens, les plus fréquentes d'abord. À documenter, ou à garder comme questions hors sujet pour
                vérifier que la recherche ne répond rien.
              </p>
            </div>
            {report.gaps.length === 0 ? (
              <p className="empty-line">Aucune pour l'instant.</p>
            ) : (
              <ul className="gaps-list">
                {report.gaps.map((g) => (
                  <li key={g.key}>
                    <div className="gaps-item-head">
                      <strong>{g.question}</strong>
                      <span className="gaps-count">{plural(g.count, "fois", "fois")}</span>
                    </div>
                    {g.variants.length > 0 && (
                      <p className="muted gaps-variants">Aussi : {g.variants.slice(0, 3).map((v) => `« ${v} »`).join(", ")}{g.variants.length > 3 ? "…" : ""}</p>
                    )}
                    <p className="gaps-meta">
                      {[
                        g.unanswered_searches > 0 && plural(g.unanswered_searches, "recherche sans passage", "recherches sans passage"),
                        g.reported > 0 && `${plural(g.reported, "signalement", "signalements")} d'agent`,
                        g.best_similarity !== null && `meilleure similarité ${fr2(g.best_similarity)}`,
                        `dernière fois le ${when(g.last_at)}`,
                      ].filter(Boolean).join(", ")}
                    </p>
                    <div className="toolbar">
                      <button
                        type="button"
                        className="btn"
                        disabled={busy === g.key}
                        onClick={() => act(g.key, () => p.acceptFeedback(g.key, true), "Ajoutée au jeu d'évaluation comme question hors sujet.")}
                      >
                        Garder comme hors sujet
                      </button>
                      <button
                        type="button"
                        className="btn"
                        disabled={busy === g.key}
                        onClick={() => act(g.key, () => p.dismissFeedback(g.key), "Question écartée.")}
                      >
                        Écarter
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="panel gaps-col" aria-labelledby="proposals-title">
            <div>
              <h2 id="proposals-title">Réponses citées</h2>
              <p className="muted">
                Questions auxquelles un agent a répondu avec ces passages. Validez-les pour enrichir le jeu d'évaluation
                sans le rédiger à la main.
              </p>
            </div>
            {report.proposals.length === 0 ? (
              <p className="empty-line">Aucune réponse citée en attente.</p>
            ) : (
              <ul className="gaps-list">
                {report.proposals.map((pr) => (
                  <li key={pr.key}>
                    <div className="gaps-item-head">
                      <strong>{pr.question}</strong>
                      {pr.partial && <span className="tool-kind writes">réponse partielle</span>}
                    </div>
                    <ul className="gaps-passages">
                      {pr.passages.map((c) => (
                        <li key={c.id}>
                          <a href={href("lire", c.page ? { doc: c.doc_id, page: String(c.page) } : { doc: c.doc_id, tab: "passages" })}>
                            {c.doc_title}{c.page ? `, page ${c.page}` : ""}
                          </a>
                          <span className="muted"> {c.excerpt}…</span>
                        </li>
                      ))}
                    </ul>
                    <p className="gaps-meta">
                      {plural(pr.count, "citation", "citations")}, dernière le {when(pr.last_at)}
                    </p>
                    <div className="toolbar">
                      <button
                        type="button"
                        className="btn btn-primary"
                        disabled={busy === pr.key}
                        onClick={() => act(pr.key, () => p.acceptFeedback(pr.key), "Ajoutée au jeu d'évaluation.")}
                      >
                        Ajouter au jeu d'évaluation
                      </button>
                      <button
                        type="button"
                        className="btn"
                        disabled={busy === pr.key}
                        onClick={() => act(pr.key, () => p.dismissFeedback(pr.key), "Réponse écartée.")}
                      >
                        Écarter
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>
      )}
    </section>
  );
}
