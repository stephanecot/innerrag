import { useEffect, useMemo, useState } from "react";
import { api, type DocumentSummary, type EvalReport, type EvalSet } from "../api";
import { fr2, num } from "../util";

const SWEEP = [0.75, 0.78, 0.8, 0.82, 0.85];
const pct = (x: number) => `${Math.round(x * 100)} %`;

export default function EvalView({ project }: { project: string }) {
  const p = useMemo(() => api.project(project), [project]);
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
          <h1 id="eval-title">Évaluation</h1>
          <p>
            Des questions de référence et la réponse attendue, pour mesurer la recherche et régler le seuil sur des
            chiffres. Le jeu est enregistré avec le projet (<span className="mono">eval.json</span>), donc partagé via git.
          </p>
        </div>
      </div>
      {error && <div className="error-banner" role="alert">{error}</div>}

      <div className="panel" style={{ padding: 20, display: "flex", flexDirection: "column", gap: 14 }}>
        <div className="toolbar" style={{ justifyContent: "space-between" }}>
          <h2>{set.questions.length ? `${num(set.questions.length)} questions de référence` : "Aucune question pour l'instant"}</h2>
          <div className="toolbar">
            <label className="field-box">
              Passages
              <select value={k} onChange={(e) => setK(Number(e.target.value))}>
                {[3, 5, 8, 12].map((n) => <option key={n} value={n}>{n}</option>)}
              </select>
            </label>
            <label className="field-box">
              <input type="checkbox" checked={sweep} onChange={(e) => setSweep(e.target.checked)} />
              Comparer les seuils
            </label>
            <button type="button" className="btn btn-primary" disabled={busy || !set.questions.length} onClick={run}>
              {busy ? "Évaluation…" : "Lancer l'évaluation"}
            </button>
          </div>
        </div>
        {set.questions.length > 0 && (
          <div className="table-wrap">
            <table>
              <thead>
                <tr><th scope="col">Question</th><th scope="col">Réponse attendue</th><th scope="col"><span className="sr-only">Actions</span></th></tr>
              </thead>
              <tbody>
                {set.questions.map((q) => (
                  <tr key={q.id}>
                    <td>{q.question}</td>
                    <td className="muted">
                      {q.expect.none
                        ? "aucune : question hors sujet"
                        : [
                            q.expect.doc && (titles.get(q.expect.doc) ?? q.expect.doc),
                            q.expect.pages?.length ? `p. ${q.expect.pages.join(", ")}` : "",
                            q.expect.contains?.length ? `contient « ${q.expect.contains.join(" », « ")} »` : "",
                          ].filter(Boolean).join(", ") || "n'importe quel passage"}
                    </td>
                    <td className="num">
                      <button type="button" className="btn btn-danger" onClick={() => save({ questions: set.questions.filter((x) => x.id !== q.id) })}>
                        Retirer
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <form onSubmit={add} style={{ display: "flex", flexDirection: "column", gap: 10, borderTop: "1px solid var(--rule-soft)", paddingTop: 14 }}>
          <h3>Ajouter une question</h3>
          <p className="muted" style={{ fontSize: 14 }}>Astuce : depuis la page Recherche, le bouton « Bonne réponse » d'un résultat l'ajoute directement.</p>
          <div className="field">
            <label htmlFor="eval-q">Question</label>
            <input id="eval-q" className="input" required value={question} onChange={(e) => setQuestion(e.target.value)} />
          </div>
          <div className="segmented" role="group" aria-label="Type de question" style={{ alignSelf: "flex-start" }}>
            <button type="button" aria-pressed={kind === "answer"} onClick={() => setKind("answer")}>La base doit répondre</button>
            <button type="button" aria-pressed={kind === "none"} onClick={() => setKind("none")}>Hors sujet</button>
          </div>
          {kind === "answer" && (
            <div className="toolbar" style={{ alignItems: "flex-start" }}>
              <div className="field" style={{ flex: "2 1 240px" }}>
                <label htmlFor="eval-doc">Document</label>
                <select id="eval-doc" className="input" value={doc} onChange={(e) => setDoc(e.target.value)}>
                  <option value="">n'importe lequel</option>
                  {docs.map((d) => <option key={d.id} value={d.id}>{d.title}</option>)}
                </select>
              </div>
              <div className="field" style={{ flex: "1 1 120px" }}>
                <label htmlFor="eval-pages">Pages</label>
                <input id="eval-pages" className="input" value={pages} placeholder="41, 42" onChange={(e) => setPages(e.target.value)} />
              </div>
              <div className="field" style={{ flex: "2 1 200px" }}>
                <label htmlFor="eval-contains">Le passage contient</label>
                <input id="eval-contains" className="input" value={contains} placeholder="inputType" onChange={(e) => setContains(e.target.value)} />
              </div>
            </div>
          )}
          <button type="submit" className="btn" style={{ alignSelf: "flex-start" }} disabled={!question.trim()}>Ajouter</button>
        </form>
      </div>

      {report && (
        <>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">Seuil</th>
                  <th scope="col" className="num">Trouvée en 1er</th>
                  <th scope="col" className="num">Dans les 3 premiers</th>
                  <th scope="col" className="num">Dans les {report.k} premiers</th>
                  <th scope="col" className="num">MRR</th>
                  <th scope="col" className="num">Hors sujet rejetées</th>
                  <th scope="col" className="num">Global</th>
                  <th scope="col" className="num">Durée</th>
                </tr>
              </thead>
              <tbody>
                {report.runs.map((r, i) => (
                  <tr key={r.min_score} className={i === report.best && report.runs.length > 1 ? "selected" : ""}>
                    <td>{fr2(r.min_score)}{i === report.best && report.runs.length > 1 ? " (meilleur)" : ""}</td>
                    <td className="num">{pct(r.hit_at_1)}</td>
                    <td className="num">{pct(r.hit_at_3)}</td>
                    <td className="num">{pct(r.hit_at_k)}</td>
                    <td className="num">{fr2(r.mrr)}</td>
                    <td className="num">{r.rejected_out_of_scope === null ? "—" : pct(r.rejected_out_of_scope)}</td>
                    <td className="num" style={{ fontWeight: 600 }}>{pct(r.overall)}</td>
                    <td className="num">{Math.round(r.avg_millis)} ms</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <h2>Détail au seuil {fr2(report.runs[report.best].min_score)}</h2>
          <div className="table-wrap">
            <table>
              <thead>
                <tr><th scope="col">Question</th><th scope="col">Résultat</th><th scope="col">Premiers passages</th></tr>
              </thead>
              <tbody>
                {report.results.map((r) => (
                  <tr key={r.id} className={r.correct ? "" : "error-row"}>
                    <td>{r.question}</td>
                    <td style={{ whiteSpace: "nowrap" }}>
                      {r.expects_none
                        ? r.correct ? "rejetée, comme attendu" : `${r.returned} passages renvoyés à tort`
                        : r.rank ? `trouvée au rang ${r.rank}` : "non trouvée"}
                      {r.best_similarity !== null && <div className="muted" style={{ fontSize: 13 }}>meilleure similarité {fr2(r.best_similarity)}</div>}
                    </td>
                    <td>
                      {r.top.map((t, i) => (
                        <div key={i} style={{ fontSize: 13 }} className={t.right ? "" : "muted"}>
                          {t.right ? "✓ " : ""}{t.doc_title}{t.page ? `, p. ${t.page}` : ""} ({fr2(t.score)})
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
