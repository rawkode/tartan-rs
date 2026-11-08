<script setup lang="ts">
/**
 * A repository's Kanban boards. Kanban learns about issue changes from Issues' events, which
 * arrive asynchronously, so the open board refreshes every few seconds. A move shows at once
 * and is rolled back if Kanban refuses it.
 */
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { css } from "styled-system/css";
import { badge, button } from "styled-system/recipes";
import KanbanBoard from "../components/kanban/KanbanBoard.vue";
import EmptyState from "../components/ui/EmptyState.vue";
import Icon from "../components/ui/Icon.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { ApiError, ISSUES_ID } from "../lib/api.ts";
import { client } from "../lib/client.ts";
import { columnOf, findCard, moveCard } from "../lib/kanban.ts";
import { useRepo } from "../lib/repo-context.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import { notify } from "../lib/toast.ts";
import type { BoardView, PropertyDef } from "../lib/types.ts";

const { org, repo, installed } = useRepo();
const route = useRoute();
const router = useRouter();
const api = () => client.kanban(org.value, repo.value);

const boardId = computed(() => (typeof route.params.board === "string" && route.params.board) || "");
const boards = useResource((signal) => api().listBoards(signal), [org, repo, () => settings.sandbox]);

// Open the first board when none is named.
watch([boards.data, boardId], ([list, id]) => {
	const first = list?.boards[0];
	if (!id && first) void router.replace({ name: "boards", params: { org: org.value, repo: repo.value, board: first.id } });
});

/**
 * Moves Kanban has accepted but may not show yet: it learns of the issue's new value from
 * Issues' events, so a board fetched right after a move can still show the card where it was.
 * Until the board shows the card at the moved version (or a while has passed), the move is
 * applied again on top of each fetched board.
 */
const accepted = new Map<number, { column: string; version: number; until: number }>();
const ACCEPTED_FOR_MS = 20_000;

const reconcile = (view: BoardView): BoardView => {
	let next = view;
	for (const [number, move] of accepted) {
		const card = findCard(next, number);
		if (Date.now() > move.until || !card || card.version >= move.version) {
			accepted.delete(number);
			continue;
		}
		next = moveCard(next, number, move.column);
	}
	return next;
};

const board = useResource<BoardView | null>(
	async (signal) => (boardId.value ? reconcile(await api().board(boardId.value, signal)) : null),
	[org, repo, boardId, () => settings.sandbox],
);
watch(boardId, () => accepted.clear());

const hasIssues = computed(() => !!installed(ISSUES_ID));
const defs = useResource<PropertyDef[]>(
	(signal) => (hasIssues.value ? client.issues(org.value, repo.value).properties(signal) : Promise.resolve([])),
	[org, repo, hasIssues, () => settings.sandbox],
);
const issueLink = computed(() =>
	hasIssues.value
		? (number: number) => ({ name: "issue", params: { org: org.value, repo: repo.value, number } })
		: undefined
);

// Moves in flight, by card number; the board does not refresh underneath them.
const pending = reactive(new Set<number>());
const dragging = ref(false);
const lastUpdated = ref<number | null>(null);
const now = ref(Date.now());
watch(board.data, (value) => {
	if (value) lastUpdated.value = Date.now();
});

const move = async (number: number, column: string) => {
	const view = board.data.value;
	if (!view || pending.has(number)) return;
	const from = columnOf(view, number);
	if (from === column) return;
	const card = findCard(view, number);
	const before = view;
	const optimistic = moveCard(view, number, column);
	board.set(optimistic);
	pending.add(number);
	try {
		const moved = await api().move(view.board.id, number, column);
		accepted.set(number, { column, version: moved.version, until: Date.now() + ACCEPTED_FOR_MS });
		const title = view.board.columns.find((c) => c.value === column)?.title ?? column;
		notify(`Moved #${number} to ${title}`, "success", 2500);
	} catch (cause) {
		// Put the card back, unless something newer replaced the board meanwhile; the refresh
		// below corrects that case.
		if (board.data.value === optimistic) board.set(before);
		const reason = cause instanceof ApiError ? cause.message : String(cause);
		notify(`Could not move #${number}${card ? ` “${card.title}”` : ""}: ${reason}`, "danger", 7000);
	} finally {
		pending.delete(number);
		void refresh();
	}
};

const refresh = async () => {
	if (pending.size > 0 || dragging.value || !boardId.value) return;
	await board.reload({ quiet: true });
};

const live = ref(true);
let timer: ReturnType<typeof setInterval> | undefined;
let clock: ReturnType<typeof setInterval> | undefined;
const schedule = () => {
	clearInterval(timer);
	if (!live.value) return;
	timer = setInterval(() => {
		if (document.visibilityState === "visible") void refresh();
	}, Math.max(1, settings.pollSeconds) * 1000);
};
watch([live, () => settings.pollSeconds], schedule);
onMounted(() => {
	schedule();
	clock = setInterval(() => (now.value = Date.now()), 1000);
});
onBeforeUnmount(() => {
	clearInterval(timer);
	clearInterval(clock);
});

const updatedText = computed(() => {
	if (lastUpdated.value === null) return "";
	const s = Math.max(0, Math.round((now.value - lastUpdated.value) / 1000));
	return s < 2 ? "Updated just now" : `Updated ${s}s ago`;
});

