#!/usr/bin/env python3
"""Bridge between the innerrag interface and the Claude Code installed on this machine.

The innerrag server runs in Docker and cannot start the `claude` of the host. This script runs
on the host, next to a logged-in Claude Code, and lets the "Assistant" page of the interface
chat with it: each message starts `claude -p`, connected to the innerrag MCP server of the
current project, and its events are streamed back to the page.

No API key is used: ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN are removed from the
environment of `claude`, which therefore uses the account you logged in with (`claude`, then
/login). The messages count against that subscription.

Claude only gets the read tools of innerrag: no shell, no file access, no ingestion
(unless --allow-writes). The bridge listens on 127.0.0.1 and only answers pages served by the
innerrag interface (Origin check), so another website cannot drive it.

Usage:
    python3 scripts/chat-bridge.py [--innerrag http://localhost:18080] [--port 18765] [--model sonnet]

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

READ_TOOLS = [
    "search_knowledge",
    "explore_entity",
    "explore_relation",
    "list_documents",
    "graph_stats",
    "run_cypher",
    "list_projects",
    "ingestion_status",
]
WRITE_TOOLS = ["ingest_document", "ingest_file"]
PROJECT_ID = re.compile(r"^[a-z0-9][a-z0-9_-]{0,63}$")
SESSION_ID = re.compile(r"^[0-9a-f-]{36}$")
MAX_MESSAGE = 20_000

SYSTEM_PROMPT = """You are the assistant of an innerrag knowledge base, project `{project}`.
Answer from that base: call the innerrag tools (search_knowledge first, explore_entity and
explore_relation to follow the graph) before answering, and say so plainly when the base does
not contain the answer instead of answering from general knowledge. Cite your sources as
"document title, page N" when the passage gives a page. Keep answers short and structured,
in Markdown, in the language of the question."""


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


def claude_version(claude):
    try:
        out = subprocess.run([claude, "--version"], capture_output=True, text=True, timeout=20)
        return out.stdout.strip() or None
    except (OSError, subprocess.TimeoutExpired):
        return None


class Bridge:
    def __init__(self, args):
        self.args = args
        self.claude = shutil.which(args.claude) or args.claude
        self.version = claude_version(self.claude)
        self.origins = allowed_origins(args.innerrag)
        # A fixed working directory: Claude Code files sessions by directory, and --resume needs
        # to find them again. Nothing of the host project leaks in (no CLAUDE.md there).
        self.workdir = Path(tempfile.gettempdir()) / "innerrag-chat"
        self.workdir.mkdir(parents=True, exist_ok=True)
        tools = READ_TOOLS + (WRITE_TOOLS if args.allow_writes else [])
        self.allowed = [f"mcp__innerrag__{t}" for t in tools]
        self.denied = [] if args.allow_writes else [f"mcp__innerrag__{t}" for t in WRITE_TOOLS]

    def command(self, project, session):
        mcp = {
            "mcpServers": {
                "innerrag": {"type": "http", "url": f"{self.args.innerrag.rstrip('/')}/mcp/{project}"}
            }
        }
        cmd = [
            self.claude, "-p",
            "--output-format", "stream-json", "--verbose", "--include-partial-messages",
            "--tools", "",
            "--mcp-config", json.dumps(mcp), "--strict-mcp-config",
            "--allowedTools", ",".join(self.allowed),
            "--setting-sources", "",
            "--append-system-prompt", SYSTEM_PROMPT.format(project=project),
        ]
        if self.denied:
            cmd += ["--disallowedTools", ",".join(self.denied)]
        if self.args.model:
            cmd += ["--model", self.args.model]
        if session:
            cmd += ["--resume", session]
        return cmd

    @staticmethod
    def env():
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


def translate(event, state):
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
            yield {"type": "text", "delta": ev["delta"].get("text", "")}
        elif ev.get("type") == "message_start":
            yield {"type": "turn"}
    elif kind == "assistant":
        for block in event.get("message", {}).get("content", []):
            if block.get("type") == "tool_use":
                yield {"type": "tool", "id": block.get("id"), "name": block.get("name", "").removeprefix("mcp__innerrag__"),
                       "input": block.get("input", {})}
    elif kind == "user":
        content = event.get("message", {}).get("content", [])
        for block in content if isinstance(content, list) else []:
            if block.get("type") == "tool_result":
                yield {"type": "tool_result", "id": block.get("tool_use_id"),
                       "text": tool_result_text(block.get("content")), "is_error": bool(block.get("is_error"))}
    elif kind == "result":
        usage = event.get("usage", {})
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
                "ok": b.version is not None,
                "claude": b.version,
                "innerrag": b.args.innerrag,
                "model": b.args.model,
                "writes": b.args.allow_writes,
                "tools": [t.removeprefix("mcp__innerrag__") for t in b.allowed],
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
        if self.bridge.version is None:
            return self.json(503, {"error": f"`{self.bridge.args.claude}` not found or not working on this machine"})
        self.stream(project, message, session)

    def stream(self, project, message, session):
        self.send_response(200)
        self.cors()
        self.send_header("Content-Type", "application/x-ndjson")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Accel-Buffering", "no")
        self.end_headers()
        self.close_connection = True

        cmd = self.bridge.command(project, session)
        try:
            proc = subprocess.Popen(
                cmd, cwd=self.bridge.workdir, env=self.bridge.env(), stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1,
            )
        except OSError as e:
            self.send({"type": "error", "message": f"cannot start claude: {e}"})
            return
        # The message goes through stdin, never through the command line.
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
                    state["done"] |= out["type"] == "done"
                    self.send(out)
            proc.wait()
            if not state["done"]:
                detail = "".join(errors).strip()[-800:] or f"claude exited with code {proc.returncode}"
                self.send({"type": "error", "message": detail})
        except (BrokenPipeError, ConnectionResetError):
            # The page stopped the answer or went away.
            self.log_message("client went away, stopping claude")
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
    p = argparse.ArgumentParser(description="Bridge between the innerrag interface and the local Claude Code.")
    p.add_argument("--innerrag", default=os.environ.get("INNERRAG_URL", "http://localhost:18080"),
                   help="URL of the innerrag server, as seen from this machine (default: %(default)s)")
    p.add_argument("--port", type=int, default=int(os.environ.get("INNERRAG_CHAT_PORT", "18765")),
                   help="port to listen on, on 127.0.0.1 (default: %(default)s)")
    p.add_argument("--model", default=os.environ.get("INNERRAG_CHAT_MODEL"),
                   help="Claude model alias or id (default: the one of your Claude Code settings)")
    p.add_argument("--claude", default="claude", help="claude executable (default: %(default)s)")
    p.add_argument("--allow-writes", action="store_true", help="also let Claude ingest documents")
    args = p.parse_args()

    bridge = Bridge(args)
    if bridge.version is None:
        sys.exit(f"`{args.claude}` not found or not working. Install Claude Code and log in with `claude` first.")
    Handler.bridge = bridge
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"innerrag chat bridge on http://127.0.0.1:{args.port}")
    print(f"  Claude Code {bridge.version}, innerrag at {args.innerrag}")
    print(f"  pages allowed: {', '.join(sorted(bridge.origins))}")
    print("  Ctrl+C to stop")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
