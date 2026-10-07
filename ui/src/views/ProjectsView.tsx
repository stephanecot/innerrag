import { useState } from "react";
import { api, type Project } from "../api";
import { go } from "../App";
import { bytes, longDate } from "../util";

export default function ProjectsView({
  projects, current, onChanged, onOpen,
}: { projects: Project[]; current: string; onChanged: () => Promise<void> | void; onOpen: (id: string) => void }) {
  const [id, setId] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; title: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

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
            <h1 id="projects-title">Projets</h1>
            <p>Chaque projet a sa propre base. Ses documents, entités et recherches ne voient jamais ceux des autres.</p>
          </div>
        </div>
        {error && <div className="error-banner" role="alert">{error}</div>}

        <ul className="project-list panel">
          {projects.map((p) =>
            confirming === p.id ? (
              <li key={p.id} className="confirm">
                <div className="project-name">
                  <strong>{p.title}</strong>
                  <span className="danger-text" style={{ fontSize: 14 }}>
                    Supprimer ce projet efface sa base ({bytes(p.size_bytes)}). Cette action est définitive.
                  </span>
                </div>
                <button type="button" className="btn" onClick={() => setConfirming(null)}>Annuler</button>
                <button
                  type="button"
                  className="btn btn-danger-solid"
                  disabled={busy}
                  onClick={() => act(async () => {
                    await api.deleteProject(p.id);
                    setConfirming(null);
                  })}
                >
                  Supprimer définitivement
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
                      <label htmlFor={`rename-${p.id}`} className="sr-only">Nouveau titre</label>
                      <input id={`rename-${p.id}`} className="input" value={renaming.title} autoFocus onChange={(e) => setRenaming({ id: p.id, title: e.target.value })} />
                      <button type="submit" className="btn btn-primary" disabled={busy}>Renommer</button>
                      <button type="button" className="btn" onClick={() => setRenaming(null)}>Annuler</button>
                    </form>
                  ) : (
                    <span style={{ display: "flex", alignItems: "baseline", gap: 10 }}>
                      <strong>{p.title}</strong>
                      {p.id === current && <span className="muted" style={{ fontSize: 13 }}>projet ouvert</span>}
                    </span>
                  )}
                  <span className="muted" style={{ fontSize: 14 }}>
                    {p.id}, créé le {longDate(p.created_at)}{p.description ? `. ${p.description}` : ""}
                  </span>
                </div>
                <span className="muted" style={{ fontSize: 14 }}>{bytes(p.size_bytes)} sur disque</span>
                {p.id !== current && (
                  <button type="button" className="btn btn-primary" onClick={() => { onOpen(p.id); go("carte"); }}>Ouvrir</button>
                )}
                {renaming?.id !== p.id && (
                  <button type="button" className="btn" onClick={() => setRenaming({ id: p.id, title: p.title })}>Renommer</button>
                )}
                <button type="button" className="btn btn-danger" onClick={() => setConfirming(p.id)}>Supprimer</button>
              </li>
            ),
          )}
          {projects.length === 0 && <li className="muted">Aucun projet. Créez-en un ci-dessous.</li>}
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
          <h2 id="new-project-title">Nouveau projet</h2>
          <div className="toolbar" style={{ alignItems: "flex-start", gap: 12 }}>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label htmlFor="project-id">Identifiant</label>
              <input id="project-id" className="input" required value={id} placeholder="juridique-2026" onChange={(e) => setId(e.target.value)} aria-describedby="project-id-hint" />
              <span id="project-id-hint" className={`hint${slugHint ? " danger-text" : ""}`}>Minuscules, chiffres, tirets. Devient le nom du dossier.</span>
            </div>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label htmlFor="project-title">Titre</label>
              <input id="project-title" className="input" value={title} placeholder="Veille juridique 2026" onChange={(e) => setTitle(e.target.value)} />
            </div>
          </div>
          <div className="field">
            <label htmlFor="project-desc">Description</label>
            <textarea id="project-desc" className="textarea" rows={2} style={{ minHeight: 64 }} value={description} onChange={(e) => setDescription(e.target.value)} />
          </div>
          <button type="submit" className="btn btn-primary" style={{ alignSelf: "flex-start" }} disabled={busy || !id || !!slugHint}>Créer le projet</button>
        </form>
      </section>

      <aside className="git-card" aria-labelledby="git-title">
        <h2 id="git-title">Partager un projet via git</h2>
        <p style={{ fontSize: 14, color: "var(--ink-2)" }}>Un projet tient dans un dossier. Montez-le depuis votre dépôt et commitez-le comme n'importe quel fichier.</p>
        <pre className="code">{`docker run -p 8080:8080 \\
  -v ./mon-depot/.innerrag:/data/projects/${current || "mon-projet"} \\
  innerrag`}</pre>
        <ul>
          <li><span className="mono">innerrag.lbdb</span> : la base, cohérente après chaque écriture. Vous pouvez commiter sans arrêter le serveur.</li>
          <li><span className="mono">project.json</span> : titre, description et modèle d'embedding utilisé.</li>
          <li><span className="mono">.gitignore</span> : écarte les fichiers temporaires.</li>
        </ul>
        <p className="note">La base est un fichier binaire : git ne sait pas fusionner deux modifications parallèles. Convenez de qui ingère à quel moment.</p>
      </aside>
    </div>
  );
}
