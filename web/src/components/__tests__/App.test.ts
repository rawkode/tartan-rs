import { expect, test, vi } from "vitest";
import { READ_ONLY_MESSAGE } from "../../lib/api.ts";
import { sampleOwner, sampleReader, sampleState } from "../../lib/sandbox.ts";
import { gateway, mountAt } from "../../test-utils.ts";
import App from "../App.vue";

test("a person the forge does not admit sees only the private-forge page, with no other API call", async () => {
	const state = sampleState();
	state.me = { ...sampleReader(), rights: [] };
	const { calls } = gateway(state);
	const { wrapper } = await mountAt("/acme/api/issues", App);
	const page = wrapper.find("[data-private-forge]");
	expect(page.text()).toContain("This forge is private; ask an owner for access.");
	expect(page.text()).toContain("@bob.test");
	expect(page.find("button").text()).toContain("Sign out");
	expect(calls).toEqual(["GET /-/api/me"]);
});

test("signing out from the private-forge page ends the session", async () => {
	const state = sampleState();
	state.me = { ...sampleReader(), rights: [] };
	const { calls } = gateway(state);
	const { wrapper, router } = await mountAt("/", App);
	await wrapper.find("[data-private-forge] button").trigger("click");
	await vi.waitFor(() => expect(router.currentRoute.value.name).toBe("sign-in"));
	expect(calls).toContain("POST /-/auth/logout");
});

test("a reader sees the read-only notice", async () => {
	const state = sampleState();
	state.me = sampleReader();
	gateway(state);
	const { wrapper } = await mountAt("/", App);
	expect(wrapper.find("[data-read-only-notice]").text()).toBe(READ_ONLY_MESSAGE);
	expect(wrapper.find("[data-private-forge]").exists()).toBe(false);
	expect(wrapper.find("[data-account-menu]").text()).toContain("@bob.test");
	expect(wrapper.find("[data-owner-badge]").exists()).toBe(false);
});

test("an owner sees no read-only notice and an owner badge", async () => {
	const state = sampleState();
	state.me = sampleOwner();
	gateway(state);
	const { wrapper } = await mountAt("/", App);
	expect(wrapper.find("[data-read-only-notice]").exists()).toBe(false);
	expect(wrapper.find("[data-account-menu]").text()).toContain("@alice.test");
	expect(wrapper.find("[data-owner-badge]").exists()).toBe(true);
});
