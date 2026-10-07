---
name: innerrag-usage
description: Report innerrag usage — call history, how much context (tokens) searches and MCP tools returned to agents, latency and errors, per channel (MCP, REST, UI), operation or project. Use for "consommation", "historique des appels", "combien de tokens", "qui appelle la base", "erreurs récentes", or cost analysis of RAG usage.
allowed-tools: Bash(python3 *innerrag.py*)
---

# innerrag call history and consumption

`CLI = python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py"` (env: `INNERRAG_URL`).

```bash
python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" history --hours 24 --limit 50
```
Filters: `--channel mcp|rest|ui`, `--operation search|ingest|replace|update|delete|explore|cypher`,
`--errors`, `-p <project>` (otherwise all projects).

The result has `totals` (calls, context_tokens, median_ms, errors) and `calls` (newest first)
with `channel`, `project`, `operation`, `detail` (query or title), `result`, `context_tokens`,
`duration_ms`, `ok`/`error`.

How to read it:
- `context_tokens` estimates (characters / 4) what the server handed back; for MCP and search
  calls that is what the calling agent then reads, i.e. the cost driver on the LLM side. The
  server itself runs only local models and costs nothing per call.
- Summarise by channel and operation, point out the heaviest calls and repeated queries, and
  suggest a lower `k` or tag filters when searches return much more context than needed.
- The UI shows the same data with an hourly chart on the "Historique" page.
