<script setup lang="ts">
import { css } from "styled-system/css";

const props = defineProps<{ label?: string; size?: number }>();

const ring = css({
	display: "inline-block",
	borderRadius: "full",
	borderWidth: "2px",
	borderColor: "border.strong",
	borderTopColor: "accent.solid",
	animation: "spin",
	animationDuration: "0.8s",
	flexShrink: 0,
});
const row = css({ display: "inline-flex", alignItems: "center", gap: "2", color: "fg.muted", fontSize: "sm" });
</script>

<template>
	<span :class="row" role="status" aria-live="polite">
		<span :class="ring" :style="{ width: `${props.size ?? 16}px`, height: `${props.size ?? 16}px` }" />
		<span v-if="props.label">{{ props.label }}</span>
		<span v-else class="sr-only">Loading</span>
	</span>
</template>
