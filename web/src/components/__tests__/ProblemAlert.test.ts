import { mount } from "@vue/test-utils";
import { expect, test } from "vitest";
import { createMemoryHistory, createRouter } from "vue-router";
import { ApiError } from "../../lib/api.ts";
import ProblemAlert from "../ui/ProblemAlert.vue";

const router = createRouter({
	history: createMemoryHistory(),
	routes: [{ path: "/-/sign-in", name: "sign-in", component: { template: "<div />" } }],
});

test("shows the gateway's problem, its kind and the request", () => {
	const error = new ApiError(
		409,
		{ error: "issue 3 is at version 4, not 2", kind: "conflict" },
		"issue 3 is at version 4, not 2",
		"POST",
		"/-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/tools/update",
	);
	const w = mount(ProblemAlert, { props: { error }, global: { plugins: [router] } });
	expect(w.attributes("role")).toBe("alert");
	expect(w.text()).toContain("Conflict");
	expect(w.text()).toContain("issue 3 is at version 4, not 2");
	expect(w.text()).toContain("conflict");
	expect(w.text()).toContain("POST /-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/tools/update");
	expect(w.find("pre").text()).toContain('"kind": "conflict"');
});

test("offers to sign in again after a 401", async () => {
	const error = new ApiError(401, { error: "unauthorized" }, "unauthorized", "GET", "/-/api/orgs");
	const w = mount(ProblemAlert, { props: { error }, global: { plugins: [router] } });
	expect(w.text()).toContain("You are signed out");
	expect(w.find("a[href='/-/sign-in']").exists()).toBe(true);
	await w.find("button").trigger("click");
	expect(w.emitted("retry")).toHaveLength(1);
});
