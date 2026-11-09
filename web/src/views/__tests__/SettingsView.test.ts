import { expect, test } from "vitest";
import { sampleOwner, sampleReader } from "../../lib/sandbox.ts";
import { as, mountAt, settle } from "../../test-utils.ts";

test("settings have tokens, signing and sessions, and no admin token", async () => {
	as(sampleOwner());
	const { wrapper } = await mountAt("/-/settings");
	const text = wrapper.text();
	expect(text).toContain("Access tokens");
	expect(text).toContain("Signing");
	expect(text).toContain("Sessions");
	expect(text).not.toContain("Admin token");
	expect(wrapper.find("#token").exists()).toBe(false);
});

test("sessions are listed with their kind and device", async () => {
	as(sampleOwner());
	const { wrapper } = await mountAt("/-/settings");
	const sessions = wrapper.find("[data-session-list]").text();
	expect(sessions).toContain("Firefox on Linux");
	expect(sessions).toContain("this browser");
	expect(sessions).toContain("tt (CLI)");
	expect(sessions).toContain("laptop");
});

test("signing instructions name this forge and the person's DID", async () => {
	as(sampleOwner());
	const { wrapper } = await mountAt("/-/settings");
	const signing = wrapper.find("[data-signing]").text();
	expect(signing).toContain(`tt login --forge ${globalThis.location.origin}`);
	expect(signing).toContain("tt sign --setup --global");
	expect(signing).toContain("user.signingKey=did:plc:a7lic3a7lic3a7lic3a7lic3");
	expect(signing).toContain("gpg.x509.program=tt");
	expect(signing).toContain("tt sign --setup --hook");
	expect(signing).toContain("tt sign revoke");
});

test("a created token's secret is shown once and gone after the dialog closes", async () => {
	as(sampleOwner());
	const { wrapper } = await mountAt("/-/settings");
	await wrapper.find("input[maxlength='100']").setValue("ci");
	await wrapper.find("form").trigger("submit");
	await settle();
	const dialog = wrapper.find("[data-token-secret]");
	expect(dialog.text()).toContain("tartan_pat_");
	expect(dialog.text()).toContain("cannot be retrieved later");
	expect(dialog.find("button[aria-label='Copy the token']").exists()).toBe(true);
	expect(wrapper.find("[data-token-list]").text()).toContain("ci");
	await dialog.findAll("button").at(-1)!.trigger("click");
	await settle();
	expect(wrapper.find("[data-token-secret]").exists()).toBe(false);
	expect(wrapper.html()).not.toContain("tartan_pat_");
});

test("an owner may give a token every scope", async () => {
	as(sampleOwner());
	const { wrapper } = await mountAt("/-/settings");
	const scopes = wrapper.findAll("[data-scopes] input").map((i) => (i.element as HTMLInputElement).value);
	expect(scopes).toEqual(["read", "write", "admin"]);
});

test("a non-owner is offered only the read scope", async () => {
	as(sampleReader());
	const { wrapper } = await mountAt("/-/settings");
	const scopes = wrapper.findAll("[data-scopes] input").map((i) => (i.element as HTMLInputElement).value);
	expect(scopes).toEqual(["read"]);
	expect(wrapper.text()).toContain("a token can only read too");
});
