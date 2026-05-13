/**
 * UI transcript parser — what Paperclip's React UI loads dynamically
 * to render a thClaws agent run.
 *
 * The contract (paperclip/docs/adapters/adapter-ui-parser.md) is a
 * default-exported parser function that consumes stdout/stderr
 * strings and returns a transcript model the UI can render. MVP
 * shape: one block per agent run, body = thclaws's stdout with the
 * `[tokens:]` summary line stripped, optional footer carrying the
 * extracted token counts.
 *
 * NO imports from node:* or @paperclipai/adapter-utils — this file
 * is loaded by the Paperclip browser bundle and must run in the
 * browser sandbox without leaking server-only dependencies.
 */

const TOKEN_SUMMARY_RE = /\[tokens:\s*(\d+)in\/(\d+)out(?:\s*·\s*([\d.]+)s)?\]\s*$/m;

interface ParserInput {
  stdout?: string;
  stderr?: string;
  exitCode?: number | null;
  signal?: string | null;
  errorMessage?: string | null;
}

interface TranscriptBlock {
  kind: "assistant" | "error" | "meta";
  text: string;
}

interface TranscriptResult {
  blocks: TranscriptBlock[];
  meta: {
    inputTokens?: number;
    outputTokens?: number;
    durationSec?: number;
  };
}

export default function parseUiTranscript(input: ParserInput): TranscriptResult {
  const stdout = input.stdout ?? "";
  const stderr = input.stderr ?? "";
  const blocks: TranscriptBlock[] = [];
  const meta: TranscriptResult["meta"] = {};

  const summaryMatch = stdout.match(TOKEN_SUMMARY_RE);
  if (summaryMatch) {
    meta.inputTokens = Number.parseInt(summaryMatch[1], 10);
    meta.outputTokens = Number.parseInt(summaryMatch[2], 10);
    if (summaryMatch[3]) meta.durationSec = Number.parseFloat(summaryMatch[3]);
  }

  const body = stdout.replace(TOKEN_SUMMARY_RE, "").trimEnd();
  if (body.length > 0) {
    blocks.push({ kind: "assistant", text: body });
  }

  // Surface stderr only when it's interesting — a clean run typically
  // leaves stderr empty or with retry-banner ANSI we'd just clutter
  // the transcript with. Heuristic: ignore stderr if exit code is 0
  // AND stderr has no obvious "error" / "failed" tokens.
  const stderrTrimmed = stderr.trim();
  if (stderrTrimmed.length > 0) {
    const looksLikeError =
      (input.exitCode ?? 0) !== 0 ||
      /\b(error|failed|panic|fatal)\b/i.test(stderrTrimmed);
    if (looksLikeError) {
      blocks.push({ kind: "error", text: stripAnsi(stderrTrimmed) });
    }
  }

  if (input.errorMessage) {
    blocks.push({ kind: "error", text: input.errorMessage });
  }

  if (blocks.length === 0) {
    blocks.push({
      kind: "meta",
      text: "(thClaws produced no output)",
    });
  }

  return { blocks, meta };
}

/**
 * Drop ANSI escape sequences from stderr before surfacing it in the
 * UI. thClaws's retry banners are yellow-coded; without this they'd
 * render as raw `\x1b[33m…\x1b[0m`.
 */
function stripAnsi(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1b\[[0-9;]*[A-Za-z]/g, "");
}
