<script setup lang="ts">
/** Placeholder shapes while a list loads, so the layout does not jump when it arrives. */
import { css } from "styled-system/css";

const props = defineProps<{ rows?: number; variant?: "rows" | "tiles" | "board" }>();

const styles = {
	list: css({ display: "grid", gap: "2" }),
	tiles: css({
		display: "grid",
		gridTemplateColumns: { base: "1fr", sm: "repeat(2, 1fr)", lg: "repeat(3, 1fr)" },
		gap: "3",
	}),
	board: css({ display: "grid", gridTemplateColumns: "repeat(4, minmax(220px, 1fr))", gap: "3", overflow: "hidden" }),
	bone: css({
		bg: "bg.subtle",
		borderRadius: "lg",
		animation: "pulse",
		borderWidth: "1px",
		borderColor: "border.default",
	}),
	row: css({ h: "14" }),
	tile: css({ h: "18" }),
	column: css({ h: "80" }),
};
</script>

<template>
	<div
		:class="props.variant === 'tiles' ? styles.tiles : props.variant === 'board' ? styles.board : styles.list"
		aria-busy="true"
		aria-label="Loading"
	>
		<div
			v-for="i in props.rows ?? 4"
			:key="i"
			:class="[styles.bone, props.variant === 'tiles' ? styles.tile : props.variant === 'board' ? styles.column : styles.row]"
		/>
	</div>
</template>
