import { useCallback, useEffect, useMemo, useState } from "react";
import { api, type CodeDrift, type CodeKind, type DocSymbol, type DocsFor, type Project } from "../api";
import { href } from "../App";
import { useT, type Key } from "../i18n";
import { num } from "../util";

const kindLabel = (kind: CodeKind): Key => `code.kinds.${kind}`;
const kindOne = (kind: CodeKind): Key => `code.kind.${kind}`;

const mentionHref = (m: DocSymbol["mentions"][number]) =>
  href("lire", m.page ? { doc: m.doc_id, page: String(m.page) } : { doc: m.doc_id, tab: "passages" });

export default function CodeView({ project, info, onChanged }: { project: string; info?: Project; onChanged: () => void }) {
  const p = useMemo(() => api.project(project), [project]);
  const [drift, setDrift] = useState<CodeDrift | null>(null);
  const [error, setError] = useState("");
  const [dir, setDir] = useState(info?.code_dir ?? "");
  const [saving, setSaving] = useState(false);
  const [target, setTarget] = useState("");
  const [found, setFound] = useState<DocsFor | null>(null);
  const [looking, setLooking] = useState(false);
  const t = useT();

  useEffect(() => setDir(info?.code_dir ?? ""), [info?.code_dir]);

  const load = useCallback(async () => {
    if (!info?.code_dir) {
      setDrift(null);
      return;
    }
    try {
      setDrift(await p.codeDrift());
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }, [p, info?.code_dir]);

  useEffect(() => {
    load();
  }, [load]);

  // Missing elements, grouped by the document that names them.
  const byDoc = useMemo(() => {
    const groups = new Map<string, { title: string; doc: string; items: DocSymbol[] }>();
    for (const s of drift?.missing ?? []) {
      const m = s.mentions[0];
      if (!m) continue;
      const g = groups.get(m.doc_id) ?? { title: m.doc_title, doc: m.doc_id, items: [] };
      g.items.push(s);
      groups.set(m.doc_id, g);
    }
    return [...groups.values()].sort((a, b) => b.items.length - a.items.length);
  }, [drift]);

  return (
    <section className="page" aria-labelledby="code-title">
      <div className="page-head">
        <div>
          <h1 id="code-title">{t("code.title")}</h1>
          <p>{t("code.intro")}</p>
        </div>
      </div>

      <form
        className="code-settings"
        onSubmit={async (e) => {
          e.preventDefault();
          setSaving(true);
          try {
            await api.updateProject(project, { code_dir: dir.trim() });
            setError("");
            onChanged();
          } catch (err) {
            setError((err as Error).message);
          } finally {
            setSaving(false);
          }
        }}
      >
        <label className="field-box" style={{ flex: "1 1 320px" }}>
          {t("code.repo")}
          <span className="muted mono">/watch/</span>
          <input type="text" value={dir} placeholder={t("code.repoPh")} onChange={(e) => setDir(e.target.value)} style={{ flex: 1 }} />
        </label>
        <button type="submit" className="btn btn-primary" disabled={saving}>{t("common.save")}</button>
        {drift && (
          <dl className="totals code-totals">
            <div><dt>{t("code.files")}</dt><dd>{num(drift.files)}</dd></div>
            <div><dt>{t("code.cited")}</dt><dd>{num(drift.symbols)}</dd></div>
            <div><dt>{t("code.missing")}</dt><dd style={{ color: drift.missing.length ? "var(--danger)" : undefined }}>{num(drift.missing.length)}</dd></div>
          </dl>
        )}
      </form>

      {error && <div className="error-banner" role="alert">{error}</div>}

      {!info?.code_dir ? (
        <div className="panel empty">
          <p>
            {t.rich("code.setup", {
              watch: <span className="mono">/watch</span>,
              example: <span className="mono">{`-v ./${t("common.exampleRepo")}:/watch/${t("common.exampleRepo")}:ro`}</span>,
            })}
          </p>
        </div>
      ) : (
        <div className="gaps-layout">
          <section className="panel gaps-col" aria-labelledby="drift-title">
            <div>
              <h2 id="drift-title">{t("code.missingTitle")}</h2>
              {drift && (
                <p className="muted">
                  {t("code.ratio", {
                    list: drift.by_kind.map(([k, n, f]) => t("code.ratioItem", { kind: t(kindLabel(k)), missing: String(n - f), cited: String(n) })).join(", "),
                  })}
                </p>
              )}
            </div>
            {drift && byDoc.length === 0 && <p className="empty-line">{t("code.allFound")}</p>}
            {byDoc.map((g) => (
              <div key={g.doc} className="drift-doc">
                <h3>{g.title} <span className="muted">({g.items.length})</span></h3>
                <ul className="drift-list">
                  {g.items.map((s) => (
                    <li key={`${s.kind}:${s.symbol}`}>
                      <code className="mono">{s.symbol}</code>
                      <span className="drift-kind">{t(kindOne(s.kind))}</span>
                      <span className="drift-where">
                        {s.mentions.slice(0, 4).map((m, i) => (
                          <a key={m.chunk_id} href={mentionHref(m)}>{m.page ? t("common.pageShort", { n: String(m.page) }) : t("common.passage", { n: String(i + 1) })}</a>
                        ))}
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            ))}
          </section>

          <section className="panel gaps-col" aria-labelledby="docsfor-title">
            <div>
              <h2 id="docsfor-title">{t("code.beforeTitle")}</h2>
              <p className="muted">{t.rich("code.beforeIntro", { tool: <span className="mono">docs_for</span> })}</p>
            </div>
            <form
              className="toolbar"
              onSubmit={async (e) => {
                e.preventDefault();
                if (!target.trim()) return;
                setLooking(true);
                try {
                  setFound(await p.docsFor(target.trim()));
                  setError("");
                } catch (err) {
                  setError((err as Error).message);
                } finally {
                  setLooking(false);
                }
              }}
            >
              <label className="field-box" style={{ flex: "1 1 260px" }}>
                <span className="sr-only">{t("code.target")}</span>
                <input type="text" value={target} placeholder="src/search.rs, search_knowledge…" onChange={(e) => setTarget(e.target.value)} style={{ flex: 1 }} />
              </label>
              <button type="submit" className="btn btn-primary" disabled={looking}>{t("code.find")}</button>
            </form>
            {found && (
              <div className="docs-for">
                {found.files.length > 0 && <p className="muted">{t("code.fileLine", { files: found.files.join(", ") })}</p>}
                {found.symbols.length === 0 ? (
                  <p className="empty-line">{t("code.noPassage", { target: found.target })}</p>
                ) : (
                  <ul className="drift-list">
                    {found.symbols.map((s) => (
                      <li key={`${s.kind}:${s.symbol}`}>
                        <code className="mono">{s.symbol}</code>
                        <span className={`drift-kind${s.found ? "" : " missing"}`}>{s.found ? t(kindOne(s.kind)) : t("code.absent")}</span>
                        <span className="drift-where">
                          {s.mentions.slice(0, 6).map((m, i) => (
                            <a key={m.chunk_id} href={mentionHref(m)}>{m.doc_title}{m.page ? `, ${t("common.pageShort", { n: String(m.page) })}` : i ? ` (${i + 1})` : ""}</a>
                          ))}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
                <p className="muted" style={{ fontSize: 13 }}>
                  {t("code.concerned", { n: found.symbols.reduce((n, s) => n + s.mentions.length, 0) })}.
                </p>
              </div>
            )}
          </section>
        </div>
      )}
    </section>
  );
}
