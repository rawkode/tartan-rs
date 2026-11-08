<script setup lang="ts">
import { css } from "styled-system/css";
import Icon, { type IconName } from "./Icon.vue";

const props = defineProps<{ icon: IconName; title: string; description?: string }>();

const styles = {
	root: css({
		display: "flex",
		flexDirection: "column",
		alignItems: "center",
		textAlign: "center",
		gap: "2",
		py: "12",
		px: "6",
		borderWidth: "1px",
		borderStyle: "dashed",
		borderColor: "border.strong",
		borderRadius: "lg",
		bg: "bg.surface",
	}),
	icon: css({
		display: "grid",
		placeItems: "center",
		w: "11",
		h: "11",
		borderRadius: "full",
		bg: "accent.subtle",
		color: "accent.fg",
		mb: "1",
	}),
	title: css({ fontSize: "md", fontWeight: "semibold" }),
	description: css({ color: "fg.muted", maxW: "md", textWrap: "pretty" }),
	actions: css({ display: "flex", gap: "2", mt: "3", flexWrap: "wrap", justifyContent: "center" }),
};
</script>

<template>
	<div :class="styles.root">
		<span :class="styles.icon"><Icon :name="props.icon" :size="20" /></span>
		<div :class="styles.title">{{ props.title }}</div>
		<p v-if="props.description" :class="styles.description">{{ props.description }}</p>
		<div v-if="$slots.default" :class="styles.actions"><slot /></div>
	</div>
</template>
