/**
 * Spawn `thclaws -p <prompt>` and stream the result back to Paperclip.
 *
 * MVP — captures stdout/stderr verbatim, no incremental tool-call
 * parsing. thClaws's `--output-format stream-json` flag exists in the
 * CLI declaration but isn't wired through `run_print_mode` yet (see
 * thclaws/crates/core/src/repl.rs:3542), so plain text mode is what
 * we have to work with for v0.1. Once thClaws ships stream-json
 * emit, this file gets a richer event-by-event parser.
 */

import { spawn } from "node:child_process";
import type {
  AdapterExecutionContext,
  AdapterExecutionResult,
} from "@paperclipai/adapter-utils";

/**
 * Read a string field off the loose `config` record without throwing
 * on missing / wrong-type input. Mirrors the helper signature
 * Paperclip's own adapter-utils ship (asString) — duplicated locally
 * so the MVP doesn't add a hard dependency on a specific utils
 * version's export surface.
 */
function asString(
  config: Record<string, unknown>,
  key: string,
  fallback: string,
): string {
  const v = config[key];
  return typeof v === "string" && v.trim().length > 0 ? v : fallback;
}

function asNumber(
  config: Record<string, unknown>,
  key: string,
  fallback: number,
): number {
  const v = config[key];
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function asStringArray(
  config: Record<string, unknown>,
  key: string,
): string[] {
  const v = config[key];
  if (!Array.isArray(v)) return [];
  return v.filter((x): x is string => typeof x === "string" && x.length > 0);
}

function asEnvRecord(
  config: Record<string, unknown>,
  key: string,
): Record<string, string> {
  const v = config[key];
  if (!v || typeof v !== "object" || Array.isArray(v)) return {};
  const out: Record<string, string> = {};
  for (const [k, val] of Object.entries(v)) {
    if (typeof val === "string") out[k] = val;
  }
  return out;
}

export async function execute(
  ctx: AdapterExecutionContext,
): Promise<AdapterExecutionResult> {
  const config = (ctx.config ?? {}) as Record<string, unknown>;

  const command = asString(config, "command", "thclaws");
  const model = asString(config, "model", "claude-sonnet-4-6");
  const extraArgs = asStringArray(config, "extraArgs");
  const cwd = asString(config, "cwd", process.cwd());
  const timeoutSec = asNumber(config, "timeoutSec", 0);
  const envOverrides = asEnvRecord(config, "env");
  const promptTemplate = asString(config, "promptTemplate", "{{prompt}}");

  // The prompt the Paperclip task runner wants the agent to act on
  // lives at `ctx.context.prompt` (string). Apply the optional
  // template before passing to thclaws.
  const rawPrompt =
    typeof (ctx.context as Record<string, unknown>)?.prompt === "string"
      ? ((ctx.context as Record<string, unknown>).prompt as string)
      : "";
  const prompt = renderTemplate(promptTemplate, { prompt: rawPrompt });

  // Build argv. thClaws's `-p` flag is print-mode (single-turn,
  // non-interactive). `-m` selects the model. `extraArgs` lets
  // callers slip in --verbose / --max-tokens / --thinking-budget.
  const args = ["-p", prompt, "-m", model, ...extraArgs];

  const env: Record<string, string> = {
    ...process.env,
    ...envOverrides,
  } as Record<string, string>;

  // Pass through Paperclip workspace context as env vars so any
  // user-side hooks / skills can read PAPERCLIP_WORKSPACE_* (matches
  // the convention claude-local and the other built-in adapters use).
  for (const [k, v] of Object.entries(ctx.context ?? {})) {
    if (typeof v === "string" && k.startsWith("PAPERCLIP_")) {
      env[k] = v;
    }
  }

  // Filter env down to PAPERCLIP_* keys before recording — Paperclip
  // logs this blob and we don't want API keys in the audit trail.
  const safeEnv = Object.keys(env)
    .filter((k) => k.startsWith("PAPERCLIP_"))
    .reduce<Record<string, string>>((acc, k) => {
      acc[k] = env[k];
      return acc;
    }, {});

  await ctx.onMeta?.({
    adapterType: "thclaws_local",
    command,
    commandArgs: args,
    cwd,
    env: safeEnv,
    prompt: rawPrompt,
  });

  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd,
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdoutBuf = "";
    let stderrBuf = "";
    let timedOut = false;

    if (child.pid) {
      ctx.onSpawn?.({
        pid: child.pid,
        processGroupId: null,
        startedAt: new Date().toISOString(),
      }).catch(() => {});
    }

    child.stdout?.setEncoding("utf8");
    child.stdout?.on("data", (chunk: string) => {
      stdoutBuf += chunk;
      ctx.onLog("stdout", chunk).catch(() => {});
    });

    child.stderr?.setEncoding("utf8");
    child.stderr?.on("data", (chunk: string) => {
      stderrBuf += chunk;
      ctx.onLog("stderr", chunk).catch(() => {});
    });

    const timer =
      timeoutSec > 0
        ? setTimeout(() => {
            timedOut = true;
            child.kill("SIGTERM");
            // SIGKILL after 5s grace, matching claude-local behavior.
            setTimeout(() => {
              if (!child.killed) child.kill("SIGKILL");
            }, 5000).unref();
          }, timeoutSec * 1000)
        : null;

    child.on("error", (err) => {
      if (timer) clearTimeout(timer);
      const message = (err as Error).message ?? String(err);
      resolve({
        exitCode: null,
        signal: null,
        timedOut: false,
        errorMessage: `failed to spawn ${command}: ${message}`,
        errorCode: "spawn_failed",
        // `errorFamily` is restricted to "transient_upstream" — a
        // failed spawn isn't that, so leave it null.
        errorFamily: null,
        model,
        summary: stdoutBuf || null,
      });
    });

    child.on("close", (code, signal) => {
      if (timer) clearTimeout(timer);
      const exitCode = typeof code === "number" ? code : null;
      const sig = signal ? String(signal) : null;
      const summary = stdoutBuf.trim() || null;

      // thClaws prints the assistant's text to stdout and the
      // `[tokens: …]` summary line at the end. We surface the full
      // stdout as the `summary` field; Paperclip's transcript view
      // displays it under the agent's run.
      resolve({
        exitCode,
        signal: sig,
        timedOut,
        errorMessage:
          exitCode === 0
            ? null
            : timedOut
              ? `thclaws timed out after ${timeoutSec}s`
              : stderrBuf.trim() || `thclaws exited ${exitCode ?? "?"}`,
        errorCode:
          exitCode === 0 ? null : timedOut ? "timeout" : "non_zero_exit",
        // `errorFamily` only accepts "transient_upstream" in v0.1 of
        // the adapter-utils contract — that doesn't match either
        // case here, so leave it null.
        errorFamily: null,
        model,
        summary,
        resultJson: null,
      });
    });
  });
}

/**
 * Replace `{{key}}` placeholders in `template` with values from `data`.
 * No escaping — Paperclip is the trust boundary on prompt content.
 * Matches the minimal templating built-in adapters use.
 */
function renderTemplate(
  template: string,
  data: Record<string, string>,
): string {
  return template.replace(/\{\{(\w+)\}\}/g, (match, key: string) =>
    Object.prototype.hasOwnProperty.call(data, key) ? data[key] : match,
  );
}
