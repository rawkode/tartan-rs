/**
 * Pure helpers over Kanban's board view: finding cards and moving one between columns
 * locally, so a drag shows at once while the `move` call is in flight. The server's next
 * board replaces whatever these guessed.
 */

import type { BoardView, Card, CardGroup, PropertyValue } from "./types.ts";

/** Where a card sits: a lane, a column and a group within the cell, or the unplaced tray. */
export interface CardPlace {
	lane: string | null;
	column: string;
	group: string | null;
}

/** Every place a card appears on the board (several, under a multi-select lane or group). */
export const placesOf = (view: BoardView, number: number): CardPlace[] => {
	const places: CardPlace[] = [];
	for (const lane of view.lanes) {
		for (const cell of lane.cells) {
			for (const group of cell.groups) {
				if (group.cards.some((card) => card.number === number)) {
					places.push({ lane: lane.value, column: cell.column, group: group.value });
				}
			}
		}
	}
	return places;
};

/** The card with this number, wherever it is. */
export const findCard = (view: BoardView, number: number): Card | undefined => {
	for (const lane of view.lanes) {
		for (const cell of lane.cells) {
			for (const group of cell.groups) {
				const card = group.cards.find((c) => c.number === number);
				if (card) return card;
			}
		}
	}
	return view.unplaced.find((c) => c.number === number);
};

/** The column a card is in, or `null` when it is unplaced or missing. */
export const columnOf = (view: BoardView, number: number): string | null =>
	placesOf(view, number)[0]?.column ?? null;

/** The values a card has for a lane or group dimension (`project` or a property name). */
const valuesBy = (card: Card, by: string): (string | null)[] => {
	const raw: PropertyValue | undefined = by === "project" ? card.project : card.properties[by];
	if (Array.isArray(raw)) return raw.length > 0 ? raw : [null];
	if (raw === null || raw === undefined || raw === "") return [null];
	return [String(raw)];
};

const cloneView = (view: BoardView): BoardView => ({
	board: view.board,
	unplaced: [...view.unplaced],
	lanes: view.lanes.map((lane) => ({
		...lane,
		cells: lane.cells.map((cell) => ({
			...cell,
			groups: cell.groups.map((group) => ({ ...group, cards: [...group.cards] })),
		})),
	})),
});

const groupIn = (view: BoardView, place: CardPlace): CardGroup | undefined => {
	const lane = view.lanes.find((l) => l.value === place.lane) ??
		view.lanes.find((l) => l.value === null);
	const cell = lane?.cells.find((c) => c.column === place.column);
	return cell?.groups.find((g) => g.value === place.group) ??
		cell?.groups.find((g) => g.value === null) ?? cell?.groups[0];
};

/**
 * The board with a card moved to another column, keeping its lanes and groups. An unplaced
 * card is placed by its own lane and group values. Returns the view unchanged when the card
 * or column does not exist.
 */
export const moveCard = (view: BoardView, number: number, column: string): BoardView => {
	if (!view.board.columns.some((c) => c.value === column)) return view;
	const card = findCard(view, number);
	if (!card) return view;
	let places = placesOf(view, number);
	if (places.length === 0) {
		const lanes = view.board.lanes ? valuesBy(card, view.board.lanes.by) : [null];
		const groups = view.board.group ? valuesBy(card, view.board.group.by) : [null];
		places = lanes.flatMap((lane) => groups.map((group) => ({ lane, group, column })));
	}
	const next = cloneView(view);
	next.unplaced = next.unplaced.filter((c) => c.number !== number);
	for (const lane of next.lanes) {
		for (const cell of lane.cells) {
			for (const group of cell.groups) {
				group.cards = group.cards.filter((c) => c.number !== number);
			}
		}
	}
	// A board whose columns are the issue states moves a card by opening or closing it.
	const moved: Card = isStateBoard(view) && (column === "open" || column === "closed")
		? { ...card, state: column }
		: card;
	const seen = new Set<string>();
	for (const place of places) {
		const key = `${place.lane}\u0000${place.group}`;
		if (seen.has(key)) continue;
		seen.add(key);
		groupIn(next, { ...place, column })?.cards.push(moved);
	}
	return next;
};

/** Whether the board's columns are the issue states (`columns: "state"`). */
export const isStateBoard = (view: BoardView): boolean =>
	view.board.columns.length === 2 &&
	view.board.columns.every((c) => c.value === "open" || c.value === "closed");

/** Every card on the board once, placed or not. */
export const allCards = (view: BoardView): Card[] => {
	const byNumber = new Map<number, Card>();
	for (const lane of view.lanes) {
		for (const cell of lane.cells) {
			for (const group of cell.groups) {
				for (const card of group.cards) byNumber.set(card.number, card);
			}
		}
	}
	for (const card of view.unplaced) byNumber.set(card.number, card);
	return [...byNumber.values()];
};

/** How many cards a column holds across lanes, each card once. */
export const columnCount = (view: BoardView, column: string): number => {
	const numbers = new Set<number>();
	for (const lane of view.lanes) {
		for (const cell of lane.cells) {
			if (cell.column !== column) continue;
			for (const group of cell.groups) for (const card of group.cards) numbers.add(card.number);
		}
	}
	return numbers.size;
};
