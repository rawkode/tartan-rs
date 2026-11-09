/**
 * The client every view uses. It answers from the in-browser sandbox instead of the gateway
 * while the sandbox is on, explains refused writes with the caller's rights, and sends the
 * person to sign in on a 401.
 */

import { account, rights, setAccount } from "./account.ts";
import { createClient } from "./api.ts";
import { createSandboxFetch } from "./sandbox.ts";
import { settings } from "./settings.ts";

let sandboxFetch: typeof fetch | undefined;

const sandbox = (): typeof fetch => sandboxFetch ??= createSandboxFetch({ latencyMs: 120 });

let unauthorized: () => void = () => {};

/** Sets what a 401 does: the router sends the person to sign in, keeping where they were. */
export const onUnauthorized = (handler: () => void): void => {
	unauthorized = handler;
};

export const client = createClient({
	fetch: (input, init) => (settings.sandbox ? sandbox()(input, init) : fetch(input, init)),
	rights: () => (account.status === "ready" ? rights.value : null),
	onUnauthorized: () => {
		setAccount({ anonymous: true, dev_identity: account.me?.dev_identity ?? false });
		unauthorized();
	},
});

let loading: Promise<void> | undefined;

/** Reads `GET /-/api/me` once per page load (or again with `force`, after the sandbox toggles). */
export const loadAccount = (force = false): Promise<void> => {
	if (!force && account.status === "ready") return Promise.resolve();
	if (loading && !force && account.status === "loading") return loading;
	account.status = "loading";
	loading = client.me().then(
		(me) => setAccount(me),
		(error: unknown) => {
			account.status = "error";
			account.error = error;
		},
	);
	return loading;
};

/** Ends the browser's session; the store then reads as signed out. */
export const signOut = async (): Promise<void> => {
	if (!settings.sandbox) await client.signOut();
	setAccount({ anonymous: true, dev_identity: account.me?.dev_identity ?? false });
};
