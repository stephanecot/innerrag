import { useCallback, useEffect, useState } from "react";
import { api, type HistoryView as History, type Project } from "../api";
import { locale, translate, useT, type Key } from "../i18n";
import { channelLabel, num, operationLabel, OPERATIONS } from "../util";

const PERIODS: { hours: number; label: Key; n: number }[] = [
  { hours: 1, label: "history.hours", n: 1 },
  { hours: 24, label: "history.hours", n: 24 },
  { hours: 24 * 7, label: "history.days", n: 7 },
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
  const [confirmClear, setConfirmClear] = useState(false);
  const [notice, setNotice] = useState("");
  const t = useT();

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
    const timer = setInterval(load, 15_000);
    return () => clearInterval(timer);
  }, [load]);

  const peak = Math.max(1, ...(data?.series.map((b) => b.mcp_tokens + b.other_tokens) ?? [1]));
  const timeFmt = (at: number, withDay: boolean) =>
    new Date(at).toLocaleString(locale(), withDay ? { weekday: "short", hour: "2-digit", minute: "2-digit" } : { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const series = data?.series ?? [];
  const axis = series.length ? [0, Math.floor(series.length / 2), series.length - 1] : [];

  return (
    <section className="page" aria-labelledby="history-title">
      <div className="page-head">
        <div>
          <h1 id="history-title">{t("history.title")}</h1>
          <p>{t("history.intro")}</p>
        </div>
        <div className="segmented" role="group" aria-label={t("history.period")}>
          {PERIODS.map((p) => (
            <button key={p.hours} type="button" aria-pressed={hours === p.hours} onClick={() => setHours(p.hours)}>{t(p.label, { n: String(p.n) })}</button>
          ))}
        </div>
      </div>

      {error && <div className="error-banner" role="alert">{error}</div>}
      {notice && <div className="success" role="status">{notice}</div>}

      {data && (
        <section className="panel" aria-label={t("history.usage")} style={{ padding: "20px 24px", display: "flex", flexDirection: "column", gap: 18 }}>
          <dl className="totals">
            <div><dt>{t("history.calls")}</dt><dd>{num(data.totals.calls)}</dd></div>
            <div><dt>{t("history.contextTokens")}</dt><dd>≈ {num(data.totals.context_tokens)}</dd></div>
            <div><dt>{t("history.median")}</dt><dd>{num(data.totals.median_ms)} ms</dd></div>
            <div><dt>{t("history.errors")}</dt><dd style={{ color: data.totals.errors ? "var(--danger)" : undefined }}>{num(data.totals.errors)}</dd></div>
          </dl>
          <figure style={{ margin: 0, display: "flex", flexDirection: "column", gap: 8 }}>
            <figcaption className="muted" style={{ fontSize: 14 }}>
              {t("history.chartCaption", { unit: t(hours <= 48 ? "history.hour" : "history.day") })}
            </figcaption>
            <div className="bars" role="img" aria-label={t("history.chartAria", { unit: t(hours <= 48 ? "history.hour" : "history.day") })}>
              {series.map((b) => (
                <div key={b.at} title={t("history.barTip", { time: timeFmt(b.at, true), calls: b.calls, tokens: b.mcp_tokens + b.other_tokens })}>
                  <div className="seg-other" style={{ height: `${(b.other_tokens / peak) * 100}%` }} />
                  <div className="seg-mcp" style={{ height: `${(b.mcp_tokens / peak) * 100}%` }} />
                </div>
              ))}
            </div>
            <div className="bars-axis">
              {axis.map((i, n) => <span key={n}>{n === axis.length - 1 ? t("history.now") : timeFmt(series[i].at, true)}</span>)}
            </div>
          </figure>
        </section>
      )}

      <div className="toolbar">
        <label className="field-box">
          {t("history.channel")}
          <select value={channel} onChange={(e) => setChannel(e.target.value)}>
            <option value="">{t("history.all")}</option>
            {["mcp", "rest", "ui"].map((c) => <option key={c} value={c}>{channelLabel(c)}</option>)}
          </select>
        </label>
        <label className="field-box">
          {t("history.operation")}
          <select value={operation} onChange={(e) => setOperation(e.target.value)}>
            <option value="">{t("history.allOps")}</option>
            {OPERATIONS.map((op) => <option key={op} value={op}>{operationLabel(op)}</option>)}
          </select>
        </label>
        <label className="field-box">
          {t("history.project")}
          <select value={project} onChange={(e) => setProject(e.target.value)}>
            <option value="">{t("history.all")}</option>
            {projects.map((p) => <option key={p.id} value={p.id}>{p.id}</option>)}
          </select>
        </label>
        <label className="field-box">
          <input type="checkbox" checked={errors} onChange={(e) => setErrors(e.target.checked)} />
          {t("history.errorsOnly")}
        </label>
        <span style={{ flex: 1 }} />
        {confirmClear ? (
          <>
            <span className="danger-text" style={{ fontSize: 14 }}>
              {project ? t("history.confirmProject", { project }) : t("history.confirmAll")}
            </span>
            <button type="button" className="btn" onClick={() => setConfirmClear(false)}>{t("common.cancel")}</button>
            <button
              type="button"
              className="btn btn-danger-solid"
              onClick={async () => {
                try {
                  const r = await api.clearHistory(project);
                  setNotice(translate("history.cleared", { n: r.removed }));
                  setConfirmClear(false);
                  setOffset(0);
                  load();
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            >
              {t("history.clear")}
            </button>
          </>
        ) : (
          <button type="button" className="btn btn-danger" onClick={() => { setNotice(""); setConfirmClear(true); }}>
            {project ? t("history.clearProject") : t("history.clearAll")}
          </button>
        )}
      </div>

      {data && data.totals.calls > PAGE && (
        <Pager offset={offset} total={data.totals.calls} page={PAGE} onChange={setOffset} />
      )}
      {data && data.calls.length === 0 ? (
        <div className="panel empty"><p>{t("history.empty")}</p></div>
      ) : (
        <div className="table-wrap">
          <table style={{ minWidth: 880 }}>
            <thead>
              <tr>
                <th scope="col">{t("history.colTime")}</th>
                <th scope="col">{t("history.channel")}</th>
                <th scope="col">{t("history.operation")}</th>
                {!project && <th scope="col">{t("history.project")}</th>}
                <th scope="col">{t("history.colDetail")}</th>
                <th scope="col" className="num">{t("history.colResult")}</th>
                <th scope="col" className="num">{t("history.colContext")}</th>
                <th scope="col" className="num">{t("history.colDuration")}</th>
              </tr>
            </thead>
            <tbody>
              {data?.calls.map((c, i) => (
                <tr key={`${c.at}-${i}`} className={c.ok ? "" : "error-row"}>
                  <td className="muted">{timeFmt(c.at, hours > 24)}</td>
                  <td><span className={`channel channel-${c.channel}`}>{channelLabel(c.channel)}</span></td>
                  <td>{operationLabel(c.operation)}</td>
                  {!project && <td>{c.project}</td>}
                  <td style={{ maxWidth: 360 }} className={c.ok ? "" : "danger-text"}>{c.ok ? c.detail : c.error ?? c.detail}</td>
                  <td className={`num${c.ok ? "" : " danger-text"}`} style={{ fontWeight: c.ok ? 400 : 600 }}>{c.ok ? c.result : t("history.error")}</td>
                  <td className="num" style={{ fontWeight: 600 }}>{c.context_tokens ? t("history.tokens", { n: c.context_tokens }) : "—"}</td>
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
  const t = useT();
  return (
    <nav className="toolbar" aria-label={t("history.pagerAria")} style={{ justifyContent: "space-between" }}>
      <div className="toolbar">
        <button type="button" className="btn" disabled={offset === 0} onClick={() => onChange(0)}>{t("history.newest")}</button>
        <button type="button" className="btn" disabled={offset === 0} onClick={() => onChange(Math.max(0, offset - page))}>{t("history.prev")}</button>
      </div>
      <span className="muted">
        {t("history.range", { from: offset + 1, to: Math.min(offset + page, total), total })}
      </span>
      <div className="toolbar">
        <button type="button" className="btn" disabled={offset + page >= total} onClick={() => onChange(offset + page)}>{t("history.next")}</button>
        <button type="button" className="btn" disabled={offset >= last} onClick={() => onChange(last)}>{t("history.oldest")}</button>
      </div>
    </nav>
  );
}
