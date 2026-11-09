import { mount } from "@vue/test-utils";
import { expect, test } from "vitest";
import { defineComponent, h } from "vue";
import { setAccount } from "../../lib/account.ts";
import { SAMPLE_PEOPLE, sampleOwner, sampleState } from "../../lib/sandbox.ts";
import { gateway, settle } from "../../test-utils.ts";
import AuthorDisplay from "../AuthorDisplay.vue";
import PersonDisplay from "../PersonDisplay.vue";
import PersonInput from "../PersonInput.vue";

const UNKNOWN = "did:plc:zzzzzzzzzzzzzzzzzzzzzzzz";

test("people on a page are looked up in one request", async () => {
	const { calls, fetch } = gateway();
	setAccount(sampleOwner());
	const wrapper = mount(
		defineComponent({
			render: () =>
				h("div", [SAMPLE_PEOPLE.alice, SAMPLE_PEOPLE.bob, SAMPLE_PEOPLE.alice, UNKNOWN].map((did) => h(PersonDisplay, { did }))),
		}),
	);
	await settle();
	expect(calls.filter((c) => c.startsWith("GET /-/api/people"))).toHaveLength(1);
	const url = String(fetch.mock.calls.find(([u]) => String(u).startsWith("/-/api/people"))![0]);
	expect(new URL(url, "http://x").searchParams.getAll("did")).toEqual([SAMPLE_PEOPLE.alice, SAMPLE_PEOPLE.bob, UNKNOWN]);
	const shown = wrapper.findAll("[data-person]").map((p) => p.text());
	expect(shown).toEqual(["@alice.test", "@bob.test", "@alice.test", "did:plc:zzzz…zzzz"]);
	expect(wrapper.find("[data-person]").attributes("title")).toBe(SAMPLE_PEOPLE.alice);
});

test("a development forge links to no profile; another links to Bluesky", async () => {
	gateway();
	setAccount(sampleOwner());
	const dev = mount(PersonDisplay, { props: { did: SAMPLE_PEOPLE.alice } });
	await settle();
	expect(dev.find("a").exists()).toBe(false);
	setAccount({ ...sampleOwner(), dev_identity: false });
	const prod = mount(PersonDisplay, { props: { node: { t: "person", did: SAMPLE_PEOPLE.alice } } });
	await settle();
	expect(prod.find("a").attributes("href")).toBe(`https://bsky.app/profile/${encodeURIComponent(SAMPLE_PEOPLE.alice)}`);
	expect(prod.text()).toBe("@alice.test");
});

test("a value recorded before people were DIDs is plain text", () => {
	gateway();
	const wrapper = mount(PersonDisplay, { props: { did: "admin" } });
	expect(wrapper.text()).toBe("admin");
	expect(wrapper.find("[data-person]").exists()).toBe(false);
});

test("an agent shows as the person it acts for", async () => {
	gateway();
	setAccount(sampleOwner());
	const wrapper = mount(AuthorDisplay, {
		props: { principal: { kind: "agent", id: "token:01J", onBehalfOf: SAMPLE_PEOPLE.alice } },
	});
	await settle();
	expect(wrapper.text()).toBe("@alice.test (agent)");
});

test("a handle typed into a person input becomes its DID", async () => {
	gateway(sampleState());
	setAccount(sampleOwner());
	const wrapper = mount(PersonInput, { props: { modelValue: null, suggestions: [SAMPLE_PEOPLE.alice] } });
	await wrapper.find("input").setValue("@Bob.test");
	await wrapper.find("input").trigger("keydown", { key: "Enter" });
	await settle();
	expect(wrapper.emitted("update:modelValue")).toEqual([[SAMPLE_PEOPLE.bob]]);
	await wrapper.find("input").setValue("nobody.test");
	await wrapper.find("input").trigger("keydown", { key: "Enter" });
	await settle();
	expect(wrapper.find("[role=alert]").text()).toContain("No account has the handle nobody.test");
	await wrapper.findAll("button").at(-1)!.trigger("click");
	expect(wrapper.emitted("update:modelValue")!.at(-1)).toEqual([SAMPLE_PEOPLE.alice]);
});
