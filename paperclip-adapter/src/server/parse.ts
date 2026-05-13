/**
 * Output parsing helpers — MVP scope.
 *
 * thClaws's `-p` print mode emits plain text to stdout: the
 * assistant's reply followed by a one-line `[tokens: Xin/Yout · Ts]`
 * summary. No structured event stream yet (the stream-json flag is
 * declared in the CLI but not wired through run_print_mode at time
 * of this commit).
 *
 * The execute path captures stdout verbatim into the
 * `AdapterExecutionResult.summary` field and Paperclip's transcript
 * view renders it as one block. This file exists so a future
 * stream-json implementation has a natural home — for now its only
 * exported function is `extractTokenSummary` so callers can pull
 * the trailing `[tokens: …]` line out of the text body if they want
 * to surface it separately in their UI.
 */

const TOKEN_SUMMARY_RE = /\[tokens:\s*(\d+)in\/(\d+)out(?:\s*·\s*([\d.]+)s)?\]\s*$/m;

export interface TokenSummary {
  inputTokens: number;
  outputTokens: number;
  durationSec: number | null;
}

/**
 * Pull the trailing `[tokens: 1234in/567out · 12.3s]` line out of a
 * thClaws stdout blob. Returns `null` when the line isn't present
 * (e.g. the run died before the summary fired).
 */
export function extractTokenSummary(stdout: string): TokenSummary | null {
  const match = stdout.match(TOKEN_SUMMARY_RE);
  if (!match) return null;
  const [, inStr, outStr, durStr] = match;
  return {
    inputTokens: Number.parseInt(inStr, 10),
    outputTokens: Number.parseInt(outStr, 10),
    durationSec: durStr ? Number.parseFloat(durStr) : null,
  };
}

/**
 * Strip the trailing token-summary line so the text body shown in
 * the transcript doesn't end with metadata noise. Safe to call on
 * stdout that doesn't have a summary — returns the input unchanged.
 */
export function stripTokenSummary(stdout: string): string {
  return stdout.replace(TOKEN_SUMMARY_RE, "").trimEnd();
}
