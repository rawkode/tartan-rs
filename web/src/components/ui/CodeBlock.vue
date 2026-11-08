<script setup lang="ts">
import { css } from "styled-system/css";
import CopyButton from "./CopyButton.vue";

const props = defineProps<{ code: string; filename?: string; language?: string }>();

const styles = {
	root: css({
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "lg",
		overflow: "hidden",
		bg: "bg.subtle",
	}),
	bar: css({
		display: "flex",
		alignItems: "center",
		justifyContent: "space-between",
		gap: "2",
		pl: "3",
		pr: "1",
		h: "9",
		borderBottomWidth: "1px",
		borderColor: "border.default",
		bg: "bg.surface",
	}),
	name: css({ fontFamily: "mono", fontSize: "xs", color: "fg.muted" }),
	pre: css({
		m: 0,
		p: "3",
		overflowX: "auto",
		fontFamily: "mono",
		fontSize: "xs",
		lineHeight: "relaxed",
		tabSize: 4,
		whiteSpace: "pre",
	}),
};
</script>

<template>
	<figure :class="styles.root">
		<figcaption :class="styles.bar">
			<span :class="styles.name">{{ props.filename ?? props.language ?? "" }}</span>
			<CopyButton :text="props.code" :label="`Copy ${props.filename ?? 'code'}`" />
		</figcaption>
		<pre :class="styles.pre" tabindex="0"><code>{{ props.code }}</code></pre>
	</figure>
</template>