const styles = {
	top: css({ display: "flex", flexWrap: "wrap", alignItems: "center", gap: "3", mb: "4" }),
	pills: css({ display: "flex", gap: "1.5", flexWrap: "wrap", flex: 1 }),
	pill: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "1.5",
		h: "8",
		px: "3",
		borderRadius: "full",
		borderWidth: "1px",
		borderColor: "border.default",
		bg: "bg.surface",
		fontSize: "sm",
		fontWeight: "medium",
		color: "fg.muted",
		_hover: { color: "fg.default", borderColor: "border.strong" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px" },
		"&[aria-current=page]": { bg: "accent.solid", borderColor: "accent.solid", color: "accent.contrast" },
	}),
	pillProject: css({ fontFamily: "mono", fontSize: "2xs", opacity: 0.8 }),
	status: css({ display: "flex", alignItems: "center", gap: "2", fontSize: "xs", color: "fg.muted" }),
	dot: css({
		w: "2",
		h: "2",
		borderRadius: "full",
		bg: "fg.subtle",
		"&[data-live=true]": { bg: "success.fg", boxShadow: "0 0 0 3px token(colors.success.subtle)" },
	}),
	invalid: css({
		display: "grid",
		gap: "1",
		p: "3",
		mb: "4",
		bg: "warning.subtle",
		borderWidth: "1px",
		borderColor: "warning.border",
		borderRadius: "lg",
		fontSize: "sm",
	}),
	invalidHead: css({ display: "flex", alignItems: "center", gap: "2", fontWeight: "semibold", color: "warning.fg" }),
	invalidItem: css({ "& code": { fontWeight: "semibold" } }),
	boardHead: css({ display: "flex", alignItems: "baseline", gap: "2", mb: "3", flexWrap: "wrap" }),
	boardName: css({ fontSize: "xl", fontWeight: "semibold", letterSpacing: "-0.01em" }),
	boardMeta: css({ color: "fg.muted", fontSize: "sm" }),
	switchLabel: css({ display: "inline-flex", alignItems: "center", gap: "1.5", cursor: "pointer" }),
};
</script>

<template>
	<ProblemAlert v-if="boards.error.value" :error="boards.error.value" @retry="boards.reload()" />
	<Skeleton v-else-if="!boards.data.value" variant="board" :rows="4" />
	<template v-else>
		<div v-if="boards.data.value.invalid.length > 0" :class="styles.invalid" role="status">
			<div :class="styles.invalidHead"><Icon name="alert" /> {{ boards.data.value.invalid.length }} board(s) could not be shown</div>
			<div v-for="bad in boards.data.value.invalid" :key="bad.id" :class="styles.invalidItem">
				<code>{{ bad.id }}</code>: {{ bad.problem }}
			</div>
		</div>

		<EmptyState
			v-if="boards.data.value.boards.length === 0"
			icon="board"
			title="No boards"
			description="Define boards under settings: boards in this repository's tartan.cue. Each board's columns are one of Issues' select properties, or the issue state."
		>
			<RouterLink :to="{ name: 'repo', params: { org, repo } }" :class="button()">See an example</RouterLink>
		</EmptyState>

		<template v-else>
			<div :class="styles.top">
				<nav :class="styles.pills" aria-label="Boards">
					<RouterLink
						v-for="b in boards.data.value.boards"
						:key="b.id"
						:to="{ name: 'boards', params: { org, repo, board: b.id } }"
						:class="styles.pill"
						:aria-current="b.id === boardId ? 'page' : undefined"
					>
						<Icon name="board" :size="14" /> {{ b.name }}
						<span v-if="b.project" :class="styles.pillProject">{{ b.project }}</span>
					</RouterLink>
				</nav>
				<div :class="styles.status">
					<span :class="styles.dot" :data-live="live" aria-hidden="true" />
					<label :class="styles.switchLabel">
						<input v-model="live" type="checkbox" /> Live
					</label>
					<span aria-live="off">{{ updatedText }}</span>
					<button type="button" :class="button({ size: 'sm', variant: 'ghost' })" aria-label="Refresh the board" @click="board.reload({ quiet: !!board.data.value })">
						<Icon name="refresh" :size="14" />
					</button>
				</div>
			</div>

			<ProblemAlert v-if="board.error.value" :error="board.error.value" @retry="board.reload()" />
			<Skeleton v-else-if="!board.data.value" variant="board" :rows="4" />
			<template v-else>
				<div :class="styles.boardHead">
					<h2 :class="styles.boardName">{{ board.data.value.board.name }}</h2>
					<span v-if="board.data.value.board.project" :class="badge()">{{ board.data.value.board.project }}</span>
					<span :class="styles.boardMeta">
						<template v-if="board.data.value.board.lanes">Lanes by {{ board.data.value.board.lanes.by }}</template>
						<template v-if="board.data.value.board.lanes && board.data.value.board.group"> · </template>
						<template v-if="board.data.value.board.group">grouped by {{ board.data.value.board.group.by }}</template>
					</span>
				</div>
				<KanbanBoard
					:view="board.data.value"
					:defs="defs.data.value ?? []"
					:pending="pending"
					:issue-link="issueLink"
					@move="move"
					@dragging="(active) => (dragging = active)"
				/>
			</template>
		</template>
	</template>
</template>
