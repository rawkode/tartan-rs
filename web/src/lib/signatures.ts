/**
 * The one table that words a signature result (design D15, *Signature labels*): the badge on
 * a branch, the commit signature view and the docs all use it.
 */

import type { CommitSignature } from "./types.ts";

export type SignatureTone = "success" | "warning" | "danger" | "muted" | "neutral";

export interface SignatureLabel {
	label: string;
	tone: SignatureTone;
	/** A neutral note shown next to the label, such as a deactivated account. */
	note?: string;
	/** The result's own detail, shown under the label. */
	detail?: string;
}

/** A date as the labels write it: `9 Nov 2026`, in UTC so every reader sees the same day. */
export const labelDate = (ms: number): string =>
	new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" })
		.format(new Date(ms));

const REVOKED: Record<string, string> = {
	withdrawn: "withdrawn by the signer",
	deleted: "signature record deleted",
	replaced: "signature record replaced",
};

/**
 * The label of a result. `person` words a DID (`@handle`, or a shortened DID while the handle
 * is unknown).
 */
export const signatureLabel = (
	result: CommitSignature,
	person: (did: string) => string,
): SignatureLabel => {
	const signer = result.did ? person(result.did) : "the signer";
	const detail = "detail" in result && result.detail ? result.detail : undefined;
	switch (result.status) {
		case "pending":
			if (result.record_missing) {
				return {
					label: `Waiting for ${signer} to publish the signature record (\`tt sign --flush\`); ` +
						"checked again hourly for up to seven days",
					tone: "neutral",
				};
			}
			return result.last_error
				? { label: `Retrying: ${result.last_error}`, tone: "neutral" }
				: { label: "Checking signatures…", tone: "neutral" };
		case "verified": {
			if (result.revoked_at !== null) {
				const why = REVOKED[result.revoked_reason ?? "withdrawn"] ?? REVOKED.withdrawn;
				return {
					label: `Signed by ${signer} — ${why} on ${labelDate(result.revoked_at)}; ` +
						"no longer counts for signing rules",
					tone: "warning",
				};
			}
			if (result.did_deactivated_at !== null) {
				const what = result.did_status === "deleted" ? "account deleted" : "account deactivated";
				return {
					label: `Signed by ${signer}`,
					tone: "success",
					note: `${what} on ${labelDate(result.did_deactivated_at)}`,
				};
			}
			return { label: `Signed by ${signer}`, tone: "success" };
		}
		case "unsigned":
			return { label: "Not signed", tone: "muted" };
		case "signer_inactive":
			return { label: "Signer's account is not active", tone: "warning", ...(detail ? { detail } : {}) };
		case "bad_signature":
			return { label: "Invalid signature", tone: "danger", ...(detail ? { detail } : {}) };
		case "unsupported_format":
			return {
				label: "Signed with GPG, SSH or another X.509 tool; only Tartan signatures are checked",
				tone: "muted",
			};
		case "not_checked":
			return { label: "Not checked", tone: "muted", ...(detail ? { detail } : {}) };
		case "unverifiable":
			if (result.record_missing) {
				return {
					label: "Could not verify",
					tone: "warning",
					detail: `Signature record was not published within seven days; ${signer} can publish it ` +
						"and run `tt sign recheck`",
				};
			}
			return { label: "Could not verify", tone: "warning", ...(detail ? { detail } : {}) };
	}
};

/**
 * Whether the viewer is offered "Check again": owners and the signer for `unverifiable`, and
 * owners for a `not_checked` result the check gave up on (not one with too many commits).
 */
export const canRecheck = (result: CommitSignature, viewer: { did: string; owner: boolean } | null): boolean => {
	if (!viewer || result.status === "pending") return false;
	if (result.status === "unverifiable") return viewer.owner || (result.did !== null && result.did === viewer.did);
	if (result.status === "not_checked") return viewer.owner && !/too many commits/i.test(result.detail ?? "");
	return false;
};
