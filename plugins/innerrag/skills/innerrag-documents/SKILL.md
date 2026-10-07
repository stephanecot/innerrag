---
name: innerrag-documents
description: Manage documents in an innerrag knowledge base — list them, read one, publish or unpublish (DRAFT/PUBLISHED), retitle, set tags, delete. Use for "liste les documents", "publie les brouillons", "passe en brouillon", "ajoute le tag", "supprime le document", "qui a créé", or reviewing what the base contains.
allowed-tools: Bash(python3 *innerrag.py*)
---

# Manage innerrag documents

`CLI = python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py"` (env: `INNERRAG_URL`, `INNERRAG_PROJECT`; `-p <project>` to switch).

| Task | Command |
|---|---|
| List (filters optional) | `$CLI docs [--status draft\|published] [--tag rh] [--q "titre"]` |
| Read one, with passages and entities | `$CLI doc <id>` |
| Publish / unpublish | `$CLI doc-set <id> --status published` (or `draft`) |
| Retitle | `$CLI doc-set <id> --title "Nouveau titre"` |
| Replace the tag list | `$CLI doc-set <id> --tags rh,paie` (`--tags ""` clears) |
| Tag counts | `$CLI tags` |
| Delete | `$CLI doc-delete <id>` |
| Replace the text | use the ingest skill with the same id |

Rules:
- Status and metadata changes do not re-index anything: they are instant.
- Only PUBLISHED documents feed searches (REST, MCP, agents); DRAFT ones stay visible in the UI.
- Deleting removes the document, its passages and the entities only it mentioned. It is
  irreversible: list exactly what will be deleted and get the user's confirmation first,
  especially for bulk deletions.
- Documents have a creator (single-user server: the configured `INNERRAG_USER`).
