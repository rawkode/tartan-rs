<script setup lang="ts">
/** A repository's issues: filtered by state and project, searched by title, paged. */
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { css } from "styled-system/css";
import { badge, button, field } from "styled-system/recipes";
import PropertyChips from "../components/issues/PropertyChips.vue";
import EmptyState from "../components/ui/EmptyState.vue";
import Icon from "../components/ui/Icon.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { client } from "../lib/client.ts";
import { useRepo } from "../lib/repo-context.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import { absoluteTime, relativeTime } from "../lib/time.ts";
import type { Issue, IssueListOutput, IssueState } from "../lib/types.ts";

type Filter = IssueState | "all";

const { org, repo } = useRepo();
const route = useRoute();
const router = useRouter();

const filter = computed<Filter>(() => {
	const q = route.query.state;
	return q === "closed" || q === "all" ? q : "open";
});
const project = computed(() => (typeof route.query.project === "string" ? route.query.project : ""));
const search = ref(typeof route.query.q === "string" ? route.query.q : "");

const setQuery = (patch: Record<string, string | undefined>) =>
	router.replace({ query: { ...route.query, ...patch } });

const sources = [org, repo, filter, project, () => settings.sandbox];
const api = () => client.issues(org.value, repo.value);
const defs = useResource((signal) => api().properties(signal), [org, repo, () => settings.sandbox]);
const list = useResource<IssueListOutput>(
	(signal) =>
		api().list({ state: filter.value, limit: 50, ...(project.value ? { project: project.value } : {}) }, signal),
	sources,
);

// Further pages, appended below the first.
const more = ref<Issue[]>([]);
const nextBefore = ref<number | null>(null);
const loadingMore = ref(false);
const moreError = ref<unknown>();
watch(list.data, (data) => {
	more.value = [];
	nextBefore.value = data?.nextBefore ?? null;
});
const loadMore = async () => {
	if (nextBefore.value === null) return;
	loadingMore.value = true;
	moreError.value = undefined;
	try {
		const page = await api().list({
			state: filter.value,
			limit: 50,
			before: nextBefore.value,
			...(project.value ? { project: project.value } : {}),
		});
		more.value = [...more.value, ...page.issues];
		nextBefore.value = page.nextBefore;
	} catch (cause) {
		moreError.value = cause;
	} finally {
		loadingMore.value = false;
	}
};

const issues = computed(() => {
	const all = [...(list.data.value?.issues ?? []), ...more.value];
	const needle = search.value.trim().toLowerCase();
	if (!needle) return all;
	return all.filter((i) => i.title.toLowerCase().includes(needle) || `#${i.number}` === needle);
});
const projects = computed(() => {
	const seen = new Set<string>();
	for (const i of list.data.value?.issues ?? []) if (i.project) seen.add(i.project);
	if (project.value) seen.add(project.value);
	return [...seen].sort();
});

const filters: { value: Filter; label: string }[] = [
	{ value: "open", label: "Open" },
	{ value: "closed", label: "Closed" },
	{ value: "all", label: "All" },
];
const count = (f: Filter) => {
	const d = list.data.value;
	if (!d) return undefined;
	return f === "open" ? d.open : f === "closed" ? d.closed : d.open + d.closed;
};

const styles = {
	toolbar: css({ display: "flex", flexWrap: "wrap", gap: "2", alignItems: "center", mb: "4" }),
	segmented: css({
		display: "inline-flex",
		p: "0.5",
		bg: "bg.subtle",
		borderRadius: "md",
		borderWidth: "1px",
		borderColor: "border.default",
	}),
	segment: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "1.5",
		h: "8",
		px: "3",
		fontSize: "sm",
		fontWeight: "medium",
		borderRadius: "sm",
		color: "fg.muted",
		cursor: "pointer",
		_hover: { color: "fg.default" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus" },
		"&[aria-pressed=true]": { bg: "bg.surface", color: "fg.default", boxShadow: "xs" },
	}),
	count: css({ fontSize: "xs", color: "fg.subtle", fontVariantNumeric: "tabular-nums" }),
	search: css({ position: "relative", flex: "1", minW: "48", maxW: "sm" }),
	searchIcon: css({ position: "absolute", left: "2.5", top: "50%", transform: "translateY(-50%)", color: "fg.subtle", pointerEvents: "none" }),
	searchInput: css({ pl: "8 !important" }),
	select: css({ w: "auto !important", minW: "40" }),
	spacer: css({ flex: 1 }),
	list: css({
		bg: "bg.surface",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "lg",
		overflow: "hidden",
		"& > li + li": { borderTopWidth: "1px", borderColor: "border.default" },
	}),
	row: css({
		display: "grid",
		gridTemplateColumns: "auto 1fr auto",
		gap: "3",
		px: "4",
		py: "3",
		alignItems: "start",
		position: "relative",
		_hover: { bg: "bg.subtle" },
		_focusWithin: { bg: "bg.subtle" },
	}),
	stateOpen: css({ color: "accent.solid", mt: "0.5" }),
	stateClosed: css({ color: "closed", mt: "0.5" }),
	main: css({ display: "grid", gap: "1.5", minW: 0 }),
	title: css({
		fontWeight: "semibold",
		color: "fg.default",
		_hover: { color: "accent.fg" },
		_focusVisible: { outline: "none" },
		"&::after": { content: '""', position: "absolute", inset: 0 },
		_focus: { "&::after": { outline: "2px solid", outlineColor: "focus", outlineOffset: "-2px", borderRadius: "lg" } },
	}),
	meta: css({ display: "flex", gap: "2", flexWrap: "wrap", alignItems: "center", fontSize: "xs", color: "fg.muted" }),
	time: css({ fontSize: "xs", color: "fg.subtle", whiteSpace: "nowrap" }),
	foot: css({ display: "flex", justifyContent: "center", mt: "4" }),
	noMatch: css({ p: "6", textAlign: "center", color: "fg.muted" }),
};
</script>

