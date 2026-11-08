<script setup lang="ts">
/** A Markdown text area with a rendered preview tab. */
import { ref, useId } from "vue";
import { css } from "styled-system/css";
import MarkdownView from "./MarkdownView.vue";

const props = defineProps<{
	modelValue: string;
	id?: string;
	placeholder?: string;
	maxLength?: number;
	rows?: number;
	invalid?: boolean;
	describedby?: string;
}>();
const emit = defineEmits<{ "update:modelValue": [value: string] }>();

const uid = useId();
const tab = ref<"write" | "preview">("write");

const onKey = (event: KeyboardEvent, current: "write" | "preview") => {
	if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
		tab.value = current === "write" ? "preview" : "write";
		(document.getElementById(`${uid}-${tab.value}`) as HTMLElement | null)?.focus();
	}
};

const styles = {
	root: css({
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "md",
		overflow: "hidden",
		bg: "bg.surface",
		_focusWithin: { borderColor: "accent.solid", boxShadow: "0 0 0 3px token(colors.accent.ring)" },
	}),
	invalid: css({ borderColor: "danger.border" }),
	tabs: css({
		display: "flex",
		alignItems: "center",
		gap: "1",
		px: "1.5",
		pt: "1.5",
		bg: "bg.subtle",
		borderBottomWidth: "1px",
		borderColor: "border.default",
	}),
	tab: css({
		px: "3",
		h: "7",
		fontSize: "xs",
		fontWeight: "medium",
		color: "fg.muted",
		borderTopRadius: "md",
		borderWidth: "1px",
		borderBottomWidth: 0,
		borderColor: "transparent",
		mb: "-1px",
		cursor: "pointer",
		_hover: { color: "fg.default" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "-2px" },
		"&[aria-selected=true]": { bg: "bg.surface", color: "fg.default", borderColor: "border.default" },
	}),
	hint: css({ ml: "auto", pr: "2", fontSize: "2xs", color: "fg.subtle" }),
	area: css({
		display: "block",
		w: "full",
		minH: "32",
		p: "3",
		bg: "transparent",
		color: "fg.default",
		fontSize: "sm",
		lineHeight: "relaxed",
		resize: "vertical",
		border: "none",
		outline: "none",
	}),
	preview: css({ p: "3", minH: "32" }),
	count: css({ px: "3", pb: "1.5", fontSize: "2xs", color: "fg.subtle", textAlign: "right" }),
};
</script>

<template>
	<div :class="[styles.root, props.invalid && styles.invalid]">
		<div :class="styles.tabs" role="tablist" aria-label="Markdown editor">
			<button
				:id="`${uid}-write`"
				type="button"
				role="tab"
				:class="styles.tab"
				:aria-selected="tab === 'write'"
				:aria-controls="`${uid}-panel`"
				:tabindex="tab === 'write' ? 0 : -1"
				@click="tab = 'write'"
				@keydown="onKey($event, 'write')"
			>Write</button>
			<button
				:id="`${uid}-preview`"
				type="button"
				role="tab"
				:class="styles.tab"
				:aria-selected="tab === 'preview'"
				:aria-controls="`${uid}-panel`"
				:tabindex="tab === 'preview' ? 0 : -1"
				@click="tab = 'preview'"
				@keydown="onKey($event, 'preview')"
			>Preview</button>
			<span :class="styles.hint">Markdown</span>
		</div>
		<div :id="`${uid}-panel`" role="tabpanel" :aria-labelledby="`${uid}-${tab}`">
			<textarea
				v-if="tab === 'write'"
				:id="props.id"
				:class="styles.area"
				:value="props.modelValue"
				:placeholder="props.placeholder"
				:maxlength="props.maxLength"
				:rows="props.rows ?? 6"
				:aria-invalid="props.invalid"
				:aria-describedby="props.describedby"
				@input="emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
			/>
			<div v-else :class="styles.preview">
				<MarkdownView :source="props.modelValue" empty="Nothing to preview." />
			</div>
		</div>
		<div v-if="props.maxLength" :class="styles.count">
			{{ props.modelValue.length.toLocaleString() }} / {{ props.maxLength.toLocaleString() }}
		</div>
	</div>
</template>
