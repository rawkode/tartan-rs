/**
 * DIDs as `tartan.rawkode.academy/types` (`#DID`) spells them: `did:plc:` and 24 characters
 * of `a-z2-7`, or `did:web:` and a lowercase hostname with no path. Only `localhost` may carry
 * a port (`did:web:localhost%3A9400`), which ATProto allows for local testing.
 */

const DID =
	/^did:(plc:[a-z2-7]{24}|web:(localhost(%3A[1-9][0-9]{0,4})?|([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?))$/;

/** Whether `text` is a DID Tartan accepts as a person. */
export const isDid = (text: string): boolean => text.length <= 261 && DID.test(text);

/**
 * A DID short enough to show where a handle would go: a `did:plc` keeps its first and last
 * four characters; a `did:web` is already readable.
 */
export const shortDid = (did: string): string => {
	const plc = did.match(/^did:plc:([a-z2-7]{24})$/);
	return plc ? `did:plc:${plc[1]!.slice(0, 4)}…${plc[1]!.slice(-4)}` : did;
};
