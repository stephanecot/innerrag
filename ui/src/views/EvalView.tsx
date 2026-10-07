import { useEffect, useMemo, useState } from "react";
import { api, type DocumentSummary, type EvalReport, type EvalSet } from "../api";
import { translate, useT } from "../i18n";
import { num2 } from "../util";

const SWEEP = [0.75, 0.78, 0.8, 0.82, 0.85];
const pct = (x: number) => translate("common.percent", { n: Math.round(x * 100) });

export default function EvalView({ project }: { project: string }) {
  const p = useMemo(() => api.project(project), [project]);
  const t = useT();
  const [set, setSet] = useState<EvalSet>({ questions: [] });
  const [docs, setDocs] = useState<DocumentSummary[]>([]);
  const [report, setReport] = useState<EvalReport | null>(null);
  const [k, setK] = useState(8);
  const [sweep, setSweep] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  // New question form
  const [question, setQuestion] = useState("");
  const [kind, setKind] = useState<"answer" | "none">("answer");
  const [doc, setDoc] = useState("");
  const [pages, setPages] = useState("");
  const [contains, setContains] = useState("");

  useEffect(() => {
    p.eval().then(setSet).catch((e) => setError((e as Error).message));
    p.documents().then(setDocs).catch(() => setDocs([]));
  }, [p]);

  const titles = useMemo(() => new Map(docs.map((d) => [d.id, d.title])), [docs]);

  const save = async (next: EvalSet) => {
    try {
      setSet(await p.saveEval(next));
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const add = (e: React.FormEvent) => {
    e.preventDefault();
    const expect =
      kind === "none"
        ? { none: true }
        : {
            doc: doc || undefined,
            pages: pages.split(/[,\s]+/).map(Number).filter((n) => n > 0),
            contains: contains.trim() ? [contains.trim()] : [],
          };
    save({ questions: [...set.questions, { id: "", question, expect }] });
    setQuestion("");
    setPages("");
    setContains("");
  };

  const run = async () => {
    setBusy(true);
    setError("");
    try {
      setReport(await p.runEval({ k, min_scores: sweep ? SWEEP : [] }));
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="page" aria-labelledby="eval-title">
      <div className="page-head">
        <div>
          <h1 id="eval-title">{t("eval.title")}</h1>
          <p>{t.rich("eval.intro", { file: <span className="mono">eval.json</span> })}</p>
        </div>
      </div>
      {error && <div className="error-banner" role="alert">{error}</div>}

      <div className="panel" style={{ padding: 20, display: "flex", flexDirection: "column", gap: 14 }}>
        <div className="toolbar" style={{ justifyContent: "space-between" }}>
          <h2>{set.questions.length ? t("eval.count", { n: set.questions.length }) : t("eval.none")}</h2>
          <div className="toolbar">
            <label className="field-box">
              {t("eval.passages")}
              <select value={k} onChange={(e) => setK(Number(e.target.value))}>
                {[3, 5, 8, 12].map((n) => <option key={n} value={n}>{n}</option>)}
              </select>
            </label>
            <label className="field-box">
              <input type="checkbox" checked={sweep} onChange={(e) => setSweep(e.target.checked)} />
              {t("eval.sweep")}
            </label>
            <button type="button" className="btn btn-primary" disabled={busy || !set.questions.length} onClick={run}>
              {busy ? t("eval.running") : t("eval.run")}
            </button>
          </div>
        </div>
        {set.questions.length > 0 && (
          <div className="table-wrap">
            <table>
              <thead>
                <tr><th scope="col">{t("eval.colQuestion")}</th><th scope="col">{t("eval.colExpected")}</th><th scope="col"><span className="sr-only">{t("eval.colActions")}</span></th></tr>
              </thead>
              <tbody>
                {set.questions.map((q) => (
                  <tr key={q.id}>
                    <td>{q.question}</td>
                    <td className="muted">
                      {q.expect.none
                        ? t("eval.offTopic")
                        : [
                            q.expect.doc && (titles.get(q.expect.doc) ?? q.expect.doc),
                            q.expect.pages?.length ? t("eval.pagesShort", { pages: q.expect.pages.join(", ") }) : "",
                            q.expect.contains?.length
                              ? t("eval.contains", { list: q.expect.contains.map((c) => t("common.quoted", { text: c })).join(", ") })
                              : "",
                          ].filter(Boolean).join(", ") || t("eval.anyPassage")}
                    </td>
                    <td className="num">
                      <button type="button" className="btn btn-danger" onClick={() => save({ questions: set.questions.filter((x) => x.id !== q.id) })}>
                        {t("eval.remove")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <form onSubmit={add} style={{ display: "flex", flexDirection: "column", gap: 10, borderTop: "1px solid var(--rule-soft)", paddingTop: 14 }}>
          <h3>{t("eval.add")}</h3>
          <p className="muted" style={{ fontSize: 14 }}>{t("eval.tip")}</p>
          <div className="field">
            <label htmlFor="eval-q">{t("eval.question")}</label>
            <input id="eval-q" className="input" required value={question} onChange={(e) => setQuestion(e.target.value)} />
          </div>
          <div className="segmented" role="group" aria-label={t("eval.kindAria")} style={{ alignSelf: "flex-start" }}>
            <button type="button" aria-pressed={kind === "answer"} onClick={() => setKind("answer")}>{t("eval.mustAnswer")}</button>
            <button type="button" aria-pressed={kind === "none"} onClick={() => setKind("none")}>{t("eval.offTopicKind")}</button>
          </div>
          {kind === "answer" && (
            <div className="toolbar" style={{ alignItems: "flex-start" }}>
              <div className="field" style={{ flex: "2 1 240px" }}>
                <label htmlFor="eval-doc">{t("eval.document")}</label>
                <select id="eval-doc" className="input" value={doc} onChange={(e) => setDoc(e.target.value)}>
                  <option value="">{t("eval.anyDoc")}</option>
                  {docs.map((d) => <option key={d.id} value={d.id}>{d.title}</option>)}
                </select>
              </div>
              <div className="field" style={{ flex: "1 1 120px" }}>
                <label htmlFor="eval-pages">{t("eval.pages")}</label>
                <input id="eval-pages" className="input" value={pages} placeholder="41, 42" onChange={(e) => setPages(e.target.value)} />
              </div>
              <div className="field" style={{ flex: "2 1 200px" }}>
                <label htmlFor="eval-contains">{t("eval.containsLabel")}</label>
                <input id="eval-contains" className="input" value={contains} placeholder="inputType" onChange={(e) => setContains(e.target.value)} />
              </div>
            </div>
          )}
          <button type="submit" className="btn" style={{ alignSelf: "flex-start" }} disabled={!question.trim()}>{t("eval.submit")}</button>
        </form>
      </div>

      {report && (
        <>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">{t("eval.colThreshold")}</th>
                  <th scope="col" className="num">{t("eval.colTop1")}</th>
                  <th scope="col" className="num">{t("eval.colTop3")}</th>
                  <th scope="col" className="num">{t("eval.colTopK", { k: String(report.k) })}</th>
                  <th scope="col" className="num">{t("eval.colMrr")}</th>
                  <th scope="col" className="num">{t("eval.colRejected")}</th>
                  <th scope="col" className="num">{t("eval.colOverall")}</th>
                  <th scope="col" className="num">{t("eval.colDuration")}</th>
                </tr>
              </thead>
              <tbody>
                {report.runs.map((r, i) => (
                  <tr key={r.min_score} className={i === report.best && report.runs.length > 1 ? "selected" : ""}>
                    <td>{num2(r.min_score)}{i === report.best && report.runs.length > 1 ? t("eval.best") : ""}</td>
                    <td className="num">{pct(r.hit_at_1)}</td>
                    <td className="num">{pct(r.hit_at_3)}</td>
                    <td className="num">{pct(r.hit_at_k)}</td>
                    <td className="num">{num2(r.mrr)}</td>
                    <td className="num">{r.rejected_out_of_scope === null ? "—" : pct(r.rejected_out_of_scope)}</td>
                    <td className="num" style={{ fontWeight: 600 }}>{pct(r.overall)}</td>
                    <td className="num">{Math.round(r.avg_millis)} ms</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <h2>{t("eval.detail", { n: num2(report.runs[report.best].min_score) })}</h2>
          <div className="table-wrap">
            <table>
              <thead>
                <tr><th scope="col">{t("eval.colQuestion")}</th><th scope="col">{t("eval.colResult")}</th><th scope="col">{t("eval.colTop")}</th></tr>
              </thead>
              <tbody>
                {report.results.map((r) => (
                  <tr key={r.id} className={r.correct ? "" : "error-row"}>
                    <td>{r.question}</td>
                    <td style={{ whiteSpace: "nowrap" }}>
                      {r.expects_none
                        ? r.correct ? t("eval.rejectedOk") : t("eval.wrongReturned", { n: r.returned })
                        : r.rank ? t("eval.foundAt", { n: r.rank }) : t("eval.notFound")}
                      {r.best_similarity !== null && <div className="muted" style={{ fontSize: 13 }}>{t("common.bestSim", { n: num2(r.best_similarity) })}</div>}
                    </td>
                    <td>
                      {r.top.map((top, i) => (
                        <div key={i} style={{ fontSize: 13 }} className={top.right ? "" : "muted"}>
                          {top.right ? "✓ " : ""}{top.doc_title}{top.page ? `, ${t("eval.pagesShort", { pages: String(top.page) })}` : ""} ({num2(top.score)})
                        </div>
                      ))}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </section>
  );
}
