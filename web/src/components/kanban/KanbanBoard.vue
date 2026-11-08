<script setup lang="ts">
/**
 * A board view: lanes as rows, columns across, and headed groups inside each cell. Cards
 * move by drag and drop, or by their column picker; either way the parent performs the move.
 */
import { computed, reactive, ref } from "vue";
import { css } from "styled-system/css";
import { columnCount } from "../../lib/kanban.ts";
import type { BoardView, Card, PropertyDef } from "../../lib/types.ts";
import Icon from "../ui/Icon.vue";
import KanbanCard from "./KanbanCard.vue";

const props = defineProps<{
	view: BoardView;
	defs: PropertyDef[];
	/** Cards with a move in flight. */
	pending: Set<number>;
	/** Builds the link to a card's issue, when Issues is installed. */
	issueLink?: (number: number) => { name: string; params: Record<string, string | number> };
}>();
const emit = defineEmits<{ move: [number: number, column: string]; dragging: [active: boolean] }>();

const DRAG_TYPE = "application/x-tartan-card";

const dragged = ref<{ number: number; from: string | null } | null>(null);
const over = ref<string | null>(null);
const collapsed = reactive<Record<string, boolean>>({});

const board = computed(() => props.view.board);
const hasLanes = computed(() => board.value.lanes !== null);
const exclude = computed(() => [board.value.lanes?.by, board.value.group?.by].filter((b): b is string => !!b));
const showProject = computed(() => board.value.lanes?.by !== "project" && board.value.group?.by !== "project");
const gridColumns = computed(() => `repeat(${board.value.columns.length}, minmax(248px, 1fr))`);

const laneKey = (value: string | null) => value ?? "\u0000none";
const laneCount = (laneIndex: number) => {
	const numbers = new Set<number>();
	for (const cell of props.view.lanes[laneIndex]?.cells ?? []) {
		for (const group of cell.groups) for (const card of group.cards) numbers.add(card.number);
	}
	return numbers.size;
};
const cellCount = (groups: { cards: Card[] }[]) => groups.reduce((n, g) => n + g.cards.length, 0);
const columnTitle = (value: string) => board.value.columns.find((c) => c.value === value)?.title ?? value;

const onDragStart = (event: DragEvent, card: Card, from: string | null) => {
	dragged.value = { number: card.number, from };
	emit("dragging", true);
	if (event.dataTransfer) {
		event.dataTransfer.effectAllowed = "move";
		event.dataTransfer.setData(DRAG_TYPE, String(card.number));
		event.dataTransfer.setData("text/plain", `#${card.number} ${card.title}`);
	}
};
const onDragEnd = () => {
	dragged.value = null;
	over.value = null;
	emit("dragging", false);
};
const dropKey = (lane: string | null, column: string) => `${laneKey(lane)}\u0001${column}`;
const onDragOver = (event: DragEvent, lane: string | null, column: string) => {
	if (!dragged.value) return;
	event.preventDefault();
	if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
	over.value = dropKey(lane, column);
};
const onDragLeave = (event: DragEvent, lane: string | null, column: string) => {
	const target = event.currentTarget as HTMLElement;
	if (event.relatedTarget instanceof Node && target.contains(event.relatedTarget)) return;
	if (over.value === dropKey(lane, column)) over.value = null;
};
const onDrop = (event: DragEvent, column: string) => {
	event.preventDefault();
	const number = Number(event.dataTransfer?.getData(DRAG_TYPE) || dragged.value?.number);
	const from = dragged.value?.from ?? null;
	onDragEnd();
	if (Number.isInteger(number) && number > 0 && from !== column) emit("move", number, column);
};

const styles = {
	scroller: css({
		overflowX: "auto",
		pb: "3",
		mx: { base: "-4", md: "-6" },
		px: { base: "4", md: "6" },
	}),
	grid: css({ display: "grid", gap: "3", minW: "fit-content" }),
	colHead: css({
		position: "sticky",
		top: "14",
		zIndex: 5,
		display: "flex",
		alignItems: "center",
		gap: "2",
		px: "3",
		h: "10",
		bg: "bg.canvas",
		borderBottomWidth: "2px",
		borderColor: "accent.solid",
		fontWeight: "semibold",
		fontSize: "sm",
	}),
	colCount: css({
		ml: "auto",
		minW: "6",
		h: "5",
		px: "1.5",
		display: "grid",
		placeItems: "center",
		borderRadius: "full",
		bg: "bg.subtle",
		color: "fg.muted",
		fontSize: "2xs",
		fontVariantNumeric: "tabular-nums",
	}),
	laneHead: css({
		gridColumn: "1 / -1",
		display: "flex",
		alignItems: "center",
		gap: "2",
		mt: "2",
		px: "1",
		h: "9",
		fontWeight: "semibold",
		fontSize: "sm",
		color: "fg.default",
		cursor: "pointer",
		borderRadius: "md",
		textAlign: "left",
		_hover: { bg: "bg.subtle" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus" },
	}),
	laneChevron: css({ color: "fg.subtle", transition: "transform 0.15s" }),
	laneCollapsed: css({ transform: "rotate(-90deg)" }),
	laneNone: css({ color: "fg.muted", fontStyle: "italic" }),
	laneCount: css({ fontSize: "xs", color: "fg.subtle", fontWeight: "normal" }),
	cell: css({
		display: "flex",
		flexDirection: "column",
		gap: "2",
		p: "2",
		minH: "28",
		bg: "bg.subtle",
		borderRadius: "lg",
		borderWidth: "1px",
		borderColor: "transparent",
		transition: "background 0.1s, border-color 0.1s",
	}),
	cellOver: css({ bg: "accent.subtle", borderColor: "accent.solid", borderStyle: "dashed" }),
	cellEmpty: css({
		flex: 1,
		display: "grid",
		placeItems: "center",
		color: "fg.subtle",
		fontSize: "xs",
		borderRadius: "md",
		borderWidth: "1px",
		borderStyle: "dashed",
		borderColor: "border.default",
		minH: "20",
	}),
	group: css({ display: "grid", gap: "2" }),
	groupHead: css({
		display: "flex",
		alignItems: "center",
		gap: "1.5",
		px: "1",
		pt: "1",
		fontSize: "2xs",
		fontWeight: "semibold",
		textTransform: "uppercase",
		letterSpacing: "0.05em",
		color: "fg.muted",
	}),
	groupNone: css({ fontStyle: "italic", textTransform: "none", letterSpacing: "normal" }),
	tray: css({
		mt: "6",
		p: "4",
		bg: "warning.subtle",
		borderWidth: "1px",
		borderColor: "warning.border",
		borderRadius: "lg",
	}),
	trayHead: css({ display: "flex", alignItems: "center", gap: "2", fontWeight: "semibold", mb: "1" }),
	trayText: css({ fontSize: "sm", color: "fg.muted", mb: "3" }),
	trayCards: css({
		display: "grid",
		gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))",
		gap: "2",
	}),
	srOnly: css({ srOnly: true }),
};
</script>

