/**
 * The client every view uses. It reads the token from the settings on each request, and
 * answers from the in-browser sandbox instead of the gateway while the sandbox is on.
 */

import { createClient } from "./api.ts";
import { createSandboxFetch } from "./sandbox.ts";
import { settings } from "./settings.ts";

const SANDBOX_TOKEN = "sandbox";

let sandboxFetch: typeof fetch | undefined;

const sandbox = (): typeof fetch =>
	sandboxFetch ??= createSandboxFetch({ token: SANDBOX_TOKEN, latencyMs: 120 });

export const client = createClient({
	token: () => (settings.sandbox ? SANDBOX_TOKEN : settings.token),
	fetch: (input, init) => (settings.sandbox ? sandbox()(input, init) : fetch(input, init)),
});
