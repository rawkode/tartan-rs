/**
 * Who is using the page: `GET /-/api/me`, read once per page load and kept in one store
 * (design D15). Views read rights from here to hide what the caller may not do; the API
 * client reads them to explain a refused write.
 */

import { computed, reactive } from "vue";
import type { Me, Right } from "./types.ts";

export interface AccountState {
	/** `unknown` until the first load starts. */
	status: "unknown" | "loading" | "ready" | "error";
	me: Me | null;
	error: unknown;
}

export const account = reactive<AccountState>({ status: "unknown", me: null, error: undefined });

/** The signed-in caller, or null for an anonymous one or before `/-/api/me` answered. */
export const signedIn = computed(() => {
	const me = account.me;
	return me && !me.anonymous ? me : null;
});

/** The caller's rights; empty while unknown or anonymous. */
export const rights = computed<readonly Right[]>(() => signedIn.value?.rights ?? []);

export const hasRight = (right: Right): boolean => rights.value.includes(right);

/** A signed-in person the forge's `readers` does not admit: the page shows only who they are. */
export const isPrivate = computed(() => signedIn.value !== null && signedIn.value.rights.length === 0);

/** Can read but not write: the page hides write controls and says why. */
export const isReadOnly = computed(() => hasRight("read") && !hasRight("write"));

/** Whether the forge runs its development identity network. */
export const devIdentity = computed(() => account.me?.dev_identity ?? false);

/** Replaces what the store knows, as a load, a sign-out or a test does. */
export const setAccount = (me: Me | null, status: AccountState["status"] = "ready"): void => {
	account.me = me;
	account.status = status;
	account.error = undefined;
};
