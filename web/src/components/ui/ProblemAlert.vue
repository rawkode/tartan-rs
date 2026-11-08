<script setup lang="ts">
/**
 * Shows why a request failed: the gateway's own message and error kind, the request it
 * answered, and the raw problem body for anyone debugging the forge.
 */
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { css } from "styled-system/css";
import { badge, button } from "styled-system/recipes";
import { ApiError } from "../../lib/api.ts";
import { settings } from "../../lib/settings.ts";
import Icon from "./Icon.vue";

const props = defineProps<{ error: unknown; title?: string; compact?: boolean; noRetry?: boolean }>();
const emit = defineEmits<{ retry: [] }>();

const apiError = computed(() => (props.error instanceof ApiError ? props.error : null));

const heading = computed(() => {
	if (props.title) return props.title;
	const e = apiError.value;
	if (!e) return "Something went wrong";
	if (e.status === 0) return "The gateway is unreachable";
	if (e.status === 401) return "The gateway refused the token";
	if (e.status === 404) return "Not found";
	if (e.status === 409) return "Conflict";
	if (e.kind === "invalid" || e.status === 400) return "The request was refused";
	if (e.status >= 500) return "The forge could not answer";
	return `Request failed (HTTP ${e.status})`;
});

const message = computed(() => {
	if (apiError.value) return apiError.value.message;
	if (props.error instanceof Error) return props.error.message;
	return String(props.error);
});

const raw = computed(() =>
	apiError.value?.problem ? JSON.stringify(apiError.value.problem, null, 2) : null
);

const styles = {
	root: css({
		display: "flex",
		gap: "3",
		borderWidth: "1px",
		borderColor: "danger.border",
		bg: "danger.subtle",
		color: "fg.default",
		borderRadius: "lg",
		p: "4",
	}),
	compact: css({ p: "3" }),
	icon: css({ color: "danger.fg", mt: "0.5" }),
	body: css({ display: "flex", flexDirection: "column", gap: "1.5", minW: 0, flex: 1 }),
	heading: css({ fontWeight: "semibold", color: "danger.fg" }),
	message: css({ wordBreak: "break-word" }),
	meta: css({ display: "flex", flexWrap: "wrap", alignItems: "center", gap: "2", color: "fg.muted", fontSize: "xs" }),
	code: css({ fontFamily: "mono", fontSize: "xs" }),
	details: css({ fontSize: "xs", color: "fg.muted", "& summary": { cursor: "pointer", w: "fit-content" } }),
	pre: css({
		mt: "2",
		p: "2",
		bg: "bg.surface",
		borderRadius: "md",
		borderWidth: "1px",
		borderColor: "border.default",
		overflowX: "auto",
		fontFamily: "mono",
	}),
	actions: css({ display: "flex", gap: "2", mt: "1", flexWrap: "wrap" }),
};
</script>

<template>
	<div :class="[styles.root, props.compact && styles.compact]" role="alert">
		<Icon name="alert" :size="18" :class="styles.icon" />
		<div :class="styles.body">
			<div :class="styles.heading">{{ heading }}</div>
			<div :class="styles.message">{{ message }}</div>
			<div v-if="apiError" :class="styles.meta">
				<span v-if="apiError.kind" :class="badge({ tone: 'danger' })">{{ apiError.kind }}</span>
				<span :class="styles.code">{{ apiError.method }} {{ apiError.path }}</span>
				<span v-if="apiError.status">· HTTP {{ apiError.status }}</span>
			</div>
			<details v-if="raw && !props.compact" :class="styles.details">
				<summary>Problem details</summary>
				<pre :class="styles.pre">{{ raw }}</pre>
			</details>
			<div :class="styles.actions">
				<button
					v-if="!props.noRetry"
					type="button"
					:class="button({ size: 'sm' })"
					@click="emit('retry')"
				>
					<Icon name="refresh" :size="14" /> Try again
				</button>
				<RouterLink
					v-if="apiError && (apiError.status === 401 || apiError.status === 0) && !settings.sandbox"
					:to="{ name: 'settings' }"
					:class="button({ size: 'sm', variant: 'ghost' })"
				>
					<Icon name="settings" :size="14" /> Open settings
				</RouterLink>
			</div>
		</div>
	</div>
</template>