<template>
	<div :class="styles.scroller">
		<div :class="styles.grid" :style="{ gridTemplateColumns: gridColumns }" role="region" :aria-label="`Board ${board.name}`">
			<div style="display: contents">
				<div v-for="column in board.columns" :key="column.value" :class="styles.colHead">
					<span>{{ column.title }}</span>
					<span :class="styles.colCount" :aria-label="`${columnCount(props.view, column.value)} cards`">
						{{ columnCount(props.view, column.value) }}
					</span>
				</div>
			</div>

			<template v-for="(lane, laneIndex) in props.view.lanes" :key="laneKey(lane.value)">
				<div v-if="hasLanes" style="display: contents">
					<button
						type="button"
						:class="styles.laneHead"
						:aria-expanded="!collapsed[laneKey(lane.value)]"
						@click="collapsed[laneKey(lane.value)] = !collapsed[laneKey(lane.value)]"
					>
						<Icon name="chevron-down" :class="[styles.laneChevron, collapsed[laneKey(lane.value)] && styles.laneCollapsed]" />
						<span :class="lane.value === null && styles.laneNone">{{ lane.title || (lane.value === null ? "None" : lane.value) }}</span>
						<span :class="styles.laneCount">{{ laneCount(laneIndex) }}</span>
					</button>
				</div>
				<div v-if="!collapsed[laneKey(lane.value)]" style="display: contents">
					<div
						v-for="cell in lane.cells"
						:key="cell.column"
						role="group"
						:aria-label="`${columnTitle(cell.column)}${hasLanes ? `, ${lane.title || 'None'}` : ''}`"
						:class="[styles.cell, over === dropKey(lane.value, cell.column) && styles.cellOver]"
						@dragover="onDragOver($event, lane.value, cell.column)"
						@dragenter="onDragOver($event, lane.value, cell.column)"
						@dragleave="onDragLeave($event, lane.value, cell.column)"
						@drop="onDrop($event, cell.column)"
					>
						<div v-if="cellCount(cell.groups) === 0" :class="styles.cellEmpty">{{ dragged ? "Drop here" : "No cards" }}</div>
						<template v-else>
						<section v-for="group in cell.groups" :key="laneKey(group.value)" :class="styles.group">
							<h4 v-if="group.title || group.value !== null || board.group" :class="[styles.groupHead, group.value === null && styles.groupNone]">
								{{ group.title || (group.value === null ? "None" : group.value) }} · {{ group.cards.length }}
							</h4>
							<KanbanCard
								v-for="card in group.cards"
								:key="card.number"
								:card="card"
								:columns="board.columns"
								:column="cell.column"
								:defs="props.defs"
								:exclude="exclude"
								:show-project="showProject"
								:issue-link="props.issueLink?.(card.number)"
								:pending="props.pending.has(card.number)"
								:dragging="dragged?.number === card.number"
								@dragstart="onDragStart($event, card, cell.column)"
								@dragend="onDragEnd"
								@move="(to) => emit('move', card.number, to)"
							/>
						</section>
						</template>
					</div>
				</div>
			</template>
		</div>
	</div>

	<section v-if="props.view.unplaced.length > 0" :class="styles.tray" aria-labelledby="unplaced-title">
		<h3 id="unplaced-title" :class="styles.trayHead">
			<Icon name="inbox" /> Unplaced · {{ props.view.unplaced.length }}
		</h3>
		<p :class="styles.trayText">
			These issues have a value that is not one of this board's columns. Drag one onto a column, or pick a column, to place it.
		</p>
		<div :class="styles.trayCards">
			<KanbanCard
				v-for="card in props.view.unplaced"
				:key="card.number"
				:card="card"
				:columns="board.columns"
				:column="null"
				:defs="props.defs"
				:exclude="[]"
				:show-project="true"
				:issue-link="props.issueLink?.(card.number)"
				:pending="props.pending.has(card.number)"
				:dragging="dragged?.number === card.number"
				@dragstart="onDragStart($event, card, null)"
				@dragend="onDragEnd"
				@move="(to) => emit('move', card.number, to)"
			/>
		</div>
	</section>
</template>
