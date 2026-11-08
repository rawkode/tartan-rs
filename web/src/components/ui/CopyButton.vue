<script setup lang="ts">
import { ref } from "vue";
import { button } from "styled-system/recipes";
import Icon from "./Icon.vue";

const props = defineProps<{ text: string; label?: string }>();
const copied = ref(false);
let timer: ReturnType<typeof setTimeout> | undefined;

const copy = async () => {
	try {
		await navigator.clipboard.writeText(props.text);
	} catch {
		// Clipboard access needs a secure context; fall back to a hidden text area.
		const area = document.createElement("textarea");
		area.value = props.text;
		area.setAttribute("readonly", "");
		area.style.position = "fixed";
		area.style.opacity = "0";
		document.body.append(area);
		area.select();
		document.execCommand("copy");
		area.remove();
	}
	copied.value = true;
	clearTimeout(timer);
	timer = setTimeout(() => (copied.value = false), 1600);
};
</script>

<template>
	<button
		type="button"
		:class="button({ size: 'xs', variant: 'ghost' })"
		:aria-label="props.label ?? 'Copy to clipboard'"
		@click="copy"
	>
		<Icon :name="copied ? 'check' : 'copy'" :size="14" />
		<span aria-live="polite">{{ copied ? "Copied" : "Copy" }}</span>
	</button>
</template>
