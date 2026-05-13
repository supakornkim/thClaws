/**
 * Server-side adapter factory.
 *
 * Paperclip's plugin loader calls `createServerAdapter()` on this
 * module's main export and registers the returned
 * `ServerAdapterModule` in its mutable adapter registry. See
 * paperclip/server/src/adapters/plugin-loader.ts for the exact
 * contract.
 */

import type { ServerAdapterModule } from "@paperclipai/adapter-utils";
import { execute } from "./execute.js";
import { testEnvironment } from "./test.js";
import { models, type, agentConfigurationDoc } from "../index.js";

export function createServerAdapter(): ServerAdapterModule {
  return {
    type,
    execute,
    testEnvironment,
    models,
    agentConfigurationDoc,
  };
}

// Convenience re-exports so callers can import individual pieces
// without going through the factory.
export { execute } from "./execute.js";
export { testEnvironment } from "./test.js";
export { extractTokenSummary, stripTokenSummary } from "./parse.js";
