# @thclaws/paperclip-adapter

Paperclip adapter for [thClaws](https://github.com/thClaws/thClaws). Lets
you hire a thClaws agent inside a Paperclip company orchestration —
alongside Claude, Codex, Cursor, Gemini, and the other built-in adapters.

## What you get

- A **`thclaws_local`** adapter type that wraps the thClaws Rust CLI
  in print mode (`thclaws -p`).
- All 21 thClaws providers reachable by model id alone:
  `claude-sonnet-4-6`, `gpt-4o`, `chatgpt-codex/gpt-5.4`,
  `openrouter/anthropic/…`, `gemini-2.5-flash`, `qwen-max`, etc.
- thClaws's built-in MCP / KMS / skills / agent-team primitives
  available inside every run, no extra Paperclip config.
- ChatGPT Plus / Pro / Team subscription billing for Codex models
  via the `chatgpt-codex/*` ids (auto-imports the Codex CLI's
  `~/.codex/auth.json`).

## Status

**v0.1 (MVP).** Spawns `thclaws -p`, captures stdout, returns one
transcript block per run. No multi-turn session continuation, no
incremental tool-call rendering — those land once thClaws ships its
`--output-format stream-json` wire format.

## Prerequisites

- Paperclip with external-adapter plugin support (the `adapter-plugin`
  Phase 1 changes — see `paperclip/adapter-plugin.md`).
- `thclaws` binary on `$PATH` (or set `command` in the adapter config
  to its absolute path). Install via:
  ```sh
  # macOS / Linux (build from source)
  git clone https://github.com/thClaws/thClaws
  cd thClaws/crates/core && cargo install --path .
  ```
- At least one provider API key reachable to thClaws — either in your
  shell env, in `~/.config/thclaws/.env`, or in the project's
  `.thclaws/.env`. The adapter does NOT manage thClaws credentials.

## Install

```sh
# In your Paperclip instance:
pnpm add @thclaws/paperclip-adapter
# Then register via the Paperclip plugin store as documented in
# paperclip/docs/adapters/external-adapters.md.
```

For local development against a Paperclip checkout, link the package
directly:

```sh
cd paperclip-adapter
pnpm build
# Inside Paperclip's adapter plugin store:
pnpm add file:../paperclip-adapter
```

## Configuration

Minimum agent config:

```json
{
  "adapterType": "thclaws_local",
  "model": "claude-sonnet-4-6"
}
```

Full field list — see the `agentConfigurationDoc` in `src/index.ts`
or the description Paperclip's UI renders on the agent-hire page.

## Building

```sh
pnpm install
pnpm build
# Outputs dist/{index.js, server/*.js, ui-parser.js}
```

## License

MIT — same as thClaws.
