<script setup lang="ts">
import { css } from "styled-system/css";

const props = defineProps<{ title: string; description?: string }>();

const styles = {
	root: css({
		display: "flex",
		flexWrap: "wrap",
		alignItems: "flex-end",
		justifyContent: "space-between",
		gap: "4",
		mb: "6",
	}),
	title: css({ fontSize: "2xl", fontWeight: "semibold", letterSpacing: "-0.02em", lineHeight: "tight" }),
	description: css({ color: "fg.muted", mt: "1", maxW: "2xl" }),
	actions: css({ display: "flex", gap: "2", alignItems: "center", flexWrap: "wrap" }),
};
</script>

<template>
	<header :class="styles.root">
		<div>
			<slot name="eyebrow" />
			<h1 :class="styles.title">{{ props.title }}</h1>
			<p v-if="props.description" :class="styles.description">{{ props.description }}</p>
		</div>
		<div v-if="$slots.actions" :class="styles.actions"><slot name="actions" /></div>
	</header>
</template>
