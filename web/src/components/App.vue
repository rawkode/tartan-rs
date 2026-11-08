<script setup lang="ts">
/** The application shell: the header with breadcrumbs, theme and settings, and the routed view. */
import { computed } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import { css } from "styled-system/css";
import { badge, button } from "styled-system/recipes";
import { settings, type ThemePreference } from "../lib/settings.ts";
import Icon, { type IconName } from "./ui/Icon.vue";
import Logo from "./ui/Logo.vue";
import Toasts from "./ui/Toasts.vue";

const route = useRoute();

const org = computed(() => (typeof route.params.org === "string" ? route.params.org : undefined));
const repo = computed(() => (typeof route.params.repo === "string" ? route.params.repo : undefined));
const wide = computed(() => route.name === "boards");

const themes: { value: ThemePreference; icon: IconName; label: string }[] = [
	{ value: "system", icon: "monitor", label: "System theme" },
	{ value: "light", icon: "sun", label: "Light theme" },
	{ value: "dark", icon: "moon", label: "Dark theme" },
];
const theme = computed(() => themes.find((t) => t.value === settings.theme) ?? themes[0]!);
const nextTheme = () => {
	const index = themes.findIndex((t) => t.value === settings.theme);
	settings.theme = themes[(index + 1) % themes.length]!.value;
};

const styles = {
	skip: css({
		position: "absolute",
		left: "2",
		top: "-12",
		zIndex: 100,
		bg: "bg.surface",
		px: "3",
		py: "2",
		borderRadius: "md",
		boxShadow: "md",
		_focus: { top: "2" },
	}),
	header: css({
		position: "sticky",
		top: 0,
		zIndex: 20,
		bg: "bg.surface/85",
		backdropFilter: "saturate(1.4) blur(10px)",
		borderBottomWidth: "1px",
		borderColor: "border.default",
	}),
	bar: css({
		display: "flex",
		alignItems: "center",
		gap: "3",
		h: "14",
		px: { base: "4", md: "6" },
		mx: "auto",
	}),
	brand: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "2",
		fontWeight: "semibold",
		fontSize: "md",
		letterSpacing: "-0.01em",
		borderRadius: "md",
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px" },
	}),
	crumbs: css({
		display: "flex",
		alignItems: "center",
		gap: "1",
		minW: 0,
		color: "fg.muted",
		"& a": {
			px: "1.5",
			py: "1",
			borderRadius: "md",
			whiteSpace: "nowrap",
			overflow: "hidden",
			textOverflow: "ellipsis",
			_hover: { bg: "bg.subtle", color: "fg.default" },
			_focusVisible: { outline: "2px solid", outlineColor: "focus" },
		},
	}),
	current: css({ color: "fg.default", fontWeight: "medium" }),
	sep: css({ color: "fg.subtle" }),
	spacer: css({ flex: 1 }),
	main: css({ mx: "auto", px: { base: "4", md: "6" }, py: { base: "6", md: "8" } }),
	narrow: css({ maxW: "6xl" }),
	wideMain: css({ maxW: "none" }),
	hideSm: css({ display: { base: "none", sm: "inline" } }),
};
</script>

<template>
	<a href="#main" :class="styles.skip">Skip to content</a>
	<header :class="styles.header">
		<div :class="styles.bar">
			<RouterLink to="/" :class="styles.brand" aria-label="Tartan home">
				<Logo :size="24" />
				<span :class="styles.hideSm">Tartan</span>
			</RouterLink>
			<nav v-if="org" :class="styles.crumbs" aria-label="Breadcrumb">
				<Icon name="chevron-right" :size="14" :class="styles.sep" />
				<RouterLink
					:to="{ name: 'org', params: { org } }"
					:class="!repo && styles.current"
					:aria-current="!repo ? 'page' : undefined"
				>{{ org }}</RouterLink>
				<template v-if="repo">
					<Icon name="chevron-right" :size="14" :class="styles.sep" />
					<RouterLink
						:to="{ name: 'repo', params: { org, repo } }"
						:class="styles.current"
					>{{ repo }}</RouterLink>
				</template>
			</nav>
			<span :class="styles.spacer" />
			<RouterLink
				v-if="settings.sandbox"
				:to="{ name: 'settings' }"
				:class="badge({ tone: 'warning' })"
				title="Requests are answered by sample data in this browser. Turn it off in Settings."
			>
				<Icon name="sandbox" :size="12" /> Sandbox
			</RouterLink>
			<button
				type="button"
				:class="button({ variant: 'ghost', size: 'sm' })"
				:aria-label="`${theme.label}. Switch theme`"
				:title="theme.label"
				@click="nextTheme"
			>
				<Icon :name="theme.icon" />
			</button>
			<RouterLink
				:to="{ name: 'settings' }"
				:class="button({ variant: 'ghost', size: 'sm' })"
				aria-label="Settings"
				title="Settings"
			>
				<Icon name="settings" />
			</RouterLink>
		</div>
	</header>
	<main id="main" tabindex="-1" :class="[styles.main, wide ? styles.wideMain : styles.narrow]">
		<RouterView />
	</main>
	<Toasts />
</template>
