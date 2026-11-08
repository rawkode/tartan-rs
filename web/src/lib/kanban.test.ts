import { describe, expect, test } from "vitest";
import { allCards, columnCount, columnOf, moveCard, placesOf } from "./kanban.ts";
import type { BoardView, Card } from "./types.ts";

const card = (number: number, properties: Card["properties"] = {}, project: string | null = null): Card => ({
	number,
	title: `Card ${number}`,
	state: "open",
	project,
	properties,
	version: 1,
});

const view = (): BoardView => ({
	board: {
		id: "triage",
		name: "Triage",
		project: null,
		columns: [
			{ value: "todo", title: "To do" },
			{ value: "done", title: "Done" },
		],
		lanes: { by: "tags" },
		group: null,
	},
	lanes: [
		{
			value: "bug",
			title: "bug",
			cells: [
				{ column: "todo", groups: [{ value: null, title: "", cards: [card(1, { tags: ["bug", "docs"] })] }] },
				{ column: "done", groups: [{ value: null, title: "", cards: [] }] },
			],
		},
		{
			value: "docs",
			title: "docs",
			cells: [
				{ column: "todo", groups: [{ value: null, title: "", cards: [card(1, { tags: ["bug", "docs"] })] }] },
				{ column: "done", groups: [{ value: null, title: "", cards: [card(2, { tags: ["docs"] })] }] },
			],
		},
		{
			value: null,
			title: "None",
			cells: [
				{ column: "todo", groups: [{ value: null, title: "", cards: [] }] },
				{ column: "done", groups: [{ value: null, title: "", cards: [] }] },
			],
		},
	],
	unplaced: [card(3, { tags: [] })],
});

describe("moveCard", () => {
	test("moves a multi-lane card in every lane it is in", () => {
		const next = moveCard(view(), 1, "done");
		expect(placesOf(next, 1)).toEqual([
			{ lane: "bug", column: "done", group: null },
			{ lane: "docs", column: "done", group: null },
		]);
		expect(columnCount(next, "todo")).toBe(0);
		expect(columnCount(next, "done")).toBe(2);
	});

	test("leaves the original view untouched", () => {
		const before = view();
		moveCard(before, 1, "done");
		expect(columnOf(before, 1)).toBe("todo");
	});

	test("places an unplaced card by its own lane value", () => {
		const next = moveCard(view(), 3, "todo");
		expect(next.unplaced).toEqual([]);
		expect(placesOf(next, 3)).toEqual([{ lane: null, column: "todo", group: null }]);
	});

	test("ignores unknown columns and cards", () => {
		const before = view();
		expect(moveCard(before, 1, "nope")).toBe(before);
		expect(moveCard(before, 99, "done")).toBe(before);
	});

	test("counts each card once", () => {
		expect(allCards(view()).map((c) => c.number).sort()).toEqual([1, 2, 3]);
		expect(columnCount(view(), "todo")).toBe(1);
	});
});
