import { Plugin } from "@opencode/plugin";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

const DEFAULT_LISTEN = "127.0.0.1:8080";
const DEFAULT_UPSTREAM = "http://localhost:3000";

function parseProxyArgs(text: string): { listen: string; upstream: string } {
  const parts = (text ?? "").trim().split(/\s+/).filter(Boolean);
  let listen = DEFAULT_LISTEN;
  let upstream = DEFAULT_UPSTREAM;
  for (const p of parts) {
    if (/^\d+\.\d+\.\d+\.\d+:\d+$/.test(p) || /^localhost:\d+$/.test(p) || /^:\d+$/.test(p)) {
      listen = p.startsWith(":") ? `127.0.0.1${p}` : p;
    } else if (/^https?:\/\//.test(p)) {
      upstream = p;
    }
  }
  return { listen, upstream };
}

async function healthCheck(listen: string): Promise<string> {
  const base = (listen.includes("://") ? listen : `http://${listen}`).replace(/\/$/, "");
  try {
    const res = await fetch(`${base}/health`, { signal: AbortSignal.timeout(3000) });
    if (res.ok) return "running (health OK)";
    return `reachable but health=${res.status}`;
  } catch {
    try {
      const curl = process.platform === "win32" ? "curl.exe" : "curl";
      const { stdout } = await execFileAsync(curl, [
        "-m",
        "4",
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        `${base}/health`,
      ]);
      if (stdout.trim() === "200") return "running (health OK via curl)";
      return `reachable but health=${stdout.trim()} (via curl)`;
    } catch (e) {
      return `not reachable (${e instanceof Error ? e.message : String(e)})`;
    }
  }
}

async function waitForHealth(
  listen: string,
  isAlive: () => boolean,
  exitInfo: () => string,
): Promise<string> {
  for (;;) {
    const status = await healthCheck(listen);
    if (status.startsWith("running")) return status;
    if (!isAlive()) return `process exited before health passed (${exitInfo()}). Last probe: ${status}`;
    await sleep(1000);
  }
}

async function binaryVersion(): Promise<string | null> {
  try {
    const { stdout } = await execFileAsync("oplire", ["--version"]);
    return stdout.trim().split("\n")[0] ?? null;
  } catch {
    return null;
  }
}

function startProxy(listen: string, upstream: string): {
  pid: number | undefined;
  isAlive: () => boolean;
  exitInfo: () => string;
} {
  const child = spawn("oplire", ["proxy", "--listen", listen, "--upstream", upstream], {
    detached: true,
    stdio: "ignore",
  });
  let spawnError: string | null = null;
  child.on("error", (e) => {
    spawnError = e instanceof Error ? e.message : String(e);
  });
  child.unref();
  return {
    pid: child.pid,
    isAlive: () => spawnError === null && child.exitCode === null && child.signalCode === null,
    exitInfo: () =>
      spawnError !== null
        ? `spawn failed: ${spawnError}`
        : child.exitCode !== null
          ? `exit code ${child.exitCode}`
          : `signal ${child.signalCode}`,
  };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function stopProxy(): Promise<string> {
  const isWin = process.platform === "win32";
  try {
    if (isWin) {
      await execFileAsync("taskkill", ["/F", "/IM", "oplire.exe"]);
      return "sent stop signal (taskkill /F /IM oplire.exe)";
    }
    await execFileAsync("pkill", ["-f", "oplire proxy"]);
    return "sent stop signal (pkill -f 'oplire proxy')";
  } catch (e) {
    return `no matching oplire proxy process (${e instanceof Error ? e.message.split("\n")[0] : String(e)})`;
  }
}

export default Plugin.define({
  id: "oplire.proxy",
  async setup(ctx) {
    await ctx.command.transform((editor) => {
      editor.add({
        name: "proxy",
        description: "Start the oplire Responses proxy for OpenCode V2",
        execute: async ({ sessionID, prompt }) => {
          const { listen, upstream } = parseProxyArgs(prompt.text ?? "");
          const status = await healthCheck(listen);
          if (status.startsWith("running")) {
            await ctx.session.synthetic({
              sessionID,
              text: `oplire proxy already running at http://${listen} (${status}).`,
              resume: false,
            });
            return;
          }
          const version = await binaryVersion();
          if (!version) {
            await ctx.session.synthetic({
              sessionID,
              text: `oplire binary not found on PATH in the OpenCode server environment. Build it (cargo build --release) and ensure \`oplire\` is on PATH, or start it manually: \`oplire proxy --listen ${listen} --upstream ${upstream}\`.`,
              resume: false,
            });
            return;
          }
          const prox = startProxy(listen, upstream);
          await ctx.session.synthetic({
            sessionID,
            text: `oplire proxy starting (${version}${prox.pid ? `, pid ${prox.pid}` : ""}): http://${listen}/v1 -> ${upstream}. Waiting for health...`,
            resume: false,
          });
          const after = await waitForHealth(listen, prox.isAlive, prox.exitInfo);
          await ctx.session.synthetic({
            sessionID,
            text: after.startsWith("running")
              ? `oplire proxy running (${version}${prox.pid ? `, pid ${prox.pid}` : ""}): http://${listen}/v1 -> ${upstream}.`
              : `oplire proxy failed (${after}). Check OpenCode logs and port availability.`,
            resume: false,
          });
        },
      });

      editor.add({
        name: "proxy-status",
        description: "Show oplire proxy status",
        execute: async ({ sessionID, prompt }) => {
          const { listen } = parseProxyArgs(prompt.text ?? "");
          let status = await healthCheck(listen);
          for (let i = 0; i < 2 && !status.startsWith("running"); i++) {
            await sleep(500);
            status = await healthCheck(listen);
          }
          await ctx.session.synthetic({
            sessionID,
            text: `oplire proxy ${status} (listen http://${listen}).`,
            resume: false,
          });
        },
      });

      editor.add({
        name: "proxy-stop",
        description: "Stop the running oplire proxy",
        execute: async ({ sessionID }) => {
          const result = await stopProxy();
          await ctx.session.synthetic({
            sessionID,
            text: `oplire proxy stop: ${result}.`,
            resume: false,
          });
        },
      });
    });
  },
});
