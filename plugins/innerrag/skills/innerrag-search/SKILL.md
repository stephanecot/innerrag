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

1. Survey first, cheaply (one line per passage: chunk id, heading path, page, best sentence):
   ```bash
   python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" search "<question in the user's words>" --map -k 10 --session <task-id>
   ```
   Then read only the passages you need, with their neighbours when the context matters:
   ```bash
   python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" read <chunk id> <chunk id> --window 1 --session <task-id>
   ```
   For a narrow question, `search "<q>" --context -k 5` gives the passages in full at once.
   Use one `--session` id per task (any string): passages already sent are not repeated.
   Options: `--budget N` caps the context in tokens, `--tags a,b` restricts to tagged documents,
   `--include-drafts` also reads DRAFT documents (only PUBLISHED by default — say so if you use
   it), `-p <project>`.
2. Answer **only** from the passages read. Cite them as `[n]` and end with the sources
   (document titles, with the page for PDFs: passages carry "page N"). If the passages do not
   answer, say what is missing instead of guessing. The user can read a document in the web UI
   at `$INNERRAG_URL/#/lire?doc=<doc id>&page=<N>`.
3. Report what your answer relied on — it builds the evaluation set and the gap report:
   ```bash
   python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" cite "<the user's question>" <chunk ids used> --outcome answered|partial|not_found
   ```
4. When the answer depends on a relation between entities (who funded what, which team owns
   which system…), follow it with the explore skill: `... entity "<name>"`.

## Tips

- Rephrase and search again with entity names from the "Related entities" section when the
  first search is thin; two focused searches beat one vague one.
- `search "<q>" -k 5` without `--context` returns JSON with scores (`similarity`, `graph_score`)
  when you need to judge relevance.
- Each search is logged in the server's call history with the size of the context returned.
