/**
 * Leaving the single-page app: the browser navigates to another document, such as `tt`'s
 * loopback address after a CLI approval. Tests replace `assign` to see where it would go.
 */

export const navigation = {
	assign: (url: string): void => globalThis.location.assign(url),
};
