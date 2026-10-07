---
name: innerrag-projects
description: Manage innerrag projects (isolated knowledge bases) and share them through git. Use for "crée un projet", "liste les projets", "supprime le projet", "partager la base dans le repo", mounting a project folder into the Docker image, or checking server health/configuration.
allowed-tools: Bash(python3 *innerrag.py*), Bash(docker ps*), Bash(git status*)
---

# innerrag projects

`CLI = python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py"` (env: `INNERRAG_URL`).

| Task | Command |
|---|---|
| Server status, models, default project | `$CLI health` |
| List projects | `$CLI projects` |
| Create | `$CLI project-create <id> --title "Titre" --description "…"` (id: lowercase, digits, `-`, `_`) |
| Counts for a project | `$CLI -p <id> stats` |
| Delete (irreversible, removes its database) | `$CLI project-delete <id>` — confirm with the user first |

Use a project per team or repository: documents, entities and searches never cross projects.
Set `INNERRAG_PROJECT=<id>` (e.g. in the repo's `.claude/settings.json` `env`) so the other
skills and the MCP server target it by default; the MCP endpoint bound to one project is
`$INNERRAG_URL/mcp/<id>`.

## Sharing a project through git

A project is one folder: `innerrag.lbdb` (the LadybugDB database), `project.json` (title,
embedding model) and a `.gitignore`. To version it inside a repository:

```bash
docker run -d -p 8080:8080 -v "$PWD/.innerrag:/data/projects/<id>" --user "$(id -u):$(id -g)" innerrag
```

- The server checkpoints after every write, so `innerrag.lbdb` is consistent and can be
  committed while the server runs (`git add .innerrag && git commit`).
- It is a binary file: git cannot merge two concurrent edits. Agree on who ingests when, and
  pull before ingesting.
- Everyone must use the same innerrag image (same embedding model); the server refuses a
  project indexed with another model.
