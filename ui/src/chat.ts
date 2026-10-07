// Client of the chat bridge (scripts/chat-bridge.py), which runs on the host next to a
// logged-in Claude Code and streams its answers as newline-delimited JSON.

export const DEFAULT_BRIDGE = "http://127.0.0.1:18765";

export interface BridgeHealth {
  ok: boolean;
  claude: string | null;
  innerrag: string;
  model: string | null;
  writes: boolean;
  tools: string[];
}

export type BridgeEvent =
  | { type: "session"; id: string; model?: string; tools: string[]; mcp?: string }
  | { type: "turn" }
  | { type: "text"; delta: string }
  | { type: "tool"; id: string; name: string; input: Record<string, unknown> }
  | { type: "tool_result"; id: string; text: string; is_error: boolean }
  | {
      type: "done";
      ok: boolean;
      error: string | null;
      session: string;
      duration_ms: number;
      turns: number;
      input_tokens: number;
      output_tokens: number;
    }
  | { type: "error"; message: string };

export async function health(bridge: string, signal?: AbortSignal): Promise<BridgeHealth> {
  const res = await fetch(`${bridge}/health`, { signal });
  if (!res.ok) throw new Error(`le pont répond ${res.status}`);
  return res.json();
}

/** Sends one message and calls `onEvent` for each event until Claude has finished. */
export async function send(
  bridge: string,
  body: { project: string; message: string; session?: string | null },
  onEvent: (e: BridgeEvent) => void,
  signal: AbortSignal,
): Promise<void> {
  const res = await fetch(`${bridge}/chat`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal,
  });
  if (!res.ok || !res.body) {
    const detail = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(detail.error ?? `le pont répond ${res.status}`);
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let nl: number;
    while ((nl = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (line) onEvent(JSON.parse(line) as BridgeEvent);
    }
  }
}
