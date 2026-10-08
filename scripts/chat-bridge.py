#!/usr/bin/env python3
"""Bridge between the innerrag interface and the coding agents installed on this machine.

The innerrag server runs in Docker and cannot start the agents of the host. This script runs on
the host and lets the "Assistant" page of the interface chat with one of them:

- Claude Code (`claude -p`), with the account you logged in with (`claude`, then /login);
  ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN are removed from its environment, so no API key
  is used and the messages count against that subscription;
- GitHub Copilot CLI (`copilot -p`), with your GitHub Copilot subscription (`copilot`, then
  /login); its built-in GitHub MCP server is turned off.

Each message starts the chosen agent, connected to the innerrag MCP server of the current
project, and its events are streamed back to the page. The agent only gets the read tools of
innerrag: no shell, no file access, no ingestion (unless --allow-writes). The bridge listens on
127.0.0.1 and only answers pages served by the innerrag interface (Origin check), so another
website cannot drive it.

Usage:
    python3 scripts/chat-bridge.py [--innerrag http://localhost:18080] [--port 18765] [--model sonnet]
                                   [--claude claude] [--copilot copilot] [--copilot-model gpt-4.1]

Standard library only (Python 3.9+), Windows, macOS and Linux.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
from urllib.request import urlopen

READ_TOOLS = [
    "search_knowledge",
    "read_passages",
    "cite_sources",
    "explore_entity",
    "explore_relation",
    "list_documents",
    "graph_stats",
    "run_cypher",
    "list_projects",
    "ingestion_status",
]
WRITE_TOOLS = ["ingest_document", "ingest_file", "ingest_url"]
AGENTS = ("claude", "copilot")
PROJECT_ID = re.compile(r"^[a-z0-9][a-z0-9_-]{0,63}$")
SESSION_ID = re.compile(r"^[0-9a-f-]{36}$")
CONVERSATION_ID = re.compile(r"^[A-Za-z0-9_-]{1,64}$")
MODEL = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._\-\[\]]{0,63}$")
MAX_MESSAGE = 20_000
# Text written before `cite_sources` that is at least this long is taken as the answer.
ANSWER_MIN_CHARS = 160

SYSTEM_PROMPT = """You are the assistant of an innerrag knowledge base, project `{project}`. Its users
need precise and complete answers drawn from their documents (specifications, procedures, reports).

Research thoroughly before answering:
- call search_knowledge several times when the question has several aspects, with different
  wordings and the documents' own vocabulary (their language, their terms, requirement ids);
  mode "map" surveys a broad question cheaply, then read_passages(ids, window 1) reads the useful
  passages in full, with their neighbours;
- follow the actors, systems and concepts the passages name with explore_entity and explore_relation;
- read every passage you rely on in full, not just its first line.
Always pass session_id "{conversation}": a passage already sent in this conversation comes back
as "already provided" because it is in your context; if you no longer have its text, read it
again with read_passages without session_id.

This is one conversation: the earlier questions, answers and passages are in your context. Use
them, and resolve references ("it", "this rule", "the previous step") with them.

Answer completely and precisely, in Markdown, in the language of the question:
- give every rule, condition, exception, threshold, value, actor, step and requirement id the
  passages contain on the subject, not a vague summary of them;
- structure long answers with headings, numbered steps, and tables to compare options or to list
  fields, values and statuses;
