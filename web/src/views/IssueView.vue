<script setup lang="ts">
/**
 * One issue: its title, Markdown body and typed properties, edited as one draft and saved
 * in a single `update` guarded by the version it was loaded at, and closing or reopening it.
 */
import { computed, reactive, ref, watch } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { css } from "styled-system/css";
import { badge, button, card, field } from "styled-system/recipes";
import MarkdownEditor from "../components/issues/MarkdownEditor.vue";
import MarkdownView from "../components/issues/MarkdownView.vue";
import PropertyEditor from "../components/issues/PropertyEditor.vue";
import StateBadge from "../components/issues/StateBadge.vue";
import Icon from "../components/ui/Icon.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { ApiError } from "../lib/api.ts";
import { client } from "../lib/client.ts";
import { changedProperties, propertyTitle, validateAll } from "../lib/properties.ts";
import { useRepo } from "../lib/repo-context.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import { absoluteTime, relativeTime } from "../lib/time.ts";
import { notify } from "../lib/toast.ts";
import type { Issue, IssueUpdateInput, PropertyValue } from "../lib/types.ts";

const { org, repo } = useRepo();
const route = useRoute();
const number = computed(() => Number(route.params.number));
const api = () => client.issues(org.value, repo.value);

const sources = [org, repo, number, () => settings.sandbox];
const issue = useResource((signal) => api().get(number.value, signal), sources);
const defs = useResource((signal) => api().properties(signal), [org, repo, () => settings.sandbox]);

const longDefs = computed(() => (defs.data.value ?? []).filter((d) => d.kind === "longtext"));
const shortDefs = computed(() => (defs.data.value ?? []).filter((d) => d.kind !== "longtext"));

// The draft: what the person has edited since the issue loaded.
const draft = reactive<{ title: string; body: string; properties: Record<string, PropertyValue> }>({
	title: "",
	body: "",
	properties: {},
});
const editingTitle = ref(false);
const editingBody = ref(false);
const editingLong = reactive<Record<string, boolean>>({});

const reset = (from: Issue | undefined) => {
	draft.title = from?.title ?? "";
	draft.body = from?.body ?? "";
	draft.properties = from ? structuredClone({ ...from.properties }) : {};
	editingTitle.value = false;
	editingBody.value = false;
	for (const key of Object.keys(editingLong)) delete editingLong[key];
};
// Closing or reopening replaces the loaded issue but keeps the draft.
let keepDraft = false;
watch(issue.data, (value) => {
	if (keepDraft) {
		keepDraft = false;
		return;
	}
	reset(value);
}, { immediate: true });

const propertyChanges = computed(() =>
	issue.data.value ? changedProperties(defs.data.value ?? [], issue.data.value.properties, draft.properties) : {}
);
const dirty = computed(() => {
	const current = issue.data.value;
	if (!current) return false;
	return draft.title !== current.title || draft.body !== (current.body ?? "") ||
		Object.keys(propertyChanges.value).length > 0;
});
const errors = computed(() => validateAll(defs.data.value ?? [], draft.properties));
const titleProblem = computed(() =>
	draft.title.trim().length === 0 ? "A title is required." : draft.title.length > 200 ? "At most 200 characters." : null
);
const valid = computed(() => !titleProblem.value && Object.keys(errors.value).length === 0);

const saving = ref(false);
const saveError = ref<unknown>();
const conflict = computed(() => saveError.value instanceof ApiError && saveError.value.status === 409);

const save = async () => {
	const current = issue.data.value;
	if (!current || !dirty.value || !valid.value || saving.value) return;
	saving.value = true;
	saveError.value = undefined;
	const input: IssueUpdateInput = { number: current.number, expectedVersion: current.version };
	if (draft.title !== current.title) input.title = draft.title.trim();
	if (draft.body !== (current.body ?? "")) input.body = draft.body;
	if (Object.keys(propertyChanges.value).length > 0) input.properties = propertyChanges.value;
	try {
		await api().update(input);
		// The update answers without the body, so fetch the issue whole.
		issue.set(await api().get(current.number));
		notify(`Saved #${current.number}`, "success");
	} catch (cause) {
		saveError.value = cause;
	} finally {
		saving.value = false;
	}
};

