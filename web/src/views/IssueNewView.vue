<script setup lang="ts">
/** Opening an issue: title, project, Markdown body and the repository's typed properties. */
import { computed, reactive, ref, watch } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { css } from "styled-system/css";
import { button, card, field } from "styled-system/recipes";
import MarkdownEditor from "../components/issues/MarkdownEditor.vue";
import PropertyEditor from "../components/issues/PropertyEditor.vue";
import Icon from "../components/ui/Icon.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { client } from "../lib/client.ts";
import { initialValues, setValues, validateAll } from "../lib/properties.ts";
import { useRepo } from "../lib/repo-context.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import { notify } from "../lib/toast.ts";
import type { PropertyValue } from "../lib/types.ts";

const { org, repo } = useRepo();
const router = useRouter();
const api = () => client.issues(org.value, repo.value);

const defs = useResource((signal) => api().properties(signal), [org, repo, () => settings.sandbox]);

const title = ref("");
const project = ref("");
const body = ref("");
const values = reactive<Record<string, PropertyValue>>({});
watch(defs.data, (list) => {
	if (!list) return;
	for (const key of Object.keys(values)) delete values[key];
	Object.assign(values, initialValues(list));
}, { immediate: true });

const submitted = ref(false);
const busy = ref(false);
const error = ref<unknown>();

const errors = computed(() => validateAll(defs.data.value ?? [], values));
const titleProblem = computed(() => {
	const t = title.value.trim();
	if (t.length === 0) return "Give the issue a title.";
	if (t.length > 200) return "At most 200 characters.";
	return null;
});
const projectProblem = computed(() =>
	project.value && !/^[^/\s]+(\/[^/\s]+)*$/.test(project.value) ? "A root path, such as crates/lsp." : null
);
const valid = computed(() => !titleProblem.value && !projectProblem.value && Object.keys(errors.value).length === 0);

const submit = async () => {
	submitted.value = true;
	if (!valid.value || busy.value) return;
	busy.value = true;
	error.value = undefined;
	try {
		const props = setValues(values);
		const issue = await api().create({
			title: title.value.trim(),
			...(body.value.trim() ? { body: body.value } : {}),
			...(project.value ? { project: project.value } : {}),
			...(Object.keys(props).length > 0 ? { properties: props } : {}),
		});
		notify(`Opened #${issue.number}`, "success");
		await router.push({ name: "issue", params: { org: org.value, repo: repo.value, number: issue.number } });
	} catch (cause) {
		error.value = cause;
	} finally {
		busy.value = false;
	}
};

const cardStyles = card();
const styles = {
	grid: css({
		display: "grid",
		gridTemplateColumns: { base: "1fr", lg: "minmax(0, 1fr) 320px" },
		gap: "6",
		alignItems: "start",
	}),
	stack: css({ display: "grid", gap: "4" }),
	label: css({ display: "block", fontSize: "xs", fontWeight: "semibold", color: "fg.muted", mb: "1.5" }),
	error: css({ fontSize: "xs", color: "danger.fg", mt: "1" }),
	hint: css({ fontSize: "2xs", color: "fg.subtle", mt: "1" }),
	titleInput: css({ fontSize: "md !important", h: "11 !important", fontWeight: "medium" }),
	props: css({ display: "grid", gap: "4" }),
	actions: css({ display: "flex", gap: "2", justifyContent: "flex-end", flexWrap: "wrap" }),
	heading: css({ fontSize: "xl", fontWeight: "semibold", mb: "4" }),
	none: css({ color: "fg.muted", fontSize: "sm" }),
};
</script>

<template>
	<h2 :class="styles.heading">New issue</h2>
	<form :class="styles.grid" novalidate @submit.prevent="submit">
		<div :class="styles.stack">
			<div>
				<label for="issue-title" :class="styles.label">Title</label>
				<input
					id="issue-title"
					v-model="title"
					:class="[field(), styles.titleInput]"
					placeholder="What needs doing?"
					maxlength="200"
					autofocus
					:aria-invalid="submitted && !!titleProblem"
					aria-describedby="issue-title-error"
				/>
				<p v-if="submitted && titleProblem" id="issue-title-error" :class="styles.error">{{ titleProblem }}</p>
			</div>
			<div>
				<label for="issue-body" :class="styles.label">Description</label>
				<MarkdownEditor id="issue-body" v-model="body" :max-length="20000" :rows="10" placeholder="Context, acceptance criteria, links…" />
			</div>
			<ProblemAlert v-if="error" :error="error" compact no-retry />
			<div :class="styles.actions">
				<RouterLink :to="{ name: 'issues', params: { org, repo } }" :class="button({ variant: 'ghost' })">Cancel</RouterLink>
				<button type="submit" :class="button({ variant: 'solid' })" :disabled="busy">
					<Icon name="check" /> {{ busy ? "Opening…" : "Open issue" }}
				</button>
			</div>
		</div>

		<aside :class="styles.stack">
			<section :class="cardStyles.root" aria-labelledby="new-project">
				<div :class="cardStyles.header"><h3 id="new-project" :class="cardStyles.title">Project</h3></div>
				<div :class="cardStyles.body">
					<label for="issue-project" class="sr-only">Project</label>
					<input
						id="issue-project"
						v-model.trim="project"
						:class="field({ mono: true })"
						placeholder="crates/lsp (optional)"
						spellcheck="false"
						:aria-invalid="!!projectProblem"
						aria-describedby="issue-project-hint"
					/>
					<p v-if="projectProblem" id="issue-project-hint" :class="styles.error">{{ projectProblem }}</p>
					<p v-else id="issue-project-hint" :class="styles.hint">The project's root path in the repository.</p>
				</div>
			</section>
			<section :class="cardStyles.root" aria-labelledby="new-props">
				<div :class="cardStyles.header"><h3 id="new-props" :class="cardStyles.title">Properties</h3></div>
				<div :class="[cardStyles.body, styles.props]">
					<ProblemAlert v-if="defs.error.value" :error="defs.error.value" compact @retry="defs.reload()" />
					<Skeleton v-else-if="!defs.data.value" :rows="3" />
					<p v-else-if="defs.data.value.length === 0" :class="styles.none">
						This repository defines no properties. Add them under <code>settings: properties</code> in its tartan.cue.
					</p>
					<template v-else>
						<PropertyEditor
							v-for="def in defs.data.value"
							:key="def.name"
							v-model="values[def.name]"
							:def="def"
							:error="submitted ? errors[def.name] : null"
							:people="['admin']"
						/>
					</template>
				</div>
			</section>
		</aside>
	</form>
</template>
