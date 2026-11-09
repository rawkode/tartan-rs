import { expect, test } from "vitest";
import { setAccount } from "../../lib/account.ts";
import { sampleState } from "../../lib/sandbox.ts";
import { gateway, mountAt, settle } from "../../test-utils.ts";

const anonymous = () => {
	const state = sampleState();
	state.me = { anonymous: true, dev_identity: false };
	return gateway(state);
};

const query = (params: Record<string, string>) =>
	Object.entries(params).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join("&");

test("a crafted problem link shows only the fixed text", async () => {
	const { calls } = anonymous();
	const { wrapper } = await mountAt(
		`/-/sign-in?${query({
			problem: "account_inactive",
			status: "deactivated",
			host: "evil.example",
			error_description: "reactivate at bsky-recovery.evil.example",
			outcome: "forged",
		})}`,
	);
	const text = wrapper.find("[data-problem]").text();
	expect(text).toContain("Your account is deactivated");
	expect(text).toContain("Reactivate it at your PDS, then sign in again.");
	expect(wrapper.html()).not.toContain("evil.example");
	// The outcome read answered 404, so the page keeps the generic words.
	expect(calls).toContain("POST /-/auth/outcome");
	expect(wrapper.find("[data-server-text]").exists()).toBe(false);
});

test("details come from the outcome record, read once", async () => {
	const { fetch } = anonymous();
	const inner = fetch.getMockImplementation()!;
	fetch.mockImplementation(async (input, init) =>
		String(input) === "/-/auth/outcome"
			? new Response(JSON.stringify({ host: "pds.example", error: "invalid_client", error_description: "unknown key" }), {
				status: 200,
			})
			: inner(input, init)
	);
	const { wrapper } = await mountAt(`/-/sign-in?${query({ problem: "client_rejected", outcome: "o1" })}`);
	const outcomeCall = fetch.mock.calls.find(([url]) => String(url) === "/-/auth/outcome")!;
	expect(JSON.parse(String(outcomeCall[1]!.body))).toEqual({ id: "o1", code: "client_rejected" });
	expect(wrapper.find("[data-problem]").text()).toContain("pds.example refused this forge's sign-in configuration");
	expect(wrapper.find("[data-problem]").text()).toContain("tell the forge's operator");
	expect(wrapper.find("[data-server-text]").text()).toBe("invalid_client: unknown key");
});

test.each([
	["rate_limited", "Too many sign-in attempts"],
	["busy", "The forge is busy"],
	["expired", "Sign-in took too long"],
])("%s has its message", async (problem, title) => {
	anonymous();
	const { wrapper } = await mountAt(`/-/sign-in?problem=${problem}`);
	expect(wrapper.find("[data-problem]").text()).toContain(title);
});

test("return_to survives try again and goes into the form", async () => {
	anonymous();
	const returnTo = "/#/acme/api/issues?x=1";
	const { wrapper, router } = await mountAt(`/-/sign-in?${query({ problem: "expired", return_to: returnTo })}`);
	expect(wrapper.find("input[name=return_to]").element.getAttribute("value")).toBe(returnTo);
	await wrapper.find("[data-problem] button").trigger("click");
	await settle();
	expect(router.currentRoute.value.query).toEqual({ return_to: returnTo });
	expect(wrapper.find("[data-problem]").exists()).toBe(false);
	expect(wrapper.find("input[name=return_to]").element.getAttribute("value")).toBe(returnTo);
	const form = wrapper.find("form");
	expect(form.attributes("method")).toBe("post");
	expect(form.attributes("action")).toBe("/-/auth/login");
});

test("an unsafe return_to is replaced by /", async () => {
	anonymous();
	const { wrapper } = await mountAt(`/-/sign-in?${query({ return_to: "//evil.example/" })}`);
	expect(wrapper.find("input[name=return_to]").element.getAttribute("value")).toBe("/");
});

test("the development shortcut shows only on a dev-identity forge", async () => {
	const { state } = anonymous();
	let { wrapper } = await mountAt("/-/sign-in");
	expect(wrapper.find("[data-dev-sign-in]").exists()).toBe(false);
	wrapper.unmount();
	state.me = { anonymous: true, dev_identity: true };
	setAccount(null, "unknown");
	({ wrapper } = await mountAt("/-/sign-in"));
	const dev = wrapper.find("[data-dev-sign-in]");
	expect(dev.text()).toContain("Sign in as @alice.test");
	expect(dev.find("input[name=handle]").element.getAttribute("value")).toBe("alice.test");
});

test("an anonymous visit to a page goes to sign-in with return_to", async () => {
	anonymous();
	const { router } = await mountAt("/acme/api/issues?x=1");
	expect(router.currentRoute.value.name).toBe("sign-in");
	expect(router.currentRoute.value.query.return_to).toBe("/#/acme/api/issues?x=1");
});
