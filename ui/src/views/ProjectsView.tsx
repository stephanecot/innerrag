import { useState } from "react";
import { api, type Project } from "../api";
import { go } from "../App";
import { locale, translate, useT } from "../i18n";
import { bytes, longDate } from "../util";

export default function ProjectsView({
  projects, current, onChanged, onOpen,
}: { projects: Project[]; current: string; onChanged: () => Promise<void> | void; onOpen: (id: string) => void }) {
  const [id, setId] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; title: string } | null>(null);
  const [watching, setWatching] = useState<{ id: string; dir: string } | null>(null);
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const t = useT();
  const repo = t("common.exampleRepo");

  const act = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError("");
    try {
      await fn();
      await onChanged();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const slugHint = id && !/^[a-z0-9][a-z0-9_-]{0,63}$/.test(id);

  return (
    <div className="page" style={{ flexDirection: "row", flexWrap: "wrap", gap: 24, alignContent: "flex-start" }}>
      <section aria-labelledby="projects-title" style={{ flex: "999 1 480px", minWidth: 0, display: "flex", flexDirection: "column", gap: 18 }}>
        <div className="page-head">
          <div>
            <h1 id="projects-title">{t("projects.title")}</h1>
            <p>{t("projects.intro")}</p>
          </div>
        </div>
        {error && <div className="error-banner" role="alert">{error}</div>}
        {notice && <div className="success" role="status">{notice}</div>}

        <ul className="project-list panel">
          {projects.map((p) =>
            confirming === p.id ? (
              <li key={p.id} className="confirm">
                <div className="project-name">
                  <strong>{p.title}</strong>
                  <span className="danger-text" style={{ fontSize: 14 }}>
                    {t("projects.deleteWarn", { size: bytes(p.size_bytes) })}
                  </span>
                </div>
                <button type="button" className="btn" onClick={() => setConfirming(null)}>{t("common.cancel")}</button>
                <button
                  type="button"
                  className="btn btn-danger-solid"
                  disabled={busy}
                  onClick={() => act(async () => {
                    await api.deleteProject(p.id);
                    setConfirming(null);
                  })}
                >
                  {t("projects.deleteForever")}
                </button>
              </li>
            ) : (
              <li key={p.id} className={p.id === current ? "current" : ""}>
                <div className="project-name">
                  {renaming?.id === p.id ? (
                    <form
                      style={{ display: "flex", gap: 8 }}
                      onSubmit={(e) => {
                        e.preventDefault();
                        act(async () => {
                          await api.updateProject(p.id, { title: renaming.title });
                          setRenaming(null);
                        });
                      }}
                    >
                      <label htmlFor={`rename-${p.id}`} className="sr-only">{t("projects.newTitle")}</label>
                      <input id={`rename-${p.id}`} className="input" value={renaming.title} autoFocus onChange={(e) => setRenaming({ id: p.id, title: e.target.value })} />
                      <button type="submit" className="btn btn-primary" disabled={busy}>{t("projects.rename")}</button>
                      <button type="button" className="btn" onClick={() => setRenaming(null)}>{t("common.cancel")}</button>
                    </form>
                  ) : (
                    <span style={{ display: "flex", alignItems: "baseline", gap: 10 }}>
                      <strong>{p.title}</strong>
                      {p.id === current && <span className="muted" style={{ fontSize: 13 }}>{t("projects.current")}</span>}
                    </span>
                  )}
                  <span className="muted" style={{ fontSize: 14 }}>
                    {t("projects.meta", { id: p.id, date: longDate(p.created_at), description: p.description ? `. ${p.description}` : "" })}
                  </span>
                  {watching?.id === p.id ? (
                    <form
                      style={{ display: "flex", flexWrap: "wrap", gap: 8, marginTop: 8 }}
                      onSubmit={(e) => {
                        e.preventDefault();
                        act(async () => {
                          await api.updateProject(p.id, { watch_dir: watching.dir });
                          setWatching(null);
                          setNotice(watching.dir ? translate("projects.watchingNow", { id: p.id, dir: watching.dir }) : translate("projects.unwatched", { id: p.id }));
                        });
                      }}
                    >
                      <label htmlFor={`watch-${p.id}`} className="sr-only">{t("projects.watchedDir")}</label>
                      <input
                        id={`watch-${p.id}`}
                        className="input"
                        style={{ flex: "1 1 220px" }}
                        placeholder={t("projects.watchPh")}
                        value={watching.dir}
                        autoFocus
                        onChange={(e) => setWatching({ id: p.id, dir: e.target.value })}
                      />
                      <button type="submit" className="btn btn-primary" disabled={busy}>{t("common.save")}</button>
                      <button type="button" className="btn" onClick={() => setWatching(null)}>{t("common.cancel")}</button>
                    </form>
                  ) : p.watch_dir ? (
                    <span style={{ fontSize: 14 }}>
                      {t.rich("projects.follows", { dir: <span className="mono">/watch/{p.watch_dir}</span> })}
                      {p.watch && `, ${t("projects.filesWatched", { n: p.watch.files })}`}
                      {p.watch?.last_scan && `, ${t("projects.lastScan", { time: new Date(p.watch.last_scan * 1000).toLocaleTimeString(locale(), { hour: "2-digit", minute: "2-digit" }) })}`}
                      {p.watch?.last_error && <span className="danger-text">. {t("projects.watchError", { error: p.watch.last_error })}</span>}
                    </span>
                  ) : null}
                </div>
                <span className="muted" style={{ fontSize: 14 }}>{t("projects.onDisk", { size: bytes(p.size_bytes) })}</span>
                {p.id !== current && (
                  <button type="button" className="btn btn-primary" onClick={() => { onOpen(p.id); go("carte"); }}>{t("common.open")}</button>
                )}
                {renaming?.id !== p.id && (
                  <button type="button" className="btn" onClick={() => setRenaming({ id: p.id, title: p.title })}>{t("projects.rename")}</button>
                )}
                {watching?.id !== p.id && (
                  <button type="button" className="btn" onClick={() => setWatching({ id: p.id, dir: p.watch_dir ?? "" })}>
                    {p.watch_dir ? t("projects.changeDir") : t("projects.watchDir")}
                  </button>
                )}
                {p.watch_dir && (
                  <button
                    type="button"
                    className="btn"
                    disabled={busy}
                    onClick={() => act(async () => {
                      const r = await api.scanWatch(p.id);
                      setNotice(translate("projects.scanResult", {
                        id: p.id,
                        added: r.added,
                        changed: r.changed,
                        removed: r.removed,
                        unchanged: r.unchanged,
                        errors: r.errors.length ? translate("projects.scanErrors", { n: r.errors.length }) : "",
                      }));
                    })}
                  >
                    {t("projects.scanNow")}
                  </button>
                )}
                <button type="button" className="btn btn-danger" onClick={() => setConfirming(p.id)}>{t("common.delete")}</button>
              </li>
            ),
          )}
          {projects.length === 0 && <li className="muted">{t("projects.none")}</li>}
        </ul>

        <form
          className="panel"
          aria-labelledby="new-project-title"
          style={{ display: "flex", flexDirection: "column", gap: 14, padding: 20 }}
          onSubmit={(e) => {
            e.preventDefault();
            act(async () => {
              const created = await api.createProject({ id, title: title || undefined, description });
              setId("");
              setTitle("");
              setDescription("");
              onOpen(created.id);
            });
          }}
        >
          <h2 id="new-project-title">{t("projects.new")}</h2>
          <div className="toolbar" style={{ alignItems: "flex-start", gap: 12 }}>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label htmlFor="project-id">{t("projects.id")}</label>
              <input id="project-id" className="input" required value={id} placeholder={t("projects.idPh")} onChange={(e) => setId(e.target.value)} aria-describedby="project-id-hint" />
              <span id="project-id-hint" className={`hint${slugHint ? " danger-text" : ""}`}>{t("projects.idHint")}</span>
            </div>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label htmlFor="project-title">{t("projects.titleLabel")}</label>
              <input id="project-title" className="input" value={title} placeholder={t("projects.titlePh")} onChange={(e) => setTitle(e.target.value)} />
            </div>
          </div>
          <div className="field">
            <label htmlFor="project-desc">{t("projects.description")}</label>
            <textarea id="project-desc" className="textarea" rows={2} style={{ minHeight: 64 }} value={description} onChange={(e) => setDescription(e.target.value)} />
          </div>
          <button type="submit" className="btn btn-primary" style={{ alignSelf: "flex-start" }} disabled={busy || !id || !!slugHint}>{t("projects.create")}</button>
        </form>
      </section>

      <aside className="git-card" aria-labelledby="git-title">
        <h2 id="git-title">{t("projects.gitTitle")}</h2>
        <p style={{ fontSize: 14, color: "var(--ink-2)" }}>{t("projects.gitIntro")}</p>
        <pre className="code">{`docker run -p 8080:8080 \\
  -v ./${repo}/.innerrag:/data/projects/${current || t("common.exampleProject")} \\
  innerrag`}</pre>
        <ul>
          <li>{t.rich("projects.lbdb", { file: <span className="mono">innerrag.lbdb</span> })}</li>
          <li>{t.rich("projects.projectJson", { file: <span className="mono">project.json</span> })}</li>
          <li>{t.rich("projects.gitignore", { file: <span className="mono">.gitignore</span> })}</li>
        </ul>
        <h3 style={{ marginTop: 6 }}>{t("projects.watchTitle")}</h3>
        <p style={{ fontSize: 14, color: "var(--ink-2)" }}>
          {t.rich("projects.watchIntro", { watch: <span className="mono">/watch</span>, docs: <span className="mono">docs/</span> })}
        </p>
        <pre className="code">{`-v ./${repo}/docs:/watch/${repo}-docs:ro`}</pre>
        <p className="note">{t("projects.binaryNote")}</p>
      </aside>
    </div>
  );
}