const toggling = ref(false);
const toggleState = async () => {
	const current = issue.data.value;
	if (!current || toggling.value) return;
	toggling.value = true;
	saveError.value = undefined;
	try {
		const result = current.state === "open" ? await api().close(current.number) : await api().reopen(current.number);
		// Unsaved edits stay: only the state, version and timestamps change.
		keepDraft = true;
		issue.set({ ...current, ...result.issue, body: current.body });
		notify(result.issue.state === "closed" ? `Closed #${current.number}` : `Reopened #${current.number}`, "success");
	} catch (cause) {
		saveError.value = cause;
	} finally {
		toggling.value = false;
	}
};

const reloadLatest = async () => {
	saveError.value = undefined;
	await issue.reload();
};

const onKeydown = (event: KeyboardEvent) => {
	if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
		event.preventDefault();
		void save();
	}
};

const cardStyles = card();
const styles = {
	back: css({ display: "inline-flex", alignItems: "center", gap: "1.5", color: "fg.muted", fontSize: "sm", mb: "3", _hover: { color: "fg.default" } }),
	head: css({ display: "grid", gap: "2", mb: "6", pb: "4", borderBottomWidth: "1px", borderColor: "border.default" }),
	titleRow: css({ display: "flex", alignItems: "flex-start", gap: "3", flexWrap: "wrap" }),
	title: css({ fontSize: "2xl", fontWeight: "semibold", letterSpacing: "-0.02em", lineHeight: "tight", flex: "1", minW: "60" }),
	number: css({ color: "fg.subtle", fontWeight: "normal" }),
	titleInput: css({ fontSize: "lg !important", h: "11 !important", fontWeight: "semibold" }),
	meta: css({ display: "flex", gap: "2", alignItems: "center", flexWrap: "wrap", color: "fg.muted", fontSize: "sm" }),
	grid: css({
		display: "grid",
		gridTemplateColumns: { base: "1fr", lg: "minmax(0, 1fr) 320px" },
		gap: "6",
		alignItems: "start",
	}),
	stack: css({ display: "grid", gap: "4" }),
	props: css({ display: "grid", gap: "4" }),
	error: css({ fontSize: "xs", color: "danger.fg", mt: "1" }),
	savebar: css({
		position: "sticky",
		bottom: "4",
		zIndex: 10,
		display: "flex",
		alignItems: "center",
		gap: "3",
		flexWrap: "wrap",
		p: "3",
		pl: "4",
		mt: "6",
		bg: "bg.surface",
		borderWidth: "1px",
		borderColor: "accent.border",
		borderRadius: "lg",
		boxShadow: "lg",
	}),
	savebarText: css({ flex: 1, fontSize: "sm", color: "fg.muted", minW: "40" }),
	kbd: css({ fontSize: "2xs", px: "1", borderWidth: "1px", borderColor: "border.default", borderRadius: "sm", bg: "bg.subtle" }),
	none: css({ color: "fg.muted", fontSize: "sm" }),
	headerButtons: css({ display: "flex", gap: "2" }),
};
</script>

