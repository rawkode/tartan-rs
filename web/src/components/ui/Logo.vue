<script setup lang="ts">
/** Tartan's mark: a small plaid sett in the forge's green, red and gold. */
const props = defineProps<{ size?: number }>();
</script>

<template>
	<svg
		:width="props.size ?? 24"
		:height="props.size ?? 24"
		viewBox="0 0 32 32"
		aria-hidden="true"
		focusable="false"
	>
		<rect width="32" height="32" rx="7" fill="#065f46" />
		<g opacity="0.85">
			<rect x="6" width="5" height="32" fill="#b91c1c" />
			<rect x="21" width="5" height="32" fill="#b91c1c" />
			<rect y="6" width="32" height="5" fill="#b91c1c" opacity="0.8" />
			<rect y="21" width="32" height="5" fill="#b91c1c" opacity="0.8" />
		</g>
		<g fill="#fde68a" opacity="0.9">
			<rect x="15" width="2" height="32" />
			<rect y="15" width="32" height="2" />
		</g>
	</svg>
</template>
