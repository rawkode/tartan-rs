/**
 * Write controls follow the caller's rights (design D15, *Read-only presentation*). The
 * in-memory gateway answers as an owner here, so each view loads its data and only the
 * store's rights differ: the views must hide what the caller may not do.
 */
import { describe, expect, test } from "vitest";
import { setAccount } from "../../lib/account.ts";
import { sampleOwner, sampleReader } from "../../lib/sandbox.ts";
import type { Me } from "../../lib/types.ts";
import { gateway, mountAt } from "../../test-utils.ts";

const viewer = (who: "owner" | "reader") => {
	gateway();
	const me: Me = who === "owner" ? sampleOwner() : sampleReader();
	setAccount(me);
};

const buttons = (wrapper: { findAll: (s: string) => { text: () => string }[] }) =>
	wrapper.findAll("button, a").map((b) => b.text().trim());

describe.each([["owner", true], ["reader", false]] as const)("as %s", (who, writes) => {
	test("New issue on the issue list", async () => {
		viewer(who);
		const { wrapper } = await mountAt("/acme/api/issues");
		expect(buttons(wrapper).some((t) => t.includes("New issue"))).toBe(writes);
	});

	test("the new-issue form", async () => {
		viewer(who);
		const { wrapper } = await mountAt("/acme/api/issues/new");
		expect(wrapper.find("form").exists()).toBe(writes);
		expect(wrapper.find("[data-read-only]").exists()).toBe(!writes);
	});

	test("edits on an issue", async () => {
		viewer(who);
		const { wrapper } = await mountAt("/acme/api/issues/1");
		const labels = buttons(wrapper);
		expect(labels.includes("Edit title")).toBe(writes);
		expect(labels.some((t) => t.includes("Close issue"))).toBe(writes);
		expect(labels.includes("Edit")).toBe(writes);
		expect(wrapper.find("[data-read-only-properties]").exists()).toBe(!writes);
		// The read-only view still shows the assignee by handle.
		expect(wrapper.text()).toContain("@alice.test");
	});

	test("moving cards on a board", async () => {
		viewer(who);
		const { wrapper } = await mountAt("/acme/api/boards/triage");
		const card = wrapper.find("article");
		expect(card.attributes("draggable")).toBe(writes ? "true" : "false");
		expect(wrapper.find("select[aria-label^='Move #']").exists()).toBe(writes);
	});

	test("the New issue shortcut on the overview", async () => {
		viewer(who);
		const { wrapper } = await mountAt("/acme/api");
		expect(buttons(wrapper).some((t) => t.includes("New issue"))).toBe(writes);
	});
});

describe.each([["owner", true], ["reader", false]] as const)("as %s", (who, admin) => {
	test("creating an org", async () => {
		viewer(who);
		const { wrapper } = await mountAt("/");
		expect(buttons(wrapper).some((t) => t.includes("New org"))).toBe(admin);
	});

	test("creating a repository", async () => {
		viewer(who);
		const { wrapper } = await mountAt("/acme");
		expect(buttons(wrapper).some((t) => t.includes("New repository"))).toBe(admin);
	});
});
