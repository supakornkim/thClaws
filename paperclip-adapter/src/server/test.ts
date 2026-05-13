/**
 * `testEnvironment` — Paperclip's diagnostic probe. Called from the
 * Settings → Adapter page so users can verify thClaws is reachable
 * before assigning it to an agent.
 *
 * Returns an `AdapterEnvironmentTestResult` shaped as
 * `{adapterType, status, checks, testedAt}` where each check is
 * `{code, level: "info"|"warn"|"error", message, detail?, hint?}`.
 * Overall `status` is the max severity across the checks.
 *
 * MVP — two checks: (1) `cwd` exists + executable, (2) `thclaws
 * --version` is callable. Provider auth / model availability is
 * NOT verified here; thClaws does its own keychain / .env
 * discovery on each run, so a missing API key surfaces at execute
 * time, not in the diagnostic.
 */

import { spawn } from "node:child_process";
import { access, constants } from "node:fs/promises";
import type {
  AdapterEnvironmentCheck,
  AdapterEnvironmentTestContext,
  AdapterEnvironmentTestResult,
  AdapterEnvironmentTestStatus,
} from "@paperclipai/adapter-utils";

const ADAPTER_TYPE = "thclaws_local";

export async function testEnvironment(
  ctx: AdapterEnvironmentTestContext,
): Promise<AdapterEnvironmentTestResult> {
  const config = (ctx.config ?? {}) as Record<string, unknown>;
  const command =
    typeof config.command === "string" && config.command.trim().length > 0
      ? config.command
      : "thclaws";
  const cwd =
    typeof config.cwd === "string" && config.cwd.trim().length > 0
      ? config.cwd
      : process.cwd();

  const checks: AdapterEnvironmentCheck[] = [];

  // Step 1: cwd accessibility.
  try {
    await access(cwd, constants.R_OK | constants.X_OK);
    checks.push({
      code: "cwd_check",
      level: "info",
      message: `cwd accessible: ${cwd}`,
    });
  } catch (err) {
    checks.push({
      code: "cwd_check",
      level: "error",
      message: `cwd not accessible: ${cwd}`,
      detail: (err as Error).message,
      hint: "Set `cwd` to an absolute path the Paperclip process can chdir into.",
    });
    return finalize(checks);
  }

  // Step 2: thclaws --version probe.
  const version = await probeVersion(command);
  if (!version.ok) {
    checks.push({
      code: "thclaws_version",
      level: "error",
      message: version.message,
      detail: version.detail ?? null,
      hint: "Install thClaws (https://github.com/thClaws/thClaws) and ensure the binary is on $PATH, or set `command` to its absolute path.",
    });
    return finalize(checks);
  }
  checks.push({
    code: "thclaws_version",
    level: "info",
    message: `thClaws available: ${version.version}`,
  });

  return finalize(checks);
}

function finalize(
  checks: AdapterEnvironmentCheck[],
): AdapterEnvironmentTestResult {
  // Status = worst level across checks. "error" > "warn" > "info".
  let status: AdapterEnvironmentTestStatus = "pass";
  for (const c of checks) {
    if (c.level === "error") {
      status = "fail";
      break;
    }
    if (c.level === "warn" && status === "pass") {
      status = "warn";
    }
  }
  return {
    adapterType: ADAPTER_TYPE,
    status,
    checks,
    testedAt: new Date().toISOString(),
  };
}

type ProbeResult =
  | { ok: true; version: string }
  | { ok: false; message: string; detail?: string };

function probeVersion(command: string): Promise<ProbeResult> {
  return new Promise((resolve) => {
    const child = spawn(command, ["--version"], { stdio: "pipe" });
    let stdout = "";
    let stderr = "";

    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      resolve({
        ok: false,
        message: `${command} --version timed out after 5s`,
      });
    }, 5000);

    child.stdout?.on("data", (b) => (stdout += b.toString("utf8")));
    child.stderr?.on("data", (b) => (stderr += b.toString("utf8")));

    child.on("error", (err) => {
      clearTimeout(timer);
      resolve({
        ok: false,
        message: `cannot run ${command}: ${(err as Error).message}`,
      });
    });

    child.on("close", (code) => {
      clearTimeout(timer);
      if (code !== 0) {
        resolve({
          ok: false,
          message: `${command} --version exited ${code}`,
          detail: stderr.trim() || undefined,
        });
        return;
      }
      const version = (stdout || stderr).trim();
      if (!version) {
        resolve({
          ok: false,
          message: `${command} --version produced no output`,
        });
        return;
      }
      resolve({ ok: true, version });
    });
  });
}
