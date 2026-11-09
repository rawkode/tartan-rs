import { mount } from "@vue/test-utils";
import { describe, expect, test } from "vitest";
import type { PropertyDef, PropertyValue } from "../../lib/types.ts";
import PropertyEditor from "../issues/PropertyEditor.vue";

const editor = (def: PropertyDef, modelValue: PropertyValue | undefined = undefined, error?: string) =>
	mount(PropertyEditor, { props: { def, modelValue, error } });

const emitted = (wrapper: ReturnType<typeof editor>) =>
	(wrapper.emitted("update:modelValue") ?? []).map((args) => args[0]);

describe("PropertyEditor", () => {
	test("select offers the options by label and clears to null", async () => {
		const w = editor({ name: "status", kind: "select", label: "Status", options: ["todo", "done"], labels: { todo: "To do" } }, "todo");
		const options = w.findAll("option").map((o) => o.text());
		expect(options).toEqual(["—", "To do", "done"]);
		expect(w.find("label").text()).toContain("Status");
		await w.find("select").setValue("done");
		await w.find("select").setValue("");
		expect(emitted(w)).toEqual(["done", null]);
	});

	test("multiselect toggles options and keeps them in option order", async () => {
		const w = editor({ name: "tags", kind: "multiselect", options: ["a", "b", "c"] }, ["c"]);
		const buttons = w.findAll("button");
		expect(buttons.map((b) => b.attributes("aria-pressed"))).toEqual(["false", "false", "true"]);
		await buttons[0]!.trigger("click");
		expect(emitted(w)).toEqual([["a", "c"]]);
		await w.setProps({ modelValue: ["c"] });
		await w.findAll("button")[2]!.trigger("click");
		expect(emitted(w).at(-1)).toBeNull();
	});

	test("number emits numbers", async () => {
		const w = editor({ name: "n", kind: "number", min: 0, max: 10 });
		await w.find("input").setValue("7");
		expect(emitted(w)).toEqual([7]);
		expect(w.text()).toContain("0 to 10");
	});

	test("money edits major units and emits minor units", async () => {
		const w = editor({ name: "budget", kind: "money", currency: "GBP" }, 1250);
		const input = w.find("input");
		expect((input.element as HTMLInputElement).value).toBe("12.50");
		expect(w.text()).toContain("GBP");
		await input.setValue("99.99");
		await input.setValue("oops");
		expect(emitted(w)).toEqual([9999, "oops"]);
	});

	test("boolean is a switch", async () => {
		const w = editor({ name: "blocked", kind: "boolean" }, false);
		const sw = w.find("[role=switch]");
		expect(sw.attributes("aria-checked")).toBe("false");
		await sw.trigger("click");
		expect(emitted(w)).toEqual([true]);
	});

	test("date, url and person use fitting inputs", () => {
		expect(editor({ name: "d", kind: "date" }).find("input").attributes("type")).toBe("date");
		expect(editor({ name: "u", kind: "url" }).find("input").attributes("type")).toBe("url");
		expect(editor({ name: "p", kind: "person" }).find("input").attributes("placeholder")).toBe("handle or DID");
	});

	test("long text is a Markdown editor with a preview", async () => {
		const w = editor({ name: "notes", kind: "longtext" }, "**bold**");
		expect(w.find("textarea").exists()).toBe(true);
		await w.findAll("[role=tab]")[1]!.trigger("click");
		expect(w.find("strong").text()).toBe("bold");
	});

	test("shows the error and marks the control invalid", () => {
		const w = editor({ name: "u", kind: "url" }, "nope", "Must be an http(s) URL.");
		expect(w.text()).toContain("Must be an http(s) URL.");
		expect(w.find("input").attributes("aria-invalid")).toBe("true");
	});
});