- point out what the documents leave open and where they contradict each other;
- cite the source of each point: "document title, page N", or its section when there are no pages.
A precise question gets the direct answer first, then its details; a broad one gets a structured
account of everything the documents say about it.
The page renders GitHub Markdown (tables, lists, links, images) and, when a picture helps (a flow,
a sequence, an architecture, a comparison), a diagram in a ```mermaid block (flowchart,
sequenceDiagram, classDiagram, stateDiagram, erDiagram, gantt…) or a ```svg block (self-contained
SVG, viewBox set, no script), or a small static mock-up in an ```html block; never describe a
diagram without drawing it, and keep diagrams faithful to the passages.
{grounding}
Order of your reply: first write the complete answer for the user as text (that text is all the
user sees); only then, as your very last action, call cite_sources once with the user's question,
the ids of the passages you relied on and the outcome (answered, partial or not_found). Never call
cite_sources before the answer text, and write nothing after it."""

# Default: the answer must come from the documents only.
STRICT = """Answer ONLY from the passages returned by the innerrag tools. Never add facts, code,
names, numbers or advice from your own knowledge, even if you are sure of them, and never fill a
gap by guessing. Every statement must be backed by a cited passage. If the tools return nothing
relevant, or only part of the answer, say plainly that the documents do not cover it (or which
part they do not cover) and stop there; you may suggest other wordings to search for."""

OPEN = """Base your answer on the passages returned by the innerrag tools first. You may complete
it with your general knowledge when the documents fall short, but put that part after the
documented answer, under a line starting with "Hors documents :", and never present it as coming
from the base."""


# Where the server usually runs: the Docker image (docker-compose.yml), then the development server.
SERVER_CANDIDATES = ("http://localhost:8080", "http://localhost:18080")


def find_innerrag():
    """The first candidate whose /api/health answers; the Docker port when none does yet (the bridge
    may start at logon, before the container)."""
    for url in SERVER_CANDIDATES:
        try:
            with urlopen(f"{url}/api/health", timeout=1) as res:
                if res.status == 200:
                    return url
        except OSError:
            pass
    return SERVER_CANDIDATES[0]


def origin_of(url):
    parts = urlsplit(url)
    return f"{parts.scheme}://{parts.netloc}".rstrip("/")


def allowed_origins(innerrag):
    """The interface's own origin, its localhost/127.0.0.1 twin, and the Vite dev server."""
    base = origin_of(innerrag)
    out = {base, "http://localhost:5173", "http://127.0.0.1:5173"}
    for a, b in (("localhost", "127.0.0.1"), ("127.0.0.1", "localhost")):
        if f"//{a}" in base:
            out.add(base.replace(f"//{a}", f"//{b}"))
    return out


def launcher(executable):
    """The command line that starts an agent. On Windows npm installs `claude.cmd` and `copilot.cmd`
    shims, and running a .cmd goes through cmd.exe, which cuts the command line at the first line
    break: the multi-line instructions were truncated, and every argument after them (--resume,
    --disallowedTools, Copilot's question) lost. The program the shim calls is started directly."""
    if not executable:
        return None
    path = Path(executable)
    if os.name != "nt" or path.suffix.lower() not in (".cmd", ".bat"):
        return [executable]
    try:
        script = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return [executable]
    # npm shims end with: "%dp0%\node_modules\<package>\<entry>.exe|.js" %*
    targets = re.findall(r'"%dp0%\\([^"]+?\.(?:exe|js|cjs|mjs))"', script, re.IGNORECASE)
    if not targets:
        return [executable]
    target = path.parent / targets[-1]
    if not target.exists():
        return [executable]
    if target.suffix.lower() == ".exe":
        return [str(target)]
    node = path.parent / "node.exe"
    return [str(node) if node.exists() else (shutil.which("node") or "node"), str(target)]


def version_of(command):
    if not command:
        return None
    try:
        out = subprocess.run(
            [*command, "--version"], capture_output=True, text=True, timeout=30, stdin=subprocess.DEVNULL
        )
        lines = out.stdout.strip().splitlines()
        return lines[0].strip() if out.returncode == 0 and lines else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def find_copilot(name):
    """The Copilot CLI itself: VS Code puts a shim first in PATH that prompts and re-launches it."""
    if os.sep in name or (os.altsep and os.altsep in name):
        return name if Path(name).exists() else None
    names = [name] if os.name != "nt" else [name + ".cmd", name + ".exe", name]
    for folder in os.environ.get("PATH", "").split(os.pathsep):
        for n in names:
            candidate = Path(folder) / n
            if candidate.is_file() and "copilotCli" not in str(candidate) and os.access(candidate, os.X_OK):
                return str(candidate)
    return shutil.which(name)


class Bridge:
    def __init__(self, args):
        self.args = args
        self.claude = launcher(shutil.which(args.claude) or args.claude)
        self.version = version_of(self.claude)
        self.copilot = launcher(find_copilot(args.copilot))
        self.copilot_version = version_of(self.copilot)
        self.origins = allowed_origins(args.innerrag)
        # A fixed working directory: the agents file their sessions by directory, and resuming
        # needs to find them again. Nothing of a host project leaks in (no CLAUDE.md, no AGENTS.md).
        self.workdir = Path(tempfile.gettempdir()) / "innerrag-chat"
        self.workdir.mkdir(parents=True, exist_ok=True)
        self.tools = READ_TOOLS + (WRITE_TOOLS if args.allow_writes else [])

    def available(self, agent):
        return (self.version if agent == "claude" else self.copilot_version) is not None

    def instructions(self, project, strict, conversation):
        return SYSTEM_PROMPT.format(project=project, grounding=STRICT if strict else OPEN, conversation=conversation)

    def mcp_url(self, project):
        return f"{self.args.innerrag.rstrip('/')}/mcp/{project}"

    def claude_command(self, project, session, model, strict, conversation):
        mcp = {"mcpServers": {"innerrag": {"type": "http", "url": self.mcp_url(project)}}}
        cmd = [
            *self.claude, "-p",
            "--output-format", "stream-json", "--verbose", "--include-partial-messages",
            "--tools", "",
            "--mcp-config", json.dumps(mcp), "--strict-mcp-config",
            "--allowedTools", ",".join(f"mcp__innerrag__{t}" for t in self.tools),
            "--setting-sources", "",
            "--append-system-prompt", self.instructions(project, strict, conversation),
        ]
        if not self.args.allow_writes:
            cmd += ["--disallowedTools", ",".join(f"mcp__innerrag__{t}" for t in WRITE_TOOLS)]
        model = model or self.args.model
        if model:
            cmd += ["--model", model]
        if session:
            cmd += ["--resume", session]
        return cmd

    def copilot_command(self, project, session, model, strict, conversation, message):
        mcp = {"mcpServers": {"innerrag": {"type": "http", "url": self.mcp_url(project), "tools": ["*"]}}}
        # JSON output is still experimental in Copilot CLI 1.0 and must follow --experimental.
        cmd = [
            *self.copilot, "--experimental", "--output-format", "json",
            "--additional-mcp-config", json.dumps(mcp),
            # Only innerrag's tools are visible to the model: no shell, no file, no web.
            "--available-tools", ",".join(f"innerrag-{t}" for t in self.tools),
            "--allow-tool", "innerrag",
            "--disable-builtin-mcps", "--no-custom-instructions", "--no-ask-user",
        ]
        if not self.args.allow_writes:
            for t in WRITE_TOOLS:
                cmd += ["--deny-tool", f"innerrag({t})"]
        model = model or self.args.copilot_model
        if model:
            cmd += ["--model", model]
        if session:
            cmd.append(f"--resume={session}")
        # Copilot has no system prompt option: the instructions open every message.
        prompt = f"{self.instructions(project, strict, conversation)}\n\n---\nUser question:\n{message}"
        return cmd + ["-p", prompt]

    @staticmethod
    def claude_env():
        env = dict(os.environ)
        for key in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"):
            env.pop(key, None)
        return env


def tool_result_text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(c.get("text", "") for c in content if isinstance(c, dict) and c.get("type") == "text")
    return ""


def answer_text(delta, state):
    """Both agents tend to write their answer again after reporting its sources, often reworded:
    that text is held back, and only passed on (by `held_answer`) when no real answer came before."""
    if state.get("cited"):
        state["after"] = state.get("after", "") + delta
    else:
        state["said"] = state.get("said", "") + delta
        yield {"type": "text", "delta": delta}


def held_answer(state):
    after = state.get("after", "").strip()
    said = " ".join(state.get("said", "").split())
    # A short line before the citation ("Je cherche…") is not an answer: keep what follows.
    if after and len(said) < ANSWER_MIN_CHARS:
        yield {"type": "text", "delta": ("\n\n" if said else "") + after}


def translate_claude(event, state):
    """Claude Code stream-json events → the few events the interface needs."""
    kind = event.get("type")
    if kind == "system" and event.get("subtype") == "init":
        state["session"] = event.get("session_id")
        tools = [t for t in event.get("tools", []) if t.startswith("mcp__innerrag__")]
        servers = {s.get("name"): s.get("status") for s in event.get("mcp_servers", [])}
        yield {"type": "session", "id": state["session"], "model": event.get("model"),
               "tools": tools, "mcp": servers.get("innerrag")}
    elif kind == "stream_event":
        ev = event.get("event", {})
        if ev.get("type") == "content_block_delta" and ev.get("delta", {}).get("type") == "text_delta":
            yield from answer_text(ev["delta"].get("text", ""), state)
        elif ev.get("type") == "message_start":
            yield {"type": "turn"}
    elif kind == "assistant":
        for block in event.get("message", {}).get("content", []):
            if block.get("type") == "tool_use":
                if block.get("name", "").endswith("cite_sources"):
                    state["cited"] = True
                yield {"type": "tool", "id": block.get("id"), "name": block.get("name", "").removeprefix("mcp__innerrag__"),
                       "input": block.get("input", {})}
    elif kind == "user":
        content = event.get("message", {}).get("content", [])
        for block in content if isinstance(content, list) else []:
            if block.get("type") == "tool_result":
                yield {"type": "tool_result", "id": block.get("tool_use_id"),
                       "text": tool_result_text(block.get("content")), "is_error": bool(block.get("is_error"))}
    elif kind == "result":
        yield from held_answer(state)
        usage = event.get("usage", {})
        # The context the model holds now: everything the last call of the run read and wrote
        # (the conversation so far, the instructions, the tools, the passages).
        last = (usage.get("iterations") or [usage])[-1]
        context = sum(last.get(k) or 0 for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"))
        windows = [m.get("contextWindow") for m in (event.get("modelUsage") or {}).values() if isinstance(m, dict) and m.get("contextWindow")]
        yield {
            "type": "done",
            "ok": not event.get("is_error") and event.get("subtype") == "success",
            "error": None if event.get("subtype") == "success" else (event.get("result") or event.get("subtype")),
            "session": event.get("session_id"),
            "duration_ms": event.get("duration_ms"),
            "turns": event.get("num_turns"),
            "cost_usd": event.get("total_cost_usd"),
            "input_tokens": (usage.get("input_tokens") or 0) + (usage.get("cache_creation_input_tokens") or 0)
            + (usage.get("cache_read_input_tokens") or 0),
            "output_tokens": usage.get("output_tokens") or 0,
            "context_tokens": context or None,
            "context_window": max(windows) if windows else None,
        }


def translate_copilot(event, state):
    """GitHub Copilot CLI JSON events → the same events as for Claude Code."""
    kind = event.get("type")
    data = event.get("data") if isinstance(event.get("data"), dict) else {}
    if kind == "assistant.message_delta":
        yield from answer_text(data.get("deltaContent", ""), state)
    elif kind == "assistant.turn_start":
        state["turns"] = state.get("turns", 0) + 1
        yield {"type": "turn"}
    elif kind == "assistant.message":
        state["output_tokens"] = state.get("output_tokens", 0) + (data.get("outputTokens") or 0)
    elif kind == "tool.execution_start":
        name = data.get("mcpToolName") or str(data.get("toolName", "")).removeprefix("innerrag-")
        if name == "cite_sources":
            state["cited"] = True
        yield {"type": "tool", "id": data.get("toolCallId"), "name": name, "input": data.get("arguments") or {}}
    elif kind == "tool.execution_complete":
        result = data.get("result") if isinstance(data.get("result"), dict) else {}
        error = data.get("error") if isinstance(data.get("error"), dict) else {}
        text = result.get("content") or tool_result_text(result.get("contents")) or error.get("message", "")
        yield {"type": "tool_result", "id": data.get("toolCallId"), "text": str(text), "is_error": not data.get("success", True)}
    elif kind == "session.error":
        state["error"] = data.get("message") or "Copilot error"
    elif kind == "session.warning" and data.get("warningType") == "mcp" and "innerrag" in str(data.get("message", "")):
        # An organization policy that blocks MCP servers leaves Copilot without the knowledge base:
        # it would answer that the documents say nothing. Stop before the model is called.
        state["blocked"] = True
        yield {"type": "error", "code": "copilot_mcp_blocked",
               "message": f"GitHub Copilot: {data.get('message')}. Your organization's Copilot policy does not allow "
                          "this MCP server; ask its administrators to allow it, or use Claude Code."}
    elif kind == "result":
        yield from held_answer(state)
        usage = event.get("usage") or {}
        error = state.get("error")
        yield {
            "type": "done",
            "ok": event.get("exitCode", 0) == 0 and not error,
            "error": error,
            "session": event.get("sessionId"),
            "duration_ms": usage.get("sessionDurationMs"),
            "turns": state.get("turns", 0),
            "input_tokens": 0,
            "output_tokens": state.get("output_tokens", 0),
            "premium_requests": usage.get("premiumRequests"),
            "context_tokens": None,
            "context_window": None,
        }


class Handler(BaseHTTPRequestHandler):
    bridge: Bridge = None  # set in main()
    server_version = "innerrag-chat-bridge"

    def log_message(self, fmt, *args):
        sys.stderr.write("chat-bridge: " + fmt % args + "\n")

    def origin_ok(self):
        origin = self.headers.get("Origin")
        return origin is None or origin in self.bridge.origins

    def cors(self):
        origin = self.headers.get("Origin")
        if origin in self.bridge.origins:
            self.send_header("Access-Control-Allow-Origin", origin)
            self.send_header("Vary", "Origin")
            self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "Content-Type")
            # Chrome's private network access preflight.
            self.send_header("Access-Control-Allow-Private-Network", "true")

    def json(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.cors()
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_OPTIONS(self):
        if not self.origin_ok():
            return self.json(403, {"error": "origin not allowed"})
        self.send_response(204)
        self.cors()
        self.end_headers()

    def do_GET(self):
        if not self.origin_ok():
            return self.json(403, {"error": "origin not allowed"})
        if self.path.rstrip("/") in ("", "/health"):
            b = self.bridge
            return self.json(200, {
                "ok": b.version is not None or b.copilot_version is not None,
                "claude": b.version,
                "copilot": b.copilot_version,
                "innerrag": b.args.innerrag,
                "model": b.args.model,
                "copilot_model": b.args.copilot_model,
                "writes": b.args.allow_writes,
                "tools": b.tools,
            })
        self.json(404, {"error": "not found"})

    def do_POST(self):
        if not self.origin_ok():
            return self.json(403, {"error": "origin not allowed"})
        if self.path.rstrip("/") != "/chat":
            return self.json(404, {"error": "not found"})
        try:
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(min(length, 1_000_000)) or b"{}")
        except (ValueError, json.JSONDecodeError):
            return self.json(400, {"error": "invalid JSON"})
        project = str(body.get("project", ""))
        message = str(body.get("message", "")).strip()
        session = body.get("session") or None
        if not PROJECT_ID.match(project):
            return self.json(400, {"error": "invalid project id"})
        if not message or len(message) > MAX_MESSAGE:
            return self.json(400, {"error": f"the message must have 1 to {MAX_MESSAGE} characters"})
        if session is not None and not SESSION_ID.match(str(session)):
            return self.json(400, {"error": "invalid session id"})
        model = body.get("model") or None
        if model is not None and not MODEL.match(str(model)):
            return self.json(400, {"error": "invalid model name"})
        strict = body.get("strict", True) is not False
        conversation = str(body.get("conversation") or "default")
        if not CONVERSATION_ID.match(conversation):
            return self.json(400, {"error": "invalid conversation id"})
        agent = str(body.get("agent") or "claude")
        if agent not in AGENTS:
            return self.json(400, {"error": f"unknown agent {agent}: use claude or copilot"})
        if not self.bridge.available(agent):
            name = self.bridge.args.claude if agent == "claude" else self.bridge.args.copilot
            return self.json(503, {"error": f"`{name}` not found or not working on this machine"})
        self.stream(agent, project, message, session, model, strict, conversation)

    def stream(self, agent, project, message, session, model, strict, conversation):
        self.send_response(200)
        self.cors()
        self.send_header("Content-Type", "application/x-ndjson")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Accel-Buffering", "no")
        self.end_headers()
        self.close_connection = True

        b = self.bridge
        if agent == "copilot":
            cmd = b.copilot_command(project, session, model, strict, conversation, message)
            env, translate = dict(os.environ), translate_copilot
        else:
            cmd = b.claude_command(project, session, model, strict, conversation)
            env, translate = b.claude_env(), translate_claude
        try:
            proc = subprocess.Popen(
                cmd, cwd=b.workdir, env=env, stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1,
            )
        except OSError as e:
            self.send({"type": "error", "message": f"cannot start {agent}: {e}"})
            return
        # Claude reads the message on stdin, never on the command line; Copilot only takes -p.
        if agent == "claude":
            proc.stdin.write(message)
        proc.stdin.close()
        errors = []
        threading.Thread(target=lambda: errors.extend(proc.stderr.readlines()), daemon=True).start()

        state = {"session": session, "done": False}
        try:
            for line in proc.stdout:
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                for out in translate(event, state):
                    state["done"] |= out["type"] in ("done", "error")
                    self.send(out)
                # A blocking error (Copilot without the knowledge base): stop the agent now.
                if state.get("blocked"):
                    break
            proc.wait()
            if not state["done"]:
                detail = state.get("error") or "".join(errors).strip()[-800:] or f"{agent} exited with code {proc.returncode}"
                self.send({"type": "error", "message": detail})
        except (BrokenPipeError, ConnectionResetError):
            # The page stopped the answer or went away.
            self.log_message("client went away, stopping %s", agent)
        finally:
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()

    def send(self, event):
        self.wfile.write((json.dumps(event, ensure_ascii=False) + "\n").encode("utf-8"))
        self.wfile.flush()


def main():
    p = argparse.ArgumentParser(description="Bridge between the innerrag interface and the local coding agents.")
    p.add_argument("--innerrag", default=os.environ.get("INNERRAG_URL"),
                   help="URL of the innerrag server, as seen from this machine "
                        f"(default: the first of {', '.join(SERVER_CANDIDATES)} that answers)")
    p.add_argument("--port", type=int, default=int(os.environ.get("INNERRAG_CHAT_PORT", "18765")),
                   help="port to listen on, on 127.0.0.1 (default: %(default)s)")
    p.add_argument("--model", default=os.environ.get("INNERRAG_CHAT_MODEL"),
                   help="Claude model alias or id (default: the one of your Claude Code settings)")
    p.add_argument("--claude", default="claude", help="Claude Code executable (default: %(default)s)")
    p.add_argument("--copilot", default="copilot", help="GitHub Copilot CLI executable (default: %(default)s)")
    p.add_argument("--copilot-model", default=os.environ.get("INNERRAG_COPILOT_MODEL"),
                   help="Copilot model (default: the one of your Copilot settings)")
    p.add_argument("--allow-writes", action="store_true", help="also let the agent ingest documents")
    args = p.parse_args()
    args.innerrag = args.innerrag or find_innerrag()

    bridge = Bridge(args)
    if bridge.version is None and bridge.copilot_version is None:
        sys.exit("Neither Claude Code nor GitHub Copilot CLI works on this machine: install one and log in first.")
    Handler.bridge = bridge
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"innerrag chat bridge on http://127.0.0.1:{args.port}")
    print(f"  Claude Code: {bridge.version or 'not found'}")
    print(f"  GitHub Copilot CLI: {bridge.copilot_version or 'not found'}" + (f" ({' '.join(bridge.copilot)})" if bridge.copilot_version else ""))
    print(f"  innerrag at {args.innerrag}")
    print(f"  pages allowed: {', '.join(sorted(bridge.origins))}")
    print("  Ctrl+C to stop")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
