import { useCallback, useEffect, useState } from "react";
import { api, type HistoryView as History, type Project } from "../api";
import { CHANNEL_LABEL, num, OPERATION_LABEL } from "../util";

const PERIODS = [
  { hours: 1, label: "1 h" },
  { hours: 24, label: "24 h" },
  { hours: 24 * 7, label: "7 jours" },
];

export default function HistoryView({ projects, current }: { projects: Project[]; current: string }) {
  const [hours, setHours] = useState(24);
  const [channel, setChannel] = useState("");
  const [operation, setOperation] = useState("");
  const [project, setProject] = useState(current);
  const [errors, setErrors] = useState(false);
  const [offset, setOffset] = useState(0);
  const PAGE = 50;
  const [data, setData] = useState<History | null>(null);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    try {
      setData(await api.history({ hours, channel, operation, project, errors, offset, limit: PAGE }));
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }, [hours, channel, operation, project, errors, offset]);

  // A new filter starts again from the newest calls.
  useEffect(() => {
    setOffset(0);
  }, [hours, channel, operation, project, errors]);

  useEffect(() => {
    load();
    const t = setInterval(load, 15_000);
    return () => clearInterval(t);
  }, [load]);

  const peak = Math.max(1, ...(data?.series.map((b) => b.mcp_tokens + b.other_tokens) ?? [1]));
  const timeFmt = (at: number, withDay: boolean) =>
    new Date(at).toLocaleString("fr-FR", withDay ? { weekday: "short", hour: "2-digit", minute: "2-digit" } : { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const series = data?.series ?? [];
  const axis = series.length ? [0, Math.floor(series.length / 2), series.length - 1] : [];

  return (
    <section className="page" aria-labelledby="history-title">
      <div className="page-head">
        <div>
          <h1 id="history-title">Historique des appels</h1>
          <p>Chaque appel REST, MCP ou depuis cette interface. Le contexte renvoyé est ce que l'agent lit ensuite : c'est lui qui pèse sur sa facture de tokens.</p>
        </div>
        <div className="segmented" role="group" aria-label="Période">
          {PERIODS.map((p) => (
            <button key={p.hours} type="button" aria-pressed={hours === p.hours} onClick={() => setHours(p.hours)}>{p.label}</button>
          ))}
        </div>
      </div>

      {error && <div className="error-banner" role="alert">{error}</div>}

      {data && (
        <section className="panel" aria-label="Consommation" style={{ padding: "20px 24px", display: "flex", flexDirection: "column", gap: 18 }}>
          <dl className="totals">
            <div><dt>appels</dt><dd>{num(data.totals.calls)}</dd></div>
            <div><dt>tokens de contexte renvoyés</dt><dd>≈ {num(data.totals.context_tokens)}</dd></div>
            <div><dt>durée médiane</dt><dd>{num(data.totals.median_ms)} ms</dd></div>
            <div><dt>en erreur</dt><dd style={{ color: data.totals.errors ? "var(--danger)" : undefined }}>{num(data.totals.errors)}</dd></div>
          </dl>
          <figure style={{ margin: 0, display: "flex", flexDirection: "column", gap: 8 }}>
            <figcaption className="muted" style={{ fontSize: 14 }}>
              Tokens de contexte par {hours <= 48 ? "heure" : "jour"} : MCP en bleu, autres canaux en gris
            </figcaption>
            <div className="bars" role="img" aria-label={`Tokens de contexte par ${hours <= 48 ? "heure" : "jour"}`}>
              {series.map((b) => (
                <div key={b.at} title={`${timeFmt(b.at, true)} : ${num(b.calls)} appels, ≈ ${num(b.mcp_tokens + b.other_tokens)} tokens`}>
                  <div className="seg-other" style={{ height: `${(b.other_tokens / peak) * 100}%` }} />
                  <div className="seg-mcp" style={{ height: `${(b.mcp_tokens / peak) * 100}%` }} />
                </div>
              ))}
            </div>
            <div className="bars-axis">
              {axis.map((i, n) => <span key={n}>{n === axis.length - 1 ? "maintenant" : timeFmt(series[i].at, true)}</span>)}
            </div>
          </figure>
        </section>
      )}

      <div className="toolbar">
        <label className="field-box">
          Canal
          <select value={channel} onChange={(e) => setChannel(e.target.value)}>
            <option value="">Tous</option>
            <option value="mcp">MCP</option>
            <option value="rest">REST</option>
            <option value="ui">Interface</option>
          </select>
        </label>
        <label className="field-box">
          Opération
          <select value={operation} onChange={(e) => setOperation(e.target.value)}>
            <option value="">Toutes</option>
            {Object.entries(OPERATION_LABEL).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
          </select>
        </label>
        <label className="field-box">
          Projet
          <select value={project} onChange={(e) => setProject(e.target.value)}>
            <option value="">Tous</option>
            {projects.map((p) => <option key={p.id} value={p.id}>{p.id}</option>)}
          </select>
        </label>
        <label className="field-box">
          <input type="checkbox" checked={errors} onChange={(e) => setErrors(e.target.checked)} />
          Erreurs seulement
        </label>
      </div>

      {data && data.totals.calls > PAGE && (
        <Pager offset={offset} total={data.totals.calls} page={PAGE} onChange={setOffset} />
      )}
      {data && data.calls.length === 0 ? (
        <div className="panel empty"><p>Aucun appel sur cette période avec ces filtres.</p></div>
      ) : (
        <div className="table-wrap">
          <table style={{ minWidth: 880 }}>
            <thead>
              <tr>
                <th scope="col">Heure</th>
                <th scope="col">Canal</th>
                <th scope="col">Opération</th>
                {!project && <th scope="col">Projet</th>}
                <th scope="col">Détail</th>
                <th scope="col" className="num">Résultat</th>
                <th scope="col" className="num">Contexte</th>
                <th scope="col" className="num">Durée</th>
              </tr>
            </thead>
            <tbody>
              {data?.calls.map((c, i) => (
                <tr key={`${c.at}-${i}`} className={c.ok ? "" : "error-row"}>
                  <td className="muted">{timeFmt(c.at, hours > 24)}</td>
                  <td><span className={`channel channel-${c.channel}`}>{CHANNEL_LABEL[c.channel] ?? c.channel}</span></td>
                  <td>{OPERATION_LABEL[c.operation] ?? c.operation}</td>
                  {!project && <td>{c.project}</td>}
                  <td style={{ maxWidth: 360 }} className={c.ok ? "" : "danger-text"}>{c.ok ? c.detail : c.error ?? c.detail}</td>
                  <td className={`num${c.ok ? "" : " danger-text"}`} style={{ fontWeight: c.ok ? 400 : 600 }}>{c.ok ? c.result : "Erreur"}</td>
                  <td className="num" style={{ fontWeight: 600 }}>{c.context_tokens ? `≈ ${num(c.context_tokens)} tokens` : "—"}</td>
                  <td className="num">{num(c.duration_ms)} ms</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {data && data.totals.calls > PAGE && (
        <Pager offset={offset} total={data.totals.calls} page={PAGE} onChange={setOffset} />
      )}
    </section>
  );
}

function Pager({ offset, total, page, onChange }: { offset: number; total: number; page: number; onChange: (o: number) => void }) {
  const last = Math.max(0, Math.floor((total - 1) / page) * page);
  return (
    <nav className="toolbar" aria-label="Pages de l'historique" style={{ justifyContent: "space-between" }}>
      <div className="toolbar">
        <button type="button" className="btn" disabled={offset === 0} onClick={() => onChange(0)}>Plus récents</button>
        <button type="button" className="btn" disabled={offset === 0} onClick={() => onChange(Math.max(0, offset - page))}>← Précédents</button>
      </div>
      <span className="muted">
        {num(offset + 1)}–{num(Math.min(offset + page, total))} sur {num(total)} appels
      </span>
      <div className="toolbar">
        <button type="button" className="btn" disabled={offset + page >= total} onClick={() => onChange(offset + page)}>Suivants →</button>
        <button type="button" className="btn" disabled={offset >= last} onClick={() => onChange(last)}>Plus anciens</button>
      </div>
    </nav>
  );
}
