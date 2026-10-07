#!/usr/bin/env node
// Captures the README screenshots from a running innerrag (and, for the assistant, its chat
// bridge) with a headless Chrome driven over the DevTools protocol. Node 22+, no dependency.
//
//   node scripts/screenshots.mjs [--url http://localhost:18080] [--out docs/screenshots] [--lang fr] [--only carte,mcp]
//
// Each shot opens a page in a fresh tab with the project and language set in localStorage,
// optionally acts on it (click, type), waits, then saves a PNG.

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1]]] : acc), []),
);
const BASE = (args.url ?? "http://localhost:18080").replace(/\/$/, "");
const OUT = args.out ?? "docs/screenshots";
const LANG = args.lang ?? "fr";
const ONLY = args.only ? new Set(args.only.split(",")) : null;
const WIDTH = 1440;
const HEIGHT = 900;

const CHROME = [
  process.env.CHROME,
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
].find((p) => p && existsSync(p));
if (!CHROME) throw new Error("Chrome not found: set CHROME=/path/to/chrome");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** The page to capture: route, project, then actions in the page (JS run with helpers). */
/** Questions typed in the search and assistant shots, in the interface language. */
const ASK = {
  fr: { search: "Comment annuler une AsyncTask ?", assistant: "Quel rapport entre ACTION_MOVE et ACTION_UP ?" },
  en: { search: "How do I cancel an AsyncTask?", assistant: "How are ACTION_MOVE and ACTION_UP related?" },
}[LANG] ?? { search: "How do I cancel an AsyncTask?", assistant: "How are ACTION_MOVE and ACTION_UP related?" };

const SHOTS = [
  { name: "map", project: "sample", route: "#/carte", wait: 14000 },
  {
    name: "link",
    project: "sample",
    route: "#/carte?entity=technology%3Ahttpclient",
    wait: 9000,
    act: `(await waitFor(() => document.querySelector(".link-score"))).click(); await sleep(1500);`,
  },
  { name: "documents", project: "sample", route: "#/documents", wait: 2500 },
  { name: "reader", project: "sample", route: "#/lire?doc=5a1e8333-6e5d-404c-80c1-3a6d3c70b2c4&page=324", wait: 3500 },
  {
    name: "search",
    project: "sample",
    route: "#/recherche",
    wait: 1500,
    act: `await type("input[type=search], input", ${JSON.stringify(ASK.search)}); await press("Enter"); await sleep(4000);`,
  },
  {
    name: "assistant",
    project: "sample",
    route: "#/assistant",
    wait: 3000,
    act: `await type("#chat-input", ${JSON.stringify(ASK.assistant)}); await press("Enter"); await waitFor(() => document.querySelector(".chat-meta"), 120000); await sleep(800); window.scrollTo(0, 0);`,
  },
  { name: "gaps", project: "sample", route: "#/lacunes", wait: 3000 },
  { name: "mcp", project: "sample", route: "#/mcp?tool=search_knowledge", wait: 2000 },
  { name: "history", project: "sample", route: "#/historique", wait: 2500 },
].filter((s) => !ONLY || ONLY.has(s.name));

// Helpers available to the `act` snippets, evaluated in the page.
const HELPERS = `
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const waitFor = async (fn, timeout = 15000) => {
    const end = Date.now() + timeout;
    while (Date.now() < end) { const v = fn(); if (v) return v; await sleep(200); }
    throw new Error("timeout");
  };
  const type = async (selector, text) => {
    const el = await waitFor(() => document.querySelector(selector));
    el.focus();
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value").set;
    setter.call(el, text);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const press = async (key) => {
    const el = document.activeElement;
    // Like a browser: Enter submits the form unless the page handled the key itself.
    const down = new KeyboardEvent("keydown", { key, code: key, bubbles: true, cancelable: true });
    if (el.dispatchEvent(down) && key === "Enter" && el.form && el.tagName !== "TEXTAREA") el.form.requestSubmit();
  };
  const clickText = async (selector, text) => {
    const el = await waitFor(() => [...document.querySelectorAll(selector)].find((e) => e.textContent.trim().startsWith(text)));
    el.click();
  };
`;

class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.id = 0;
    this.pending = new Map();
    this.ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
      }
    };
  }
  open() {
    return new Promise((resolve, reject) => {
      this.ws.onopen = resolve;
      this.ws.onerror = reject;
    });
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params, sessionId }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
}

async function main() {
  mkdirSync(OUT, { recursive: true });
  const profile = mkdtempSync(join(tmpdir(), "innerrag-shots-"));
  const port = 9300 + Math.floor(Math.random() * 500);
  const chrome = spawn(CHROME, [
    "--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, `--lang=${LANG}`,
    "--hide-scrollbars", "--no-first-run", "--no-default-browser-check", `--window-size=${WIDTH},${HEIGHT}`,
  ], { stdio: "ignore" });
  try {
    let version;
    for (let i = 0; i < 50 && !version; i++) {
      await sleep(200);
      version = await fetch(`http://127.0.0.1:${port}/json/version`).then((r) => r.json()).catch(() => null);
    }
    const cdp = new Cdp(version.webSocketDebuggerUrl);
    await cdp.open();
    for (const shot of SHOTS) {
      const { targetId } = await cdp.send("Target.createTarget", { url: "about:blank" });
      const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
      const page = (m, p) => cdp.send(m, p, sessionId);
      await page("Page.enable");
      // A background tab gets throttled animation frames: the map would never settle.
      await page("Emulation.setFocusEmulationEnabled", { enabled: true });
      await page("Page.bringToFront");
      await page("Emulation.setDeviceMetricsOverride", { width: WIDTH, height: HEIGHT, deviceScaleFactor: 1, mobile: false });
      await page("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "light" }] });
      // Same origin first, to seed localStorage before the app reads it.
      await page("Page.navigate", { url: `${BASE}/` });
      await sleep(800);
      await page("Runtime.evaluate", {
        expression: `localStorage.setItem("innerrag.project", ${JSON.stringify(shot.project)});
                     localStorage.setItem("innerrag.lang", ${JSON.stringify(LANG)});
                     localStorage.setItem("innerrag.theme", "light");`,
      });
      await page("Page.navigate", { url: `${BASE}/?shot=${shot.name}${shot.route}` });
      await sleep(shot.wait);
      if (shot.act) {
        const r = await page("Runtime.evaluate", {
          expression: `(async () => { ${HELPERS} ${shot.act} })()`,
          awaitPromise: true,
        });
        if (r.exceptionDetails) console.warn(`${shot.name}: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
        await sleep(600);
      }
      const { data } = await page("Page.captureScreenshot", { format: "png" });
      writeFileSync(join(OUT, `${shot.name}.png`), Buffer.from(data, "base64"));
      console.log(`${shot.name}.png`);
      await cdp.send("Target.closeTarget", { targetId });
    }
    cdp.ws.close();
  } finally {
    chrome.kill();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