<template>
	<RouterLink :to="{ name: 'issues', params: { org, repo } }" :class="styles.back">
		<Icon name="arrow-left" :size="14" /> All issues
	</RouterLink>

	<ProblemAlert v-if="issue.error.value" :error="issue.error.value" @retry="issue.reload()" />
	<Skeleton v-else-if="!issue.data.value" :rows="4" />
	<div v-else @keydown="onKeydown">
		<header :class="styles.head">
			<div :class="styles.titleRow">
				<div v-if="editingTitle" :class="styles.title">
					<label for="title-edit" class="sr-only">Title</label>
					<input
						id="title-edit"
						v-model="draft.title"
						:class="[field(), styles.titleInput]"
						maxlength="200"
						autofocus
						:aria-invalid="!!titleProblem"
						@keydown.esc="draft.title = issue.data.value.title; editingTitle = false"
					/>
					<p v-if="titleProblem" :class="styles.error">{{ titleProblem }}</p>
				</div>
				<h2 v-else :class="styles.title">
					{{ draft.title }} <span :class="styles.number">#{{ issue.data.value.number }}</span>
				</h2>
				<div :class="styles.headerButtons">
					<button
						v-if="!editingTitle"
						type="button"
						:class="button({ size: 'sm' })"
						@click="editingTitle = true"
					>Edit title</button>
					<button
						type="button"
						:class="button({ size: 'sm', variant: issue.data.value.state === 'open' ? 'danger' : 'outline' })"
						:disabled="toggling"
						@click="toggleState"
					>
						<Icon :name="issue.data.value.state === 'open' ? 'issue-closed' : 'issue-open'" :size="14" />
						{{ issue.data.value.state === "open" ? "Close issue" : "Reopen issue" }}
					</button>
				</div>
			</div>
			<div :class="styles.meta">
				<StateBadge :state="issue.data.value.state" />
				<span>
					<strong>{{ issue.data.value.author.id }}</strong>
					<template v-if="issue.data.value.author.onBehalfOf"> for {{ issue.data.value.author.onBehalfOf }}</template>
					opened this
					<time :datetime="new Date(issue.data.value.createdAt).toISOString()" :title="absoluteTime(issue.data.value.createdAt)">{{ relativeTime(issue.data.value.createdAt) }}</time>
				</span>
				<span v-if="issue.data.value.project" :class="badge()">{{ issue.data.value.project }}</span>
				<span :class="badge()" title="Issues' version of this issue; saves are checked against it">v{{ issue.data.value.version }}</span>
			</div>
		</header>

		<div :class="styles.grid">
			<div :class="styles.stack">
				<section :class="cardStyles.root" aria-labelledby="body-title">
					<div :class="cardStyles.header">
						<h3 id="body-title" :class="cardStyles.title">Description</h3>
						<button v-if="!editingBody" type="button" :class="button({ size: 'xs', variant: 'ghost' })" @click="editingBody = true">Edit</button>
					</div>
					<div :class="cardStyles.body">
						<MarkdownEditor v-if="editingBody" v-model="draft.body" :max-length="20000" :rows="10" />
						<MarkdownView v-else :source="draft.body" empty="No description." />
					</div>
				</section>
				<section v-for="def in longDefs" :key="def.name" :class="cardStyles.root" :aria-labelledby="`long-${def.name}`">
					<div :class="cardStyles.header">
						<h3 :id="`long-${def.name}`" :class="cardStyles.title">{{ propertyTitle(def) }}</h3>
						<button v-if="!editingLong[def.name]" type="button" :class="button({ size: 'xs', variant: 'ghost' })" @click="editingLong[def.name] = true">Edit</button>
					</div>
					<div :class="cardStyles.body">
						<PropertyEditor v-if="editingLong[def.name]" v-model="draft.properties[def.name]" :def="def" :error="errors[def.name]" />
						<MarkdownView v-else :source="String(draft.properties[def.name] ?? '')" empty="Not set." />
					</div>
				</section>
			</div>

			<aside :class="styles.stack">
				<section :class="cardStyles.root" aria-labelledby="props-title">
					<div :class="cardStyles.header"><h3 id="props-title" :class="cardStyles.title">Properties</h3></div>
					<div :class="[cardStyles.body, styles.props]">
						<ProblemAlert v-if="defs.error.value" :error="defs.error.value" compact @retry="defs.reload()" />
						<Skeleton v-else-if="!defs.data.value" :rows="3" />
						<p v-else-if="shortDefs.length === 0" :class="styles.none">No properties defined.</p>
						<template v-else>
							<PropertyEditor
								v-for="def in shortDefs"
								:key="def.name"
								v-model="draft.properties[def.name]"
								:def="def"
								:error="errors[def.name]"
								:people="['admin', issue.data.value.author.id]"
							/>
						</template>
					</div>
				</section>
			</aside>
		</div>

		<ProblemAlert v-if="saveError && !conflict" :error="saveError" compact no-retry />
		<div v-if="conflict" :class="styles.savebar" role="alert">
			<Icon name="alert" />
			<span :class="styles.savebarText">Someone changed this issue since you opened it. Reload to see their version; your edits will be discarded.</span>
			<button type="button" :class="button({ variant: 'solid', size: 'sm' })" @click="reloadLatest">Reload latest</button>
		</div>
		<div v-else-if="dirty" :class="styles.savebar" role="region" aria-label="Unsaved changes">
			<span :class="styles.savebarText">Unsaved changes <kbd :class="styles.kbd">Ctrl</kbd>+<kbd :class="styles.kbd">Enter</kbd> saves</span>
			<button type="button" :class="button({ variant: 'ghost', size: 'sm' })" @click="reset(issue.data.value)">Discard</button>
			<button type="button" :class="button({ variant: 'solid', size: 'sm' })" :disabled="!valid || saving" @click="save">
				<Icon name="check" :size="14" /> {{ saving ? "Saving…" : "Save changes" }}
			</button>
		</div>
	</div>
</template>