<template>
	<div :class="styles.toolbar">
		<div :class="styles.segmented" role="group" aria-label="Filter by state">
			<button
				v-for="f in filters"
				:key="f.value"
				type="button"
				:class="styles.segment"
				:aria-pressed="filter === f.value"
				@click="setQuery({ state: f.value === 'open' ? undefined : f.value })"
			>
				{{ f.label }}
				<span v-if="count(f.value) !== undefined" :class="styles.count">{{ count(f.value) }}</span>
			</button>
		</div>
		<label :class="styles.search">
			<span class="sr-only">Search titles</span>
			<Icon name="search" :class="styles.searchIcon" :size="14" />
			<input
				v-model="search"
				type="search"
				:class="[field(), styles.searchInput]"
				placeholder="Search titles or #number"
				@change="setQuery({ q: search || undefined })"
			/>
		</label>
		<label v-if="projects.length > 0">
			<span class="sr-only">Project</span>
			<select
				:class="[field(), styles.select]"
				:value="project"
				@change="setQuery({ project: ($event.target as HTMLSelectElement).value || undefined })"
			>
				<option value="">All projects</option>
				<option v-for="p in projects" :key="p" :value="p">{{ p }}</option>
			</select>
		</label>
		<span :class="styles.spacer" />
		<button type="button" :class="button({ variant: 'ghost', size: 'md' })" aria-label="Refresh" @click="list.reload()">
			<Icon name="refresh" />
		</button>
		<RouterLink :to="{ name: 'issue-new', params: { org, repo } }" :class="button({ variant: 'solid' })">
			<Icon name="plus" /> New issue
		</RouterLink>
	</div>

	<ProblemAlert v-if="list.error.value" :error="list.error.value" @retry="list.reload()" />
	<Skeleton v-else-if="list.loading.value && !list.data.value" :rows="5" />
	<template v-else-if="list.data.value">
		<EmptyState
			v-if="list.data.value.issues.length === 0"
			:icon="filter === 'closed' ? 'issue-closed' : 'issue-open'"
			:title="filter === 'open' ? 'No open issues' : filter === 'closed' ? 'No closed issues' : 'No issues yet'"
			:description="filter === 'open' && list.data.value.closed > 0 ? `${list.data.value.closed} closed issue(s) are hidden by the filter.` : 'Issues track work with the typed properties this repository configures.'"
		>
			<RouterLink :to="{ name: 'issue-new', params: { org, repo } }" :class="button({ variant: 'solid' })">
				<Icon name="plus" /> New issue
			</RouterLink>
		</EmptyState>
		<template v-else>
			<ul :class="styles.list" role="list" :aria-busy="list.loading.value">
				<li v-for="issue in issues" :key="issue.number" :class="styles.row">
					<Icon
						:name="issue.state === 'open' ? 'issue-open' : 'issue-closed'"
						:class="issue.state === 'open' ? styles.stateOpen : styles.stateClosed"
						:label="issue.state"
					/>
					<div :class="styles.main">
						<RouterLink :to="{ name: 'issue', params: { org, repo, number: issue.number } }" :class="styles.title">
							{{ issue.title }}
						</RouterLink>
						<div :class="styles.meta">
							<span>#{{ issue.number }}</span>
							<span v-if="issue.project" :class="badge()">{{ issue.project }}</span>
							<span>by {{ issue.author.id }}</span>
						</div>
						<PropertyChips v-if="defs.data.value" :defs="defs.data.value" :values="issue.properties" :limit="6" />
					</div>
					<time :class="styles.time" :datetime="new Date(issue.updatedAt).toISOString()" :title="absoluteTime(issue.updatedAt)">
						{{ relativeTime(issue.updatedAt) }}
					</time>
				</li>
				<li v-if="issues.length === 0" :class="styles.noMatch">No issue on this page matches “{{ search }}”.</li>
			</ul>
			<ProblemAlert v-if="moreError" :error="moreError" compact @retry="loadMore" />
			<div v-if="nextBefore !== null" :class="styles.foot">
				<button type="button" :class="button()" :disabled="loadingMore" @click="loadMore">
					{{ loadingMore ? "Loading…" : "Load more" }}
				</button>
			</div>
		</template>
	</template>
</template>
