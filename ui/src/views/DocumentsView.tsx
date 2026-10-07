import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type DocumentSummary, type DocumentWithChunks, type FileInfo, type Job, type ServerConfig, type Status, type Stats } from "../api";
import { href } from "../App";
import { CloseIcon, PlusIcon } from "../Icons";
import { bytes, JOB_STAGE, jobPercent, longDate, num, plural, relativeDate, STATUS_LABEL } from "../util";

const ACCEPT = ".pdf,.docx,.pptx,.doc,.ppt,.md,.markdown,.html,.htm,.txt";

type Panel = { kind: "none" } | { kind: "new" } | { kind: "doc"; id: string };

export default function DocumentsView({
  project, config, params,
}: { project: string; config: ServerConfig | null; params: URLSearchParams }) {
  const p = useMemo(() => api.project(project), [project]);
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

  const selectedId = panel.kind === "doc" ? panel.id : null;

  return (
    <>
      <section className="page" aria-labelledby="docs-title">
        <div className="page-head">
          <div>
            <h1 id="docs-title">Documents</h1>
            <p>Seuls les documents publiés servent aux réponses. Les brouillons restent visibles sur la carte, en pointillés.</p>
          </div>
          <button type="button" className="btn btn-primary" onClick={() => setPanel({ kind: "new" })}>
            <PlusIcon />
            Ajouter un document
          </button>
        </div>

        <div className="toolbar">
          <label htmlFor="doc-filter" className="sr-only">Filtrer par titre</label>
          <input
            id="doc-filter"
            className="input"
            type="search"
            placeholder="Filtrer par titre"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            style={{ flex: "1 1 220px", maxWidth: 320 }}
          />
          <div className="segmented" role="group" aria-label="Statut">
            {([["", "Tous", stats?.documents], ["PUBLISHED", "Publiés", stats?.published], ["DRAFT", "Brouillons", stats?.drafts]] as const).map(
              ([value, label, count]) => (
                <button key={value} type="button" aria-pressed={status === value} onClick={() => setStatus(value)}>
                  {label} {count ?? ""}
                </button>
              ),
            )}
          </div>
          {tags.length > 0 && (
            <label className="field-box">
              Tag
              <select value={tag} onChange={(e) => setTag(e.target.value)}>
                <option value="">Tous</option>
                {tags.map((t) => <option key={t.tag} value={t.tag}>{t.tag} ({t.count})</option>)}
              </select>
            </label>
          )}
        </div>

        {error && <div className="error-banner" role="alert">{error}</div>}

        {recent.length > 0 && (
          <section className="panel jobs" aria-label="Ingestions en cours et récentes">
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
            <p>{q || status || tag ? "Aucun document ne correspond à ces filtres." : "Ce projet ne contient encore aucun document."}</p>
            {!(q || status || tag) && (
              <button type="button" className="btn btn-primary" onClick={() => setPanel({ kind: "new" })}>Ajouter un document</button>
            )}
          </div>
        ) : (
          <div className="table-wrap">
            <table style={{ minWidth: 760 }}>
              <thead>
                <tr>
                  <th scope="col">Titre</th>
                  <th scope="col">Statut</th>
                  <th scope="col">Tags</th>
                  <th scope="col">Créé par</th>
                  <th scope="col" className="num">Passages</th>
                  <th scope="col" className="num">Entités</th>
                  <th scope="col">Modifié</th>
                </tr>
              </thead>
              <tbody>
                {pending.map((j) => (
                  <tr key={j.id} className="pending-row">
                    <td><strong className="cell-title" title={j.filename}>{j.filename}</strong></td>
                    <td><span className="status status-INDEXING">Indexation {jobPercent(j)} %</span></td>
                    <td colSpan={4} className="muted">
                      {JOB_STAGE[j.stage]}{j.total ? ` : ${num(j.done)} / ${num(j.total)} passages` : ""}
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
                        Annuler
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
                      <button type="button" className="row-title cell-title" title={d.title} onClick={() => setPanel({ kind: "doc", id: d.id })}>{d.title}</button>
                    </td>
                    <td><span className={`status status-${d.status}`}>{STATUS_LABEL[d.status]}</span></td>
                    <td>
                      <span style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                        {d.tags.map((t) => <span key={t} className="chip">{t}</span>)}
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
  const add = (raw: string) => {
    const t = raw.trim().replace(/,$/, "").trim();
    if (t && !value.some((v) => v.toLowerCase() === t.toLowerCase())) onChange([...value, t]);
    setDraft("");
  };
  return (
    <div className="tags-input">
      {value.map((t) => (
        <span key={t} className="chip">
          {t}
          <button type="button" aria-label={`Retirer le tag ${t}`} onClick={() => onChange(value.filter((v) => v !== t))}>×</button>
        </span>
      ))}
      <input
        id={id}
        type="text"
        placeholder="Ajouter un tag"
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
  return (
    <fieldset style={{ margin: 0, padding: 0, border: 0 }} className="field">
      <legend className="label" style={{ paddingBottom: 6 }}>Statut</legend>
      <div className="status-choice">
        {(["DRAFT", "PUBLISHED"] as const).map((s) => (
          <label key={s} className={value === s ? "on" : ""}>
            <input type="radio" name="status" checked={value === s} onChange={() => onChange(s)} />
            {STATUS_LABEL[s]}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

function JobRow({ job, onOpen, onDismiss }: { job: Job; onOpen: () => void; onDismiss: () => void }) {
  const pct = jobPercent(job);
  const running = !job.finished_at;
  return (
    <div className={`job-row${job.stage === "failed" ? " failed" : ""}`}>
      <div className="job-main">
        <div className="job-head">
          <strong>{job.filename}</strong>
          <span className="muted">
            {job.stage === "done" && job.report
              ? `${plural(job.report.chunks, "passage", "passages")}, ${plural(job.report.entities, "entité", "entités")} en ${Math.round(job.report.millis / 1000)} s`
              : job.stage === "failed"
                ? "Échec"
                : job.stage === "cancelled"
                ? "Annulée"
                : `${JOB_STAGE[job.stage]}${job.total ? ` ${num(job.done)} / ${num(job.total)}` : ""}`}
          </span>
        </div>
        {running && (
          <div className="meter-track job-track" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label={`Ingestion de ${job.filename}`}>
            <span className="meter-fill" style={{ display: "block", width: `${pct}%`, background: "var(--c-organization)" }} />
          </div>
        )}
        {job.error && <span className="danger-text" style={{ fontSize: 14 }}>{job.error}</span>}
      </div>
      {job.stage === "done" && <button type="button" className="btn" onClick={onOpen}>Ouvrir</button>}
      {!running && (
        <button type="button" className="icon-button" aria-label="Masquer cette ligne" onClick={onDismiss}><CloseIcon /></button>
      )}
    </div>
  );
}

/** Drop zone + file picker for every supported format. */
function FileDrop({ file, onFile, maxMb }: { file: File | null; onFile: (f: File | null) => void; maxMb: number }) {
  const [over, setOver] = useState(false);
  if (file) {
    return (
      <div className="file-chosen">
        <span><strong>{file.name}</strong> <span className="muted">{bytes(file.size)}</span></span>
        <button type="button" className="btn" onClick={() => onFile(null)}>Changer</button>
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
      <strong>Déposez un fichier ici, ou cliquez pour le choisir</strong>
      <span className="muted">PDF, Word, PowerPoint, Markdown ou texte, {maxMb} Mo au plus</span>
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
  const [pasting, setPasting] = useState(false);
  const [title, setTitle] = useState("");
  const [id, setId] = useState("");
  const [source, setSource] = useState("");
  const [tags, setTags] = useState<string[]>([]);
  const [status, setStatus] = useState<Status>(config?.default_status ?? "PUBLISHED");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const maxMb = config?.max_upload_mb ?? 200;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    try {
      const p = api.project(project);
      const job = file
        ? await p.uploadDocument(file, { title, tags: tags.join(","), status, id: id.trim(), source: source.trim() })
        : await p.createDocument({ title, text, tags, status, id: id.trim() || undefined, source: source.trim() || undefined });
      onQueued(job);
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const ready = file ? file.size <= maxMb * 1024 * 1024 : pasting && text.trim().length > 0;

  return (
    <aside className="side" aria-labelledby="new-title">
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <h2 id="new-title" style={{ fontSize: 28 }}>Nouveau document</h2>
        <button type="button" className="btn" onClick={onClose}>Annuler</button>
      </div>
      <form onSubmit={submit} style={{ display: "flex", flexDirection: "column", gap: 14 }}>
        {pasting ? (
          <div className="field">
            <label htmlFor="new-doc-text">Texte</label>
            <textarea id="new-doc-text" className="textarea" rows={10} required value={text} onChange={(e) => setText(e.target.value)} />
            <span className="hint">Le Markdown est compris : titres, et front-matter title, tags, status.</span>
            <button type="button" className="btn" style={{ alignSelf: "flex-start" }} onClick={() => setPasting(false)}>Importer un fichier plutôt</button>
          </div>
        ) : (
          <div className="field">
            <span className="label">Fichier</span>
            <FileDrop file={file} onFile={setFile} maxMb={maxMb} />
            {file && file.size > maxMb * 1024 * 1024 && <span className="danger-text hint">Ce fichier dépasse {maxMb} Mo.</span>}
            {!file && <button type="button" className="btn" style={{ alignSelf: "flex-start" }} onClick={() => setPasting(true)}>Coller du texte plutôt</button>}
          </div>
        )}
        <div className="field">
          <label htmlFor="new-doc-title">Titre</label>
          <input id="new-doc-title" className="input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Repris du document si vide" />
        </div>
        <StatusChoice value={status} onChange={setStatus} />
        <div className="field">
          <label htmlFor="new-doc-tags">Tags</label>
          <TagsInput id="new-doc-tags" value={tags} onChange={setTags} />
        </div>
        <details>
          <summary style={{ minHeight: 40, cursor: "pointer" }}>Identifiant et source</summary>
          <div style={{ display: "flex", flexDirection: "column", gap: 12, paddingTop: 8 }}>
            <div className="field">
              <label htmlFor="new-doc-id">Identifiant</label>
              <input id="new-doc-id" className="input" value={id} onChange={(e) => setId(e.target.value)} placeholder="généré si vide" />
              <span className="hint">Un identifiant stable permet de remplacer le document plus tard.</span>
            </div>
            <div className="field">
              <label htmlFor="new-doc-source">Source</label>
              <input id="new-doc-source" className="input" value={source} onChange={(e) => setSource(e.target.value)} placeholder="nom du fichier si vide" />
            </div>
          </div>
        </details>
        {error && <div className="error-banner" role="alert">{error}</div>}
        <button type="submit" className="btn btn-primary" disabled={busy || !ready}>
          {busy ? "Envoi…" : "Ajouter au projet"}
        </button>
        <p className="hint muted">L'indexation se poursuit en arrière-plan : vous pouvez fermer ce panneau et suivre sa progression en haut de la liste.</p>
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
      <aside className="side" aria-label="Document">
        {job ? (
          <p className="muted">Ce document est en cours d'indexation ({JOB_STAGE[job.stage].toLowerCase()}, {jobPercent(job)} %).</p>
        ) : error ? (
          <div className="error-banner" role="alert">{error}</div>
        ) : (
          <p className="muted">Chargement…</p>
        )}
        <button type="button" className="btn" onClick={onClose}>Fermer</button>
      </aside>
    );
  }

  const file = (doc.metadata as { file?: FileInfo } | null)?.file;
  const dirty = title !== doc.title || status !== doc.status || source !== doc.source || tags.join("\u0000") !== doc.tags.join("\u0000");

  return (
    <aside className="side" aria-labelledby="doc-title">
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <h2 id="doc-title" className="side-title" style={{ fontSize: 26 }} title={doc.title}>{doc.title}</h2>
          <p className="muted wrap-anywhere" style={{ fontSize: 14 }}>
            Identifiant {doc.id}, créé par {doc.creator} le {longDate(doc.created_at)}
          </p>
          {file && (
            <p className="muted" style={{ fontSize: 14 }}>
              Importé de {file.name} ({file.format_label}{file.pages ? `, ${plural(file.pages, file.format === "pptx" || file.format === "ppt" ? "diapositive" : "page", file.format === "pptx" || file.format === "ppt" ? "diapositives" : "pages")}` : ""}, {bytes(file.size)})
            </p>
          )}
          {job && <p style={{ fontSize: 14 }}>Réindexation en cours : {JOB_STAGE[job.stage].toLowerCase()}, {jobPercent(job)} %</p>}
        </div>
        <button type="button" className="btn" onClick={onClose}>Fermer</button>
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
              setMessage("Remplacement lancé : la réindexation se poursuit en arrière-plan.");
            });
          }}
        >
          <div className="field">
            <span className="label">Nouveau fichier</span>
            <FileDrop file={newFile} onFile={setNewFile} maxMb={200} />
          </div>
          {!newFile && (
            <div className="field">
              <label htmlFor="replace-text">Ou nouveau texte</label>
              <textarea id="replace-text" className="textarea" rows={8} value={newText} onChange={(e) => setNewText(e.target.value)} />
            </div>
          )}
          <span className="hint">L'ancien contenu, ses passages et ses liens sont retirés du graphe. Créateur et date de création sont conservés.</span>
          <div className="toolbar">
            <button type="submit" className="btn btn-primary" disabled={busy || (!newFile && !newText.trim())}>
              {busy ? "Envoi…" : "Remplacer et réindexer"}
            </button>
            <button type="button" className="btn" onClick={() => setMode("edit")}>Annuler</button>
          </div>
        </form>
      ) : (
        <form
          style={{ display: "flex", flexDirection: "column", gap: 14 }}
          onSubmit={(e) => {
            e.preventDefault();
            run(async () => {
              fill(await p.patchDocument(doc.id, { title, status, tags, source }));
              setMessage("Modifications enregistrées.");
              onChanged();
            });
          }}
        >
          <div className="field">
            <label htmlFor="doc-edit-title">Titre</label>
            <input id="doc-edit-title" className="input" required value={title} onChange={(e) => setTitle(e.target.value)} />
          </div>
          <StatusChoice value={status} onChange={setStatus} />
          <div className="field">
            <label htmlFor="doc-edit-tags">Tags</label>
            <TagsInput id="doc-edit-tags" value={tags} onChange={setTags} />
          </div>
          <div className="field">
            <label htmlFor="doc-edit-source">Source</label>
            <input id="doc-edit-source" className="input" value={source} onChange={(e) => setSource(e.target.value)} />
          </div>
          {mode === "confirm-delete" ? (
            <div className="error-banner" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              <span>Supprimer « {doc.title} » retire ses {plural(doc.passages.length, "passage", "passages")} et les entités qu'il est seul à citer.</span>
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
                  Supprimer définitivement
                </button>
                <button type="button" className="btn" onClick={() => setMode("edit")}>Annuler</button>
              </div>
            </div>
          ) : (
            <div className="toolbar">
              <button type="submit" className="btn btn-primary" disabled={busy || !dirty}>Enregistrer</button>
              <button type="button" className="btn" onClick={() => setMode("replace")}>Remplacer le texte</button>
              <button type="button" className="btn btn-danger" onClick={() => setMode("confirm-delete")}>Supprimer</button>
            </div>
          )}
        </form>
      )}

      {message && <div className="success" role="status">{message}</div>}
      {error && <div className="error-banner" role="alert">{error}</div>}

      <section style={{ display: "flex", flexDirection: "column", gap: 10, borderTop: "1px solid var(--rule-soft)", paddingTop: 18 }}>
        <h3>Lire</h3>
        <p className="muted" style={{ fontSize: 14 }}>
          {plural(doc.passage_count, "passage indexé", "passages indexés")}, {plural(doc.entities, "entité", "entités")}.
        </p>
        <div className="toolbar">
          <a className="btn btn-primary" href={href("lire", { doc: doc.id })}>Lire le document</a>
          {file?.stored && (
            <a className="btn" href={p.fileUrl(doc.id)} target="_blank" rel="noopener">Ouvrir l'original ({file.format_label})</a>
          )}
          <a className="btn" href={href("lire", { doc: doc.id, tab: "passages" })}>Voir les passages</a>
        </div>
        {doc.passages.slice(0, 2).map((c) => (
          <article key={c.id} className="passage">
            <div className="passage-head">
              <strong>Passage {c.idx + 1}</strong>
              {c.page && <a href={href("lire", { doc: doc.id, page: String(c.page) })}>page {c.page}</a>}
            </div>
            <p className="clamp">{c.text}</p>
          </article>
        ))}
      </section>
    </aside>
  );
}
