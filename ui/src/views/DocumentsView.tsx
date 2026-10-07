import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type DocumentSummary, type DocumentWithChunks, type FileInfo, type Job, type ServerConfig, type Status, type Stats } from "../api";
import { href } from "../App";
import { CloseIcon, DocTypeIcon, PlusIcon } from "../Icons";
import { translate, useT } from "../i18n";
import { bytes, docKind, docKindInfo, jobPercent, jobStage, longDate, num, relativeDate, statusLabel } from "../util";

const ACCEPT = ".pdf,.docx,.pptx,.doc,.ppt,.md,.markdown,.html,.htm,.txt";

type Panel = { kind: "none" } | { kind: "new" } | { kind: "doc"; id: string };

export default function DocumentsView({
  project, config, params,
}: { project: string; config: ServerConfig | null; params: URLSearchParams }) {
  const p = useMemo(() => api.project(project), [project]);
  const t = useT();
  const [docs, setDocs] = useState<DocumentSummary[]>([]);
  const [stats, setStats] = useState<Stats | null>(null);
  const [tags, setTags] = useState<{ tag: string; count: number }[]>([]);
  const [q, setQ] = useState("");
  const [status, setStatus] = useState<"" | Status>("");
  const [tag, setTag] = useState("");
  const [error, setError] = useState("");
  const [panel, setPanel] = useState<Panel>(() =>
    params.get("doc") ? { kind: "doc", id: params.get("doc")! } : params.get("new") ? { kind: "new" } : { kind: "none" },
  );

  const refresh = useCallback(async () => {
    try {
      const [d, s, t] = await Promise.all([p.documents({ q, status, tag }), p.stats(), p.tags()]);
      setDocs(d);
      setStats(s);
      setTags(t);
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }, [p, q, status, tag]);

  useEffect(() => {
    const t = setTimeout(refresh, q ? 200 : 0);
    return () => clearTimeout(t);
  }, [refresh, q]);

  // Ingestions run in the background: poll while one is queued or running.
  const [jobs, setJobs] = useState<Job[]>([]);
  const [dismissed, setDismissed] = useState<Set<string>>(new Set());
  const finished = useRef<Set<string> | null>(null);
  const loadJobs = useCallback(async () => {
    try {
      const list = await api.jobs(project);
      setJobs(list);
      const done = new Set(list.filter((j) => j.finished_at).map((j) => j.id));
      if (finished.current && [...done].some((id) => !finished.current!.has(id))) refresh();
      finished.current = done;
    } catch {
      /* the strip simply stays as it is */
    }
  }, [project, refresh]);
  useEffect(() => {
    loadJobs();
  }, [loadJobs]);
  const active = jobs.some((j) => !j.finished_at);
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(loadJobs, 1500);
    return () => clearInterval(timer);
  }, [active, loadJobs]);
  // Running ingestions are rows of the table; the strip reports the ones that just ended.
  const recent = jobs.filter((j) => !dismissed.has(j.id) && j.finished_at && Date.now() - j.finished_at < 30 * 60_000);
  const queued = (job: Job) => {
    setJobs((list) => [job, ...list.filter((j) => j.id !== job.id)]);
    window.dispatchEvent(new Event("innerrag:job-queued"));
  };
  // Documents being indexed show up in the list before they exist in the base.
  const pending = jobs.filter((j) => !j.finished_at && !docs.some((d) => d.id === j.document_id));

  // Escape closes the side panel.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPanel({ kind: "none" });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const selectedId = panel.kind === "doc" ? panel.id : null;

  return (
    <>
      <section className="page" aria-labelledby="docs-title">
        <div className="page-head">
          <div>
            <h1 id="docs-title">{t("docs.title")}</h1>
            <p>{t("docs.intro")}</p>
          </div>
          <button type="button" className="btn btn-primary" onClick={() => setPanel({ kind: "new" })}>
            <PlusIcon />
            {t("common.addDocument")}
          </button>
        </div>

        <div className="toolbar">
          <label htmlFor="doc-filter" className="sr-only">{t("docs.filter")}</label>
          <input
            id="doc-filter"
            className="input"
            type="search"
            placeholder={t("docs.filter")}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            style={{ flex: "1 1 220px", maxWidth: 320 }}
          />
          <div className="segmented" role="group" aria-label={t("docs.status")}>
            {([["", t("docs.all"), stats?.documents], ["PUBLISHED", t("docs.published"), stats?.published], ["DRAFT", t("docs.drafts"), stats?.drafts]] as const).map(
              ([value, label, count]) => (
                <button key={value} type="button" aria-pressed={status === value} onClick={() => setStatus(value)}>
                  {label} {count ?? ""}
                </button>
              ),
            )}
          </div>
          {tags.length > 0 && (
            <label className="field-box">
              {t("docs.tag")}
              <select value={tag} onChange={(e) => setTag(e.target.value)}>
                <option value="">{t("docs.all")}</option>
                {tags.map((tg) => <option key={tg.tag} value={tg.tag}>{tg.tag} ({tg.count})</option>)}
              </select>
            </label>
          )}
        </div>

        {error && <div className="error-banner" role="alert">{error}</div>}

        {recent.length > 0 && (
          <section className="panel jobs" aria-label={t("docs.jobsAria")}>
            {recent.slice(0, 6).map((j) => (
              <JobRow
                key={j.id}
                job={j}
                onOpen={() => setPanel({ kind: "doc", id: j.document_id })}
                onDismiss={() => setDismissed((d) => new Set(d).add(j.id))}
              />
            ))}
          </section>
        )}

        {docs.length === 0 && pending.length === 0 && !error ? (
          <div className="panel empty">
            <p>{q || status || tag ? t("docs.noMatch") : t("docs.empty")}</p>
            {!(q || status || tag) && (
              <button type="button" className="btn btn-primary" onClick={() => setPanel({ kind: "new" })}>{t("common.addDocument")}</button>
            )}
          </div>
        ) : (
          <div className="table-wrap">
            <table style={{ minWidth: 760 }}>
              <thead>
                <tr>
                  <th scope="col">{t("docs.colTitle")}</th>
                  <th scope="col">{t("docs.colStatus")}</th>
                  <th scope="col">{t("docs.colTags")}</th>
                  <th scope="col">{t("docs.colCreator")}</th>
                  <th scope="col" className="num">{t("docs.colPassages")}</th>
                  <th scope="col" className="num">{t("docs.colEntities")}</th>
                  <th scope="col">{t("docs.colUpdated")}</th>
                </tr>
              </thead>
              <tbody>
                {pending.map((j) => (
                  <tr key={j.id} className="pending-row">
                    <td>
                      <span className="title-with-type">
                        <DocTypeIcon kind={docKind(j.filename)} {...docKindInfo(docKind(j.filename))} />
                        <strong className="cell-title" title={j.filename}>{j.filename}</strong>
                      </span>
                    </td>
                    <td><span className="status status-INDEXING">{t("docs.indexing", { pct: t("common.percent", { n: jobPercent(j) }) })}</span></td>
                    <td colSpan={4} className="muted">
                      {jobStage(j.stage)}{j.total ? t("docs.progressPassages", { done: j.done, total: j.total }) : ""}
                    </td>
                    <td>
                      <button
                        type="button"
                        className="btn"
                        onClick={async () => {
                          await api.cancelJob(j.id).catch(() => undefined);
                          loadJobs();
                        }}
                      >
                        {t("common.cancel")}
                      </button>
                    </td>
                  </tr>
                ))}
                {docs.map((d) => (
                  <tr
                    key={d.id}
                    className={`selectable${d.id === selectedId ? " selected" : ""}`}
                    onClick={() => setPanel({ kind: "doc", id: d.id })}
                  >
                    <td>
                      <span className="title-with-type">
                        <DocTypeIcon kind={docKind(d.source)} {...docKindInfo(docKind(d.source))} />
                        <button type="button" className="row-title cell-title" title={d.title} onClick={() => setPanel({ kind: "doc", id: d.id })}>{d.title}</button>
                      </span>
                    </td>
                    <td><span className={`status status-${d.status}`}>{statusLabel(d.status)}</span></td>
                    <td>
                      <span style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                        {d.tags.map((tg) => <span key={tg} className="chip">{tg}</span>)}
                      </span>
                    </td>
                    <td>{d.creator}</td>
                    <td className="num">{num(d.chunks)}</td>
                    <td className="num">{num(d.entities)}</td>
                    <td className="muted">{relativeDate(d.updated_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {panel.kind === "new" && (
        <NewDocument
          key="new"
          project={project}
          config={config}
          onClose={() => setPanel({ kind: "none" })}
          onQueued={(job) => {
            queued(job);
            setPanel({ kind: "none" });
          }}
        />
      )}
      {panel.kind === "doc" && (
        <DocumentPanel
          key={panel.id}
          project={project}
          id={panel.id}
          job={jobs.find((j) => j.document_id === panel.id && !j.finished_at)}
          onQueued={queued}
          onChanged={refresh}
          onDeleted={() => {
            refresh();
            setPanel({ kind: "none" });
          }}
          onClose={() => setPanel({ kind: "none" })}
        />
      )}
    </>
  );
}

function TagsInput({ id, value, onChange }: { id: string; value: string[]; onChange: (tags: string[]) => void }) {
  const [draft, setDraft] = useState("");
  const t = useT();
  const add = (raw: string) => {
    const tag = raw.trim().replace(/,$/, "").trim();
    if (tag && !value.some((v) => v.toLowerCase() === tag.toLowerCase())) onChange([...value, tag]);
    setDraft("");
  };
  return (
    <div className="tags-input">
      {value.map((tag) => (
        <span key={tag} className="chip">
          {tag}
          <button type="button" aria-label={t("docs.removeTag", { tag })} onClick={() => onChange(value.filter((v) => v !== tag))}>×</button>
        </span>
      ))}
      <input
        id={id}
        type="text"
        placeholder={t("docs.addTag")}
        value={draft}
        onChange={(e) => (e.target.value.endsWith(",") ? add(e.target.value) : setDraft(e.target.value))}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            add(draft);
          } else if (e.key === "Backspace" && !draft && value.length) {
            onChange(value.slice(0, -1));
          }
        }}
        onBlur={() => draft && add(draft)}
      />
    </div>
  );
}

function StatusChoice({ value, onChange }: { value: Status; onChange: (s: Status) => void }) {
  const t = useT();
  return (
    <fieldset style={{ margin: 0, padding: 0, border: 0 }} className="field">
      <legend className="label" style={{ paddingBottom: 6 }}>{t("docs.status")}</legend>
      <div className="status-choice">
        {(["DRAFT", "PUBLISHED"] as const).map((s) => (
          <label key={s} className={value === s ? "on" : ""}>
            <input type="radio" name="status" checked={value === s} onChange={() => onChange(s)} />
            {statusLabel(s)}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

function JobRow({ job, onOpen, onDismiss }: { job: Job; onOpen: () => void; onDismiss: () => void }) {
  const pct = jobPercent(job);
  const running = !job.finished_at;
  const t = useT();
  return (
    <div className={`job-row${job.stage === "failed" ? " failed" : ""}`}>
      <div className="job-main">
        <div className="job-head">
          <strong>{job.filename}</strong>
          <span className="muted">
            {job.stage === "done" && job.report
              ? t("docs.report", {
                passages: t("common.passages", { n: job.report.chunks }),
                entities: t("common.entities", { n: job.report.entities }),
                s: job.report.millis < 1000 ? "< 1" : num(Math.round(job.report.millis / 1000)),
              })
              : job.stage === "failed" || job.stage === "cancelled"
                ? jobStage(job.stage)
                : `${jobStage(job.stage)}${job.total ? ` ${num(job.done)} / ${num(job.total)}` : ""}`}
          </span>
        </div>
        {running && (
          <div className="meter-track job-track" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label={t("docs.ingestionOf", { file: job.filename })}>
            <span className="meter-fill" style={{ display: "block", width: `${pct}%`, background: "var(--c-organization)" }} />
          </div>
        )}
        {job.error && <span className="danger-text" style={{ fontSize: 14 }}>{job.error}</span>}
      </div>
      {job.stage === "done" && <button type="button" className="btn" onClick={onOpen}>{t("common.open")}</button>}
      {!running && (
        <button type="button" className="icon-button" aria-label={t("docs.hideRow")} onClick={onDismiss}><CloseIcon /></button>
      )}
    </div>
  );
}

/** Drop zone + file picker for every supported format. */
function FileDrop({ file, onFile, maxMb }: { file: File | null; onFile: (f: File | null) => void; maxMb: number }) {
  const [over, setOver] = useState(false);
  const t = useT();
  if (file) {
    return (
      <div className="file-chosen">
        <span><strong>{file.name}</strong> <span className="muted">{bytes(file.size)}</span></span>
        <button type="button" className="btn" onClick={() => onFile(null)}>{t("docs.change")}</button>
      </div>
    );
  }
  return (
    <label
      className={`drop${over ? " over" : ""}`}
      onDragOver={(e) => {
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        const f = e.dataTransfer.files?.[0];
        if (f) onFile(f);
      }}
    >
      <strong>{t("docs.drop")}</strong>
      <span className="muted">{t("docs.formats", { mb: maxMb })}</span>
      <input
        type="file"
        accept={ACCEPT}
        className="sr-only"
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) onFile(f);
          e.target.value = "";
        }}
      />
    </label>
  );
}

function NewDocument({
  project, config, onClose, onQueued,
}: { project: string; config: ServerConfig | null; onClose: () => void; onQueued: (job: Job) => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [kind, setKind] = useState<"file" | "url" | "text">("file");
  const [url, setUrl] = useState("");
  const [title, setTitle] = useState("");
  const [id, setId] = useState("");
  const [source, setSource] = useState("");
  const [tags, setTags] = useState<string[]>([]);
  const [status, setStatus] = useState<Status>(config?.default_status ?? "PUBLISHED");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const maxMb = config?.max_upload_mb ?? 200;
  const t = useT();

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    try {
      const p = api.project(project);
      const job =
        kind === "url"
          ? await p.ingestUrl({ url: url.trim(), title: title || undefined, tags, status, id: id.trim() || undefined })
          : kind === "file" && file
            ? await p.uploadDocument(file, { title, tags: tags.join(","), status, id: id.trim(), source: source.trim() })
            : await p.createDocument({ title, text, tags, status, id: id.trim() || undefined, source: source.trim() || undefined });
      onQueued(job);
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const ready =
    kind === "file"
      ? !!file && file.size <= maxMb * 1024 * 1024
      : kind === "url"
        ? /^https?:\/\/\S+\.\S+/i.test(url.trim())
        : text.trim().length > 0;

  return (
    <aside className="side" aria-labelledby="new-title">
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <h2 id="new-title" style={{ fontSize: 28 }}>{t("docs.new")}</h2>
        <button type="button" className="btn" onClick={onClose}>{t("common.cancel")}</button>
      </div>
      <form onSubmit={submit} style={{ display: "flex", flexDirection: "column", gap: 14 }}>
        <div className="segmented" role="group" aria-label={t("docs.sourceKind")} style={{ alignSelf: "flex-start" }}>
          {(["file", "url", "text"] as const).map((k) => (
            <button key={k} type="button" aria-pressed={kind === k} onClick={() => setKind(k)}>
              {t(k === "file" ? "docs.kindFile" : k === "url" ? "docs.kindUrl" : "docs.kindText")}
            </button>
          ))}
        </div>
        {kind === "text" && (
          <div className="field">
            <label htmlFor="new-doc-text">{t("docs.text")}</label>
            <textarea id="new-doc-text" className="textarea" rows={10} required value={text} onChange={(e) => setText(e.target.value)} />
            <span className="hint">{t("docs.mdHint")}</span>
          </div>
        )}
        {kind === "url" && (
          <div className="field">
            <label htmlFor="new-doc-url">{t("docs.url")}</label>
            <input
              id="new-doc-url"
              className="input"
              type="url"
              inputMode="url"
              autoComplete="url"
              required
              placeholder="https://"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              aria-describedby="new-doc-url-hint"
            />
            <span id="new-doc-url-hint" className="hint">{t("docs.urlHint")}</span>
          </div>
        )}
        {kind === "file" && (
          <div className="field">
            <span className="label">{t("docs.file")}</span>
            <FileDrop file={file} onFile={setFile} maxMb={maxMb} />
            {file && file.size > maxMb * 1024 * 1024 && <span className="danger-text hint">{t("docs.tooBig", { mb: maxMb })}</span>}
          </div>
        )}
        <div className="field">
          <label htmlFor="new-doc-title">{t("docs.titleLabel")}</label>
          <input id="new-doc-title" className="input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t("docs.titlePh")} />
        </div>
        <StatusChoice value={status} onChange={setStatus} />
        <div className="field">
          <label htmlFor="new-doc-tags">{t("docs.tags")}</label>
          <TagsInput id="new-doc-tags" value={tags} onChange={setTags} />
        </div>
        <details>
          <summary style={{ minHeight: 40, cursor: "pointer" }}>{t("docs.idSource")}</summary>
          <div style={{ display: "flex", flexDirection: "column", gap: 12, paddingTop: 8 }}>
            <div className="field">
              <label htmlFor="new-doc-id">{t("docs.id")}</label>
              <input id="new-doc-id" className="input" value={id} onChange={(e) => setId(e.target.value)} placeholder={t("docs.idPh")} />
              <span className="hint">{t("docs.idHint")}</span>
            </div>
            <div className="field">
              <label htmlFor="new-doc-source">{t("docs.source")}</label>
              <input id="new-doc-source" className="input" value={source} onChange={(e) => setSource(e.target.value)} placeholder={t("docs.sourcePh")} />
            </div>
          </div>
        </details>
        {error && <div className="error-banner" role="alert">{error}</div>}
        <button type="submit" className="btn btn-primary" disabled={busy || !ready}>
          {busy ? t("docs.sending") : t("docs.addToProject")}
        </button>
        <p className="hint muted">{t("docs.bgHint")}</p>
      </form>
    </aside>
  );
}

function DocumentPanel({
  project, id, job, onQueued, onChanged, onDeleted, onClose,
}: {
  project: string;
  id: string;
  job?: Job;
  onQueued: (job: Job) => void;
  onChanged: () => void;
  onDeleted: () => void;
  onClose: () => void;
}) {
  const p = useMemo(() => api.project(project), [project]);
  const t = useT();
  const [doc, setDoc] = useState<DocumentWithChunks | null>(null);
  const [title, setTitle] = useState("");
  const [status, setStatus] = useState<Status>("PUBLISHED");
  const [tags, setTags] = useState<string[]>([]);
  const [source, setSource] = useState("");
  const [mode, setMode] = useState<"edit" | "replace" | "confirm-delete">("edit");
  const [newText, setNewText] = useState("");
  const [newFile, setNewFile] = useState<File | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");

  const fill = (d: DocumentWithChunks) => {
    setDoc(d);
    setTitle(d.title);
    setStatus(d.status);
    setTags(d.tags);
    setSource(d.source);
  };

  // Reload when a running ingestion of this document ends.
  const running = Boolean(job);
  useEffect(() => {
    if (running) return;
    p.document(id).then(fill).catch((e) => setError((e as Error).message));
  }, [p, id, running]);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await action();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  if (!doc) {
    return (
      <aside className="side" aria-label={t("docs.docAria")}>
        {job ? (
          <p className="muted">{t("docs.indexingNow", { stage: jobStage(job.stage).toLowerCase(), pct: t("common.percent", { n: jobPercent(job) }) })}</p>
        ) : error ? (
          <div className="error-banner" role="alert">{error}</div>
        ) : (
          <p className="muted">{t("common.loading")}</p>
        )}
        <button type="button" className="btn" onClick={onClose}>{t("common.close")}</button>
      </aside>
    );
  }

  const file = (doc.metadata as { file?: FileInfo } | null)?.file;
  const dirty = title !== doc.title || status !== doc.status || source !== doc.source || tags.join("\u0000") !== doc.tags.join("\u0000");

  return (
    <aside className="side" aria-labelledby="doc-title">
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <h2 id="doc-title" className="side-title title-with-type" style={{ fontSize: 26 }} title={doc.title}>
            <DocTypeIcon kind={docKind(doc.source)} {...docKindInfo(docKind(doc.source))} />
            <span>{doc.title}</span>
          </h2>
          <p className="muted wrap-anywhere" style={{ fontSize: 14 }}>
            {t("docs.meta", { id: doc.id, creator: doc.creator, date: longDate(doc.created_at) })}
          </p>
          {file && (
            <p className="muted" style={{ fontSize: 14 }}>
              {t("docs.imported", {
                name: file.name,
                format: file.format_label,
                pages: file.pages ? `, ${t(file.format === "pptx" || file.format === "ppt" ? "docs.slides" : "common.pages", { n: file.pages })}` : "",
                size: bytes(file.size),
              })}
            </p>
          )}
          {job && <p style={{ fontSize: 14 }}>{t("docs.reindexing", { stage: jobStage(job.stage).toLowerCase(), pct: t("common.percent", { n: jobPercent(job) }) })}</p>}
        </div>
        <button type="button" className="btn" onClick={onClose}>{t("common.close")}</button>
      </div>

      {mode === "replace" ? (
        <form
          style={{ display: "flex", flexDirection: "column", gap: 12 }}
          onSubmit={(e) => {
            e.preventDefault();
            run(async () => {
              const queuedJob = newFile
                ? await p.uploadReplace(doc.id, newFile, { title, tags: tags.join(","), status, source })
                : await p.replaceDocument(doc.id, { title, text: newText, tags, status, source });
              onQueued(queuedJob);
              setMode("edit");
              setNewText("");
              setNewFile(null);
              setMessage(translate("docs.replaceStarted"));
            });
          }}
        >
          <div className="field">
            <span className="label">{t("docs.newFile")}</span>
            <FileDrop file={newFile} onFile={setNewFile} maxMb={200} />
          </div>
          {!newFile && (
            <div className="field">
              <label htmlFor="replace-text">{t("docs.orNewText")}</label>
              <textarea id="replace-text" className="textarea" rows={8} value={newText} onChange={(e) => setNewText(e.target.value)} />
            </div>
          )}
          <span className="hint">{t("docs.replaceHint")}</span>
          <div className="toolbar">
            <button type="submit" className="btn btn-primary" disabled={busy || (!newFile && !newText.trim())}>
              {busy ? t("docs.sending") : t("docs.replaceReindex")}
            </button>
            <button type="button" className="btn" onClick={() => setMode("edit")}>{t("common.cancel")}</button>
          </div>
        </form>
      ) : (
        <form
          style={{ display: "flex", flexDirection: "column", gap: 14 }}
          onSubmit={(e) => {
            e.preventDefault();
            run(async () => {
              fill(await p.patchDocument(doc.id, { title, status, tags, source }));
              setMessage(translate("docs.saved"));
              onChanged();
            });
          }}
        >
          <div className="field">
            <label htmlFor="doc-edit-title">{t("docs.titleLabel")}</label>
            <input id="doc-edit-title" className="input" required value={title} onChange={(e) => setTitle(e.target.value)} />
          </div>
          <StatusChoice value={status} onChange={setStatus} />
          <div className="field">
            <label htmlFor="doc-edit-tags">{t("docs.tags")}</label>
            <TagsInput id="doc-edit-tags" value={tags} onChange={setTags} />
          </div>
          <div className="field">
            <label htmlFor="doc-edit-source">{t("docs.source")}</label>
            <input id="doc-edit-source" className="input" value={source} onChange={(e) => setSource(e.target.value)} />
          </div>
          {mode === "confirm-delete" ? (
            <div className="error-banner" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              <span>{t("docs.deleteWarn", { title: doc.title, passages: t("common.passages", { n: doc.passages.length }) })}</span>
              <div className="toolbar">
                <button
                  type="button"
                  className="btn btn-danger-solid"
                  disabled={busy}
                  onClick={() => run(async () => {
                    await p.deleteDocument(doc.id);
                    onDeleted();
                  })}
                >
                  {t("docs.deleteForever")}
                </button>
                <button type="button" className="btn" onClick={() => setMode("edit")}>{t("common.cancel")}</button>
              </div>
            </div>
          ) : (
            <div className="toolbar">
              <button type="submit" className="btn btn-primary" disabled={busy || !dirty}>{t("common.save")}</button>
              <button type="button" className="btn" onClick={() => setMode("replace")}>{t("docs.replaceText")}</button>
              <button type="button" className="btn btn-danger" onClick={() => setMode("confirm-delete")}>{t("common.delete")}</button>
            </div>
          )}
        </form>
      )}

      {message && <div className="success" role="status">{message}</div>}
      {error && <div className="error-banner" role="alert">{error}</div>}

      <section style={{ display: "flex", flexDirection: "column", gap: 10, borderTop: "1px solid var(--rule-soft)", paddingTop: 18 }}>
        <h3>{t("docs.read")}</h3>
        <div className="toolbar">
          <a className="btn btn-primary" href={href("lire", { doc: doc.id })}>{t("docs.readDoc")}</a>
          {file?.stored && (
            <a className="btn" href={p.fileUrl(doc.id)} target="_blank" rel="noopener">{t("docs.openOriginal", { format: file.format_label })}</a>
          )}
        </div>
      </section>

      <section style={{ display: "flex", flexDirection: "column", gap: 10, borderTop: "1px solid var(--rule-soft)", paddingTop: 18 }}>
        <h3>{t("docs.howIndexed")}</h3>
        <p className="muted" style={{ fontSize: 14 }}>
          {t("docs.howText", {
            passages: t("common.passages", { n: doc.passage_count }),
            entities: t("docs.entitiesFound", { n: doc.entities }),
          })}
        </p>
        <a className="btn" style={{ alignSelf: "flex-start" }} href={href("lire", { doc: doc.id, tab: "passages" })}>
          {t("docs.seeSplit")}
        </a>
        {doc.passages.slice(0, 1).map((c) => (
          <article key={c.id} className="passage">
            <div className="passage-head">
              <strong>{t("docs.example", { n: String(c.idx + 1) })}</strong>
              {c.page && <a href={href("lire", { doc: doc.id, page: String(c.page) })}>{t("common.page", { n: String(c.page) })}</a>}
            </div>
            <p className="clamp">{c.text}</p>
          </article>
        ))}
      </section>
    </aside>
  );
}
