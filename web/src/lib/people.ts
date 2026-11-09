/**
 * Handles for the DIDs on a page (design D14). Every person shown asks for its DID; the asks
 * of one tick are batched into one `GET /-/api/people` (at most 100 DIDs each), and answers
 * are cached for the page's life. The forge answers from its cache and never waits on the
 * network, so a DID it has not resolved yet shows as a shortened DID.
 */

import { reactive } from "vue";
import { PEOPLE_BATCH } from "./api.ts";
import { client } from "./client.ts";
import { isDid, shortDid } from "./did.ts";
import type { PersonInfo } from "./types.ts";

/** What is known per DID: an answer (null for a DID the forge never resolved), or nothing yet. */
export const people = reactive(new Map<string, PersonInfo | null>());

const queued = new Set<string>();
const asked = new Set<string>();
let scheduled = false;

const flush = async (): Promise<void> => {
	scheduled = false;
	const dids = [...queued];
	queued.clear();
	for (let i = 0; i < dids.length; i += PEOPLE_BATCH) {
		const batch = dids.slice(i, i + PEOPLE_BATCH);
		try {
			const answer = await client.people(batch);
			for (const did of batch) people.set(did, answer[did] ?? null);
		} catch {
			// Shown as shortened DIDs; a later page may ask again.
			for (const did of batch) asked.delete(did);
		}
	}
};

/** Asks for the handles of `dids` (each DID once per page load). */
export const requestPeople = (dids: Iterable<string>): void => {
	for (const did of dids) {
		if (!isDid(did) || asked.has(did)) continue;
		asked.add(did);
		queued.add(did);
	}
	if (queued.size > 0 && !scheduled) {
		scheduled = true;
		queueMicrotask(() => void flush());
	}
};

/** The verified handle of `did`, or null while unknown or invalid. */
export const handleOf = (did: string): string | null => {
	const handle = people.get(did)?.handle ?? null;
	return handle && handle !== "handle.invalid" ? handle : null;
};

/** How a person reads in text: `@handle`, else the shortened DID. */
export const personLabel = (did: string): string => {
	const handle = handleOf(did);
	return handle ? `@${handle}` : shortDid(did);
};

/** Forgets every answer, as a test or a sign-out does. */
export const resetPeople = (): void => {
	people.clear();
	queued.clear();
	asked.clear();
	scheduled = false;
};
