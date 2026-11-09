import { expect, test } from "vitest";
import { setAccount } from "../../lib/account.ts";
import { SAMPLE_PEOPLE, sampleOwner, sampleReader, sampleState } from "../../lib/sandbox.ts";
import type { RefSignature, StoredSignature } from "../../lib/types.ts";
import { gateway, mountAt, settle } from "../../test-utils.ts";

const MAIN = "4f1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c";

test("each branch head has a badge, and a non-compliant branch a banner", async () => {
	gateway();
	setAccount(sampleOwner());
	const { wrapper } = await mountAt("/acme/api");
	const branches = wrapper.find("[data-branches]").text();
	expect(branches).toContain("main");
	expect(branches).toContain("Signed by @alice.test");
	expect(branches).toContain("release/1.0");
	expect(branches).toContain("Signed by @bob.test");
	const banner = wrapper.find("[data-violation]");
	expect(banner.text()).toContain("release/1.0 does not comply with the signing rule");
	expect(banner.text()).toContain("(@bob.test)");
	expect(wrapper.find("[data-config-waiting]").exists()).toBe(false);
});

test("a violation lists disallowed signers first and counts the rest", async () => {
	const state = sampleState();
	const repo = state.orgs.get("acme")!.repos.get("api")!;
	const release = repo.branches[1]!;
	release.violations = [
		{ oid: "1".repeat(40), problem: "is unsigned", did: null, allowed: true },
		{ oid: "2".repeat(40), problem: "signed by a DID the rule does not list", did: SAMPLE_PEOPLE.bob, allowed: false },
		{ oid: "3".repeat(40), problem: "is unsigned", did: null, allowed: true },
	];
	repo.config = { applied: null, pending: { commit: MAIN, waiting_on: "signature_check" } };
	gateway(state);
	setAccount(sampleOwner());
	const { wrapper } = await mountAt("/acme/api");
	const items = wrapper.findAll("[data-violation] li").map((li) => li.text());
	expect(items).toHaveLength(1);
	expect(items[0]).toContain("222222222222");
	expect(wrapper.find("[data-violation-more]").text()).toBe("and 2 more");
	expect(wrapper.find("[data-config-waiting]").text()).toContain("waits for its signature check");
});

test("a pending head shows its check, naming the signer it waits for", async () => {
	const state = sampleState();
	const repo = state.orgs.get("acme")!.repos.get("api")!;
	repo.branches = [{
		status: "pending",
		attempts: 2,
		next_at: null,
		last_error: null,
		did: SAMPLE_PEOPLE.carol,
		record_missing: true,
		ref: "refs/heads/main",
		oid: MAIN,
		covered: true,
		compliant: null,
		violations: [],
	} as RefSignature];
	gateway(state);
	setAccount(sampleOwner());
	const { wrapper } = await mountAt("/acme/api");
	expect(wrapper.find("[data-branches]").text()).toContain("Waiting for @carol.test to publish the signature record");
});

test("the commit signature view shows the verified signer apart from the claimed author", async () => {
	gateway();
	setAccount(sampleReader());
	const { wrapper } = await mountAt(`/acme/api/commit/${MAIN}/signature`);
	const text = wrapper.text();
	expect(text).toContain("Signed by @alice.test");
	expect(text).toContain("Verified signer");
	expect(text).toContain("Alice <alice@example.com>");
	expect(wrapper.find("[data-record-uri]").text()).toBe(
		`at://${SAMPLE_PEOPLE.alice}/academy.rawkode.tartan.signature/3kabcdefghijk`,
	);
	expect(text).toContain("in @alice.test's repository since");
	expect(text).not.toContain("Check again");
});

test("an owner may check an unverifiable result again", async () => {
	const state = sampleState();
	const repo = state.orgs.get("acme")!.repos.get("api")!;
	const result = repo.commits.get(MAIN) as StoredSignature;
	repo.commits.set(MAIN, { ...result, status: "unverifiable", detail: "the PDS did not answer", annotations: ["a note"] });
	const { calls } = gateway(state);
	setAccount(sampleOwner());
	const { wrapper } = await mountAt(`/acme/api/commit/${MAIN}/signature`);
	expect(wrapper.find("[data-detail]").text()).toBe("the PDS did not answer");
	expect(wrapper.find("[data-annotations]").text()).toBe("a note");
	const again = wrapper.findAll("button").find((b) => b.text().includes("Check again"))!;
	await again.trigger("click");
	await settle();
	expect(calls).toContain(`POST /-/api/orgs/acme/repos/api/commits/${MAIN}/signature/recheck`);
	expect(wrapper.text()).toContain("Checking signatures…");
});

test("a board with lanes by a person labels them by handle", async () => {
	gateway();
	setAccount(sampleOwner());
	const { wrapper } = await mountAt("/acme/api/boards/people");
	const lanes = wrapper.findAll("button[aria-expanded]").map((b) => b.text());
	expect(lanes.some((l) => l.includes("@alice.test"))).toBe(true);
	expect(lanes.some((l) => l.includes("@carol.test"))).toBe(true);
	expect(lanes.some((l) => l.includes(SAMPLE_PEOPLE.alice))).toBe(false);
});
