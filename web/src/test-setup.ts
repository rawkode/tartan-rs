/**
 * Every test starts with no caller known, no people cached and the in-memory gateway as
 * `fetch`, so no test reaches the network; a test that needs another gateway stubs `fetch`
 * itself.
 */

import { enableAutoUnmount } from "@vue/test-utils";
import { afterEach, beforeEach, vi } from "vitest";
import { setAccount } from "./lib/account.ts";
import { resetPeople } from "./lib/people.ts";
import { createSandboxFetch } from "./lib/sandbox.ts";

enableAutoUnmount(afterEach);

beforeEach(() => {
	vi.unstubAllGlobals();
	vi.stubGlobal("fetch", createSandboxFetch());
	setAccount(null, "unknown");
	resetPeople();
});
