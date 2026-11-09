import { afterEach, expect, test, vi } from "vitest";
import { navigation } from "../../lib/navigate.ts";
import { sampleOwner, sampleState } from "../../lib/sandbox.ts";
import { as, gateway, mountAt, settle } from "../../test-utils.ts";

const STATE = "s".repeat(43);
const CHALLENGE = "c".repeat(43);
const path = `/-/cli-login?port=8765&state=${STATE}&challenge=${CHALLENGE}&device=laptop`;

const original = navigation.assign;
afterEach(() => {
	navigation.assign = original;
});

test("approve goes to the loopback address the forge answered", async () => {
	const { fetch } = as(sampleOwner());
	const assign = vi.fn();
	navigation.assign = assign;
	const { wrapper } = await mountAt(path);
	expect(wrapper.find("[data-question]").text()).toContain("Allow tt on laptop to act as @alice.test");
	await wrapper.findAll("button")[0]!.trigger("click");
	await settle();
	const approval = fetch.mock.calls.find(([url]) => String(url) === "/-/auth/cli/approve")!;
	expect(JSON.parse(String(approval[1]!.body))).toEqual({
		port: 8765,
		state: STATE,
		challenge: CHALLENGE,
		device: "laptop",
	});
	expect(assign).toHaveBeenCalledWith(`http://127.0.0.1:8765/forge?code=sandbox&state=${STATE}`);
});

test("cancel sends access_denied to tt without asking the forge", async () => {
	const { calls } = as(sampleOwner());
	const assign = vi.fn();
	navigation.assign = assign;
	const { wrapper } = await mountAt(path);
	await wrapper.findAll("button")[1]!.trigger("click");
	expect(assign).toHaveBeenCalledWith(`http://127.0.0.1:8765/forge?error=access_denied&state=${STATE}`);
	expect(calls).not.toContain("POST /-/auth/cli/approve");
});

test("a signed-out visit goes to sign-in and comes back to the approval", async () => {
	const state = sampleState();
	state.me = { anonymous: true, dev_identity: false };
	gateway(state);
	const { router } = await mountAt(path);
	expect(router.currentRoute.value.name).toBe("sign-in");
	expect(router.currentRoute.value.query.return_to).toBe(`/#${path}`);
});

test("an incomplete link offers nothing to approve", async () => {
	as(sampleOwner());
	const { wrapper } = await mountAt("/-/cli-login?port=80&state=x&challenge=y");
	expect(wrapper.find("[data-question]").exists()).toBe(false);
	expect(wrapper.find("[role=alert]").text()).toContain("Run tt login again");
});
