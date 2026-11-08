<script setup lang="ts">
import { css } from "styled-system/css";
import type { IssueState } from "../../lib/types.ts";
import Icon from "../ui/Icon.vue";

const props = defineProps<{ state: IssueState }>();

const styles = {
	root: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "1",
		h: "6",
		px: "2.5",
		borderRadius: "full",
		fontSize: "xs",
		fontWeight: "semibold",
		color: "white",
	}),
	open: css({ bg: "accent.solid", color: "accent.contrast" }),
	closed: css({ bg: "closed" , color: { base: "white", _dark: "zinc.950" } }),
};
</script>

<template>
	<span :class="[styles.root, styles[props.state]]">
		<Icon :name="props.state === 'open' ? 'issue-open' : 'issue-closed'" :size="13" />
		{{ props.state === "open" ? "Open" : "Closed" }}
	</span>
</template>
