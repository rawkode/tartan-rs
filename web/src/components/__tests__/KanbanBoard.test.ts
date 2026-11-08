import { mount } from "@vue/test-utils";
import { expect, test } from "vitest";
import { createMemoryHistory, createRouter } from "vue-router";
import { createClient } from "../../lib/api.ts";
import { createSandboxFetch } from "../../lib/sandbox.ts";
import KanbanBoard from "../kanban/KanbanBoard.vue";

const router = createRouter({
	history: createMemoryHistory(),
	routes: [{ path: "/:pathMatch(.*)*", name: "issue", component: { template: "<div />" } }],
});

const triage = async () => {
	const client = createClient({ token: () => "dev-admin-token", fetch: createSandboxFetch() });
	const api = client.kanban("acme", "api");
	return { view: await api.board("triage"), defs: await client.issues("acme", "api").properties() };
};

test("renders columns, lanes with a final None lane, groups and the unplaced tray", async () => {
	const { view, defs } = await triage();
	const w = mount(KanbanBoard, { props: { view, defs, pending: new Set<number>() }, global: { plugins: [router] } });
	const text = w.text();
	for (const title of ["To do", "In progress", "In review", "Done"]) expect(text).toContain(title);
	const lanes = w.findAll("button[aria-expanded]").map((b) => b.text());
	expect(lanes[0]).toContain("P1 · urgent");
	expect(lanes.at(-1)).toContain("None");
	expect(text).toContain("CRATES/GATEWAY".toLowerCase());
	expect(w.find("[aria-labelledby=unplaced-title]").text()).toContain("Archive the old org boards");
});

test("the column picker emits a move", async () => {
	const { view, defs } = await triage();
	const w = mount(KanbanBoard, { props: { view, defs, pending: new Set<number>() }, global: { plugins: [router] } });
	const picker = w.find("select[aria-label^='Move #1 ']");
	await picker.setValue("done");
	expect(w.emitted("move")).toEqual([[1, "done"]]);
});

test("collapsing a lane hides its cells", async () => {
	const { view, defs } = await triage();
	const w = mount(KanbanBoard, { props: { view, defs, pending: new Set<number>() }, global: { plugins: [router] } });
	const before = w.findAll("article").length;
	await w.find("button[aria-expanded]").trigger("click");
	expect(w.find("button[aria-expanded]").attributes("aria-expanded")).toBe("false");
	expect(w.findAll("article").length).toBeLessThan(before);
});
