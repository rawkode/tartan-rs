<script setup lang="ts">
/** A repository: its header and tabs, and the identity and extensions its pages share. */
import { computed, provide } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import { css } from "styled-system/css";
import { badge } from "styled-system/recipes";
import CopyButton from "../components/ui/CopyButton.vue";
import Icon, { type IconName } from "../components/ui/Icon.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import { ISSUES_ID, KANBAN_ID } from "../lib/api.ts";
import { client } from "../lib/client.ts";
import { repoContextKey } from "../lib/repo-context.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";

const props = defineProps<{ org: string; repo: string }>();
const route = useRoute();

const org = computed(() => props.org);
const repo = computed(() => props.repo);
const sources = [() => props.org, () => props.repo, () => settings.sandbox];
const info = useResource((signal) => client.getRepo(props.org, props.repo, signal), sources);
const extensions = useResource(
	(signal) => client.listExtensions(props.org, props.repo, signal),
	sources,
);
const installed = (id: string) => extensions.data.value?.find((e) => e.extension_id === id);

provide(repoContextKey, { org, repo, info, extensions, installed });

interface Tab {
	name: string;
	label: string;
	icon: IconName;
	matches: string[];
	extension?: string;
}
const tabs = computed<Tab[]>(() => {
	const all: Tab[] = [
		{ name: "repo", label: "Overview", icon: "repo", matches: ["repo"] },
		{
			name: "issues",
			label: "Issues",
			icon: "issue-open",
			matches: ["issues", "issue", "issue-new"],
			extension: ISSUES_ID,
		},
		{ name: "boards", label: "Boards", icon: "board", matches: ["boards"], extension: KANBAN_ID },
	];
	return all.filter((t) => !t.extension || installed(t.extension));
});
const active = (tab: Tab) => tab.matches.includes(String(route.name));

const styles = {
	head: css({ mb: "6", display: "grid", gap: "3" }),
	titleRow: css({ display: "flex", alignItems: "center", gap: "3", flexWrap: "wrap" }),
	title: css({ fontSize: "2xl", fontWeight: "semibold", letterSpacing: "-0.02em", display: "flex", gap: "1", alignItems: "baseline" }),
	orgPart: css({ color: "fg.muted", fontWeight: "normal", _hover: { color: "accent.fg" } }),
	clone: css({ display: "flex", alignItems: "center", gap: "1", minW: 0, maxW: "full" }),
	url: css({
		fontFamily: "mono",
		fontSize: "xs",
		color: "fg.muted",
		bg: "bg.subtle",
		borderWidth: "1px",
		borderColor: "border.default",
		px: "2",
		py: "1",
		borderRadius: "md",
		overflow: "hidden",
		textOverflow: "ellipsis",
		whiteSpace: "nowrap",
	}),
	tabs: css({
		display: "flex",
		gap: "1",
		boxShadow: "inset 0 -1px 0 token(colors.border.default)",
		overflowX: "auto",
		overflowY: "hidden",
		scrollbarWidth: "none",
	}),
	tab: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "2",
		px: "3",
		h: "10",
		color: "fg.muted",
		fontWeight: "medium",
		borderBottomWidth: "2px",
		borderColor: "transparent",
		whiteSpace: "nowrap",
		_hover: { color: "fg.default", borderColor: "border.strong" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "-2px", borderRadius: "sm" },
		"&[aria-current=page]": { color: "fg.default", borderColor: "accent.solid" },
	}),
};
</script>

<template>
	<ProblemAlert v-if="info.error.value" :error="info.error.value" @retry="info.reload()" />
	<template v-else>
		<div :class="styles.head">
			<div :class="styles.titleRow">
				<h1 :class="styles.title">
					<RouterLink :to="{ name: 'org', params: { org: props.org } }" :class="styles.orgPart">{{ props.org }}</RouterLink>
					<span :class="styles.orgPart" aria-hidden="true">/</span>
					<span>{{ props.repo }}</span>
				</h1>
				<span v-if="info.data.value?.hosted_in" :class="badge()">pool {{ info.data.value.hosted_in }}</span>
				<div v-if="info.data.value" :class="styles.clone">
					<code :class="styles.url" :title="info.data.value.clone_url">{{ info.data.value.clone_url }}</code>
					<CopyButton :text="info.data.value.clone_url" label="Copy the clone URL" />
				</div>
			</div>
			<nav :class="styles.tabs" aria-label="Repository">
				<RouterLink
					v-for="tab in tabs"
					:key="tab.name"
					:to="{ name: tab.name, params: { org: props.org, repo: props.repo } }"
					:class="styles.tab"
					:aria-current="active(tab) ? 'page' : undefined"
				>
					<Icon :name="tab.icon" :size="15" /> {{ tab.label }}
				</RouterLink>
			</nav>
		</div>
		<RouterView />
	</template>
</template>
