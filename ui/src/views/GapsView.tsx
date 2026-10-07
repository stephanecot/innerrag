import { useCallback, useEffect, useMemo, useState } from "react";
import { api, type FeedbackReport } from "../api";
import { href } from "../App";
import { locale, translate, useT } from "../i18n";
import { num, num2 } from "../util";

const when = (at: number) => new Date(at).toLocaleString(locale(), { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });

export default function GapsView({ project }: { project: string }) {
  const p = useMemo(() => api.project(project), [project]);
  const t = useT();
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
          <h1 id="gaps-title">{t("gaps.title")}</h1>
          <p>{t.rich("gaps.intro", { tool: <span className="mono">cite_sources</span> })}</p>
        </div>
        {report && (
          <dl className="totals gaps-totals">
            <div><dt>{t("gaps.searches")}</dt><dd>{num(report.searches)}</dd></div>
            <div><dt>{t("gaps.unanswered")}</dt><dd>{num(report.unanswered_searches)}</dd></div>
            <div><dt>{t("gaps.citations")}</dt><dd>{num(report.citations)}</dd></div>
          </dl>
        )}
      </div>

      {error && <div className="error-banner" role="alert">{error}</div>}
      {notice && <div className="success" role="status">{notice}</div>}

      {report && (
        <div className="gaps-layout">
          <section className="panel gaps-col" aria-labelledby="gaps-list-title">
            <div>
              <h2 id="gaps-list-title">{t("gaps.listTitle")}</h2>
              <p className="muted">{t("gaps.listIntro")}</p>
            </div>
            {report.gaps.length === 0 ? (
              <p className="empty-line">{t("gaps.none")}</p>
            ) : (
              <ul className="gaps-list">
                {report.gaps.map((g) => (
                  <li key={g.key}>
                    <div className="gaps-item-head">
                      <strong>{g.question}</strong>
                      <span className="gaps-count">{t("gaps.times", { n: g.count })}</span>
                    </div>
                    {g.variants.length > 0 && (
                      <p className="muted gaps-variants">{t("gaps.also", { list: g.variants.slice(0, 3).map((v) => t("common.quoted", { text: v })).join(", ") })}{g.variants.length > 3 ? "…" : ""}</p>
                    )}
                    <p className="gaps-meta">
                      {[
                        g.unanswered_searches > 0 && t("gaps.unansweredSearches", { n: g.unanswered_searches }),
                        g.reported > 0 && t("gaps.reports", { n: g.reported }),
                        g.best_similarity !== null && t("common.bestSim", { n: num2(g.best_similarity) }),
                        t("gaps.lastTime", { date: when(g.last_at) }),
                      ].filter(Boolean).join(", ")}
                    </p>
                    <div className="toolbar">
                      <button
                        type="button"
                        className="btn"
                        disabled={busy === g.key}
                        onClick={() => act(g.key, () => p.acceptFeedback(g.key, true), translate("gaps.keptOffTopic"))}
                      >
                        {t("gaps.keepOffTopic")}
                      </button>
                      <button
                        type="button"
                        className="btn"
                        disabled={busy === g.key}
                        onClick={() => act(g.key, () => p.dismissFeedback(g.key), translate("gaps.dismissedQuestion"))}
                      >
                        {t("gaps.dismiss")}
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="panel gaps-col" aria-labelledby="proposals-title">
            <div>
              <h2 id="proposals-title">{t("gaps.proposalsTitle")}</h2>
              <p className="muted">{t("gaps.proposalsIntro")}</p>
            </div>
            {report.proposals.length === 0 ? (
              <p className="empty-line">{t("gaps.noProposals")}</p>
            ) : (
              <ul className="gaps-list">
                {report.proposals.map((pr) => (
                  <li key={pr.key}>
                    <div className="gaps-item-head">
                      <strong>{pr.question}</strong>
                      {pr.partial && <span className="tool-kind writes">{t("gaps.partial")}</span>}
                    </div>
                    <ul className="gaps-passages">
                      {pr.passages.map((c) => (
                        <li key={c.id}>
                          <a href={href("lire", c.page ? { doc: c.doc_id, page: String(c.page) } : { doc: c.doc_id, tab: "passages" })}>
                            {c.doc_title}{c.page ? `, ${t("common.page", { n: String(c.page) })}` : ""}
                          </a>
                          <span className="muted"> {c.excerpt}…</span>
                        </li>
                      ))}
                    </ul>
                    <p className="gaps-meta">
                      {t("gaps.citationsLine", { citations: t("gaps.citationsCount", { n: pr.count }), date: when(pr.last_at) })}
                    </p>
                    <div className="toolbar">
                      <button
                        type="button"
                        className="btn btn-primary"
                        disabled={busy === pr.key}
                        onClick={() => act(pr.key, () => p.acceptFeedback(pr.key), translate("gaps.added"))}
                      >
                        {t("gaps.addToEval")}
                      </button>
                      <button
                        type="button"
                        className="btn"
                        disabled={busy === pr.key}
                        onClick={() => act(pr.key, () => p.dismissFeedback(pr.key), translate("gaps.dismissedAnswer"))}
                      >
                        {t("gaps.dismiss")}
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
