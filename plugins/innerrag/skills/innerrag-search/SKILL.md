---
name: innerrag-search
description: Answer questions from an innerrag knowledge base (local Graph RAG server). Use when the user asks about the content of their ingested documents, internal knowledge, procedures or anything "in the knowledge base / dans la base / dans nos documents / d'après la doc", or asks to check what the graph knows before answering. Retrieves passages fused with entity-graph expansion and cites them.
allowed-tools: Bash(python3 *innerrag.py*)
---

# Search the innerrag knowledge graph

`CLI = python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py"`

Server and project come from the environment: `INNERRAG_URL` (default `http://localhost:8080`) and
`INNERRAG_PROJECT` (default: the server's default project).
Pass `-p <project>` to target another project; `$CLI projects` lists them.

## Steps

1. Retrieve context (markdown, ready to read):
   ```bash
   python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" search "<question in the user's words>" --context -k 8
   ```
   Options: `--tags a,b` to restrict to tagged documents, `--include-drafts` to also read DRAFT
   documents (only PUBLISHED ones by default — say so if you use it), `-p <project>`.
2. Answer **only** from the returned passages. Cite them as `[n]` and end with the sources
   (document titles, with the page for PDFs: passages carry "page N"). If the passages do not
   answer, say what is missing instead of guessing. The user can read a document in the web UI
   at `$INNERRAG_URL/#/lire?doc=<doc id>&page=<N>`.
3. When the answer depends on a relation between entities (who funded what, which team owns
   which system…), follow it with the explore skill: `... entity "<name>"`.

## Tips

- Rephrase and search again with entity names from the "Related entities" section when the
  first search is thin; two focused searches beat one vague one.
- `search "<q>" -k 5` without `--context` returns JSON with scores (`similarity`, `graph_score`)
  when you need to judge relevance.
- Each search is logged in the server's call history with the size of the context returned.
