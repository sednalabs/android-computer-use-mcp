import test from "node:test";
import assert from "node:assert/strict";

import {
  contextFromObservation,
  createCodexThreadItemsAdapter,
  createMessageItem,
  createThreadInjectItemsParams,
} from "./index.js";

test("package entrypoint re-exports Codex bridge helpers", async () => {
  assert.equal(typeof createCodexThreadItemsAdapter, "function");
  assert.equal(typeof createMessageItem, "function");
  assert.equal(typeof createThreadInjectItemsParams, "function");
  assert.equal(typeof contextFromObservation, "function");
});
