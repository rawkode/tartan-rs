<script setup lang="ts">
import { css } from "styled-system/css";
import { dismiss, toasts } from "../../lib/toast.ts";
import Icon from "./Icon.vue";

const styles = {
	region: css({
		position: "fixed",
		bottom: "4",
		right: "4",
		left: { base: "4", sm: "auto" },
		zIndex: 50,
		display: "flex",
		flexDirection: "column",
		gap: "2",
		maxW: { sm: "sm" },
		pointerEvents: "none",
	}),
	toast: css({
		pointerEvents: "auto",
		display: "flex",
		alignItems: "flex-start",
		gap: "2",
		p: "3",
		borderRadius: "lg",
		borderWidth: "1px",
		bg: "bg.surface",
		boxShadow: "lg",
		fontSize: "sm",
	}),
	success: css({ borderColor: "success.border" }),
	danger: css({ borderColor: "danger.border" }),
	neutral: css({ borderColor: "border.default" }),
	icon: css({ mt: "0.5" }),
	iconSuccess: css({ color: "success.fg" }),
	iconDanger: css({ color: "danger.fg" }),
	message: css({ flex: 1 }),
	close: css({
		color: "fg.subtle",
		cursor: "pointer",
		borderRadius: "sm",
		_hover: { color: "fg.default" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus" },
	}),
};
</script>

<template>
	<div :class="styles.region" role="region" aria-label="Notifications" aria-live="polite">
		<div v-for="t in toasts" :key="t.id" :class="[styles.toast, styles[t.tone]]">
			<Icon
				:name="t.tone === 'danger' ? 'alert' : 'check'"
				:class="[styles.icon, t.tone === 'danger' ? styles.iconDanger : styles.iconSuccess]"
			/>
			<span :class="styles.message">{{ t.message }}</span>
			<button type="button" :class="styles.close" aria-label="Dismiss" @click="dismiss(t.id)">
				<Icon name="x" :size="14" />
			</button>
		</div>
	</div>
</template>
