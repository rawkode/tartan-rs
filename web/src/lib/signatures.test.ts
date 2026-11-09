import { describe, expect, test } from "vitest";
import { canRecheck, labelDate, signatureLabel } from "./signatures.ts";
import type { PendingSignature, StoredSignature } from "./types.ts";

const ALICE = "did:plc:a7lic3a7lic3a7lic3a7lic3";
const person = (did: string) => (did === ALICE ? "@alice.test" : did);
const DAY = Date.UTC(2026, 10, 9);

const stored = (overrides: Partial<StoredSignature>): StoredSignature => ({
	status: "verified",
	oid: "a".repeat(40),
	detail: null,
	did: ALICE,
	rkey: null,
	record_cid: null,
	record_uri: null,
	record_rev: null,
	verified_at: null,
	revoked_at: null,
	revoked_reason: null,
	did_deactivated_at: null,
	did_status: null,
	record_missing: false,
	author: null,
	committer: null,
	proof_url: null,
	annotations: [],
	...overrides,
});

const pending = (overrides: Partial<PendingSignature>): PendingSignature => ({
	status: "pending",
	attempts: 0,
	next_at: null,
	last_error: null,
	did: ALICE,
	record_missing: false,
	...overrides,
});

describe("the label table", () => {
	test("dates are UTC days", () => {
		expect(labelDate(DAY)).toBe("9 Nov 2026");
	});

	test.each([
		[stored({}), "Signed by @alice.test", "success", undefined],
		[
			stored({ revoked_at: DAY, revoked_reason: "withdrawn" }),
			"Signed by @alice.test — withdrawn by the signer on 9 Nov 2026; no longer counts for signing rules",
			"warning",
			undefined,
		],
		[
			stored({ revoked_at: DAY, revoked_reason: "deleted" }),
			"Signed by @alice.test — signature record deleted on 9 Nov 2026; no longer counts for signing rules",
			"warning",
			undefined,
		],
		[
			stored({ revoked_at: DAY, revoked_reason: "replaced" }),
			"Signed by @alice.test — signature record replaced on 9 Nov 2026; no longer counts for signing rules",
			"warning",
			undefined,
		],
		[stored({ did_deactivated_at: DAY, did_status: "deactivated" }), "Signed by @alice.test", "success", "account deactivated on 9 Nov 2026"],
		[stored({ did_deactivated_at: DAY, did_status: "deleted" }), "Signed by @alice.test", "success", "account deleted on 9 Nov 2026"],
		[stored({ status: "unsigned", did: null }), "Not signed", "muted", undefined],
		[stored({ status: "signer_inactive" }), "Signer's account is not active", "warning", undefined],
		[stored({ status: "bad_signature", detail: "record does not sign this commit" }), "Invalid signature", "danger", undefined],
		[
			stored({ status: "unsupported_format", did: null }),
			"Signed with GPG, SSH or another X.509 tool; only Tartan signatures are checked",
			"muted",
			undefined,
		],
		[stored({ status: "not_checked", detail: "too many commits in one push" }), "Not checked", "muted", undefined],
		[stored({ status: "unverifiable", detail: "the PDS did not answer" }), "Could not verify", "warning", undefined],
		[pending({}), "Checking signatures…", "neutral", undefined],
		[pending({ last_error: "PDS unreachable" }), "Retrying: PDS unreachable", "neutral", undefined],
		[
			pending({ record_missing: true }),
			"Waiting for @alice.test to publish the signature record (`tt sign --flush`); checked again hourly for up to seven days",
			"neutral",
			undefined,
		],
	])("%#: %s", (result, label, tone, note) => {
		const worded = signatureLabel(result, person);
		expect(worded.label).toBe(label);
		expect(worded.tone).toBe(tone);
		expect(worded.note).toBe(note);
	});

	test("details are kept", () => {
		expect(signatureLabel(stored({ status: "bad_signature", detail: "record was replaced" }), person).detail).toBe(
			"record was replaced",
		);
		expect(
			signatureLabel(stored({ status: "unverifiable", record_missing: true }), person).detail,
		).toBe(
			"Signature record was not published within seven days; @alice.test can publish it and run `tt sign recheck`",
		);
	});

	test("check again is for owners and the signer", () => {
		const unverifiable = stored({ status: "unverifiable" });
		expect(canRecheck(unverifiable, { did: "did:web:owner.example", owner: true })).toBe(true);
		expect(canRecheck(unverifiable, { did: ALICE, owner: false })).toBe(true);
		expect(canRecheck(unverifiable, { did: "did:web:other.example", owner: false })).toBe(false);
		expect(canRecheck(stored({}), { did: ALICE, owner: true })).toBe(false);
		expect(canRecheck(stored({ status: "not_checked", detail: "not reached before the check gave up" }), { did: ALICE, owner: true }))
			.toBe(true);
		expect(canRecheck(stored({ status: "not_checked", detail: "too many commits in one push" }), { did: ALICE, owner: true }))
			.toBe(false);
	});
});
