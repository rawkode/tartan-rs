<script setup lang="ts">
/** Sanitised Markdown, styled for reading. */
import { computed } from "vue";
import { css } from "styled-system/css";
import { renderMarkdown } from "../../lib/markdown.ts";

const props = defineProps<{ source: string; empty?: string }>();
const html = computed(() => renderMarkdown(props.source));

const prose = css({
	lineHeight: "relaxed",
	wordBreak: "break-word",
	"& > * + *": { mt: "3" },
	"& h1, & h2, & h3, & h4": { fontWeight: "semibold", lineHeight: "tight", mt: "5" },
	"& h1": { fontSize: "xl" },
	"& h2": { fontSize: "lg", pb: "1", borderBottomWidth: "1px", borderColor: "border.default" },
	"& h3": { fontSize: "md" },
	"& ul": { listStyle: "disc", pl: "5" },
	"& ol": { listStyle: "decimal", pl: "5" },
	"& li + li": { mt: "1" },
	"& a": { color: "accent.fg", textDecoration: "underline", textUnderlineOffset: "2px" },
	"& code": { fontSize: "0.85em", bg: "bg.subtle", px: "1", py: "0.5", borderRadius: "sm" },
	"& pre": { bg: "bg.subtle", p: "3", borderRadius: "md", overflowX: "auto", fontSize: "xs" },
	"& pre code": { bg: "transparent", p: 0 },
	"& blockquote": { borderLeftWidth: "3px", borderColor: "border.strong", pl: "3", color: "fg.muted" },
	"& table": { borderCollapse: "collapse", fontSize: "sm" },
	"& th, & td": { borderWidth: "1px", borderColor: "border.default", px: "2", py: "1" },
	"& hr": { borderColor: "border.default" },
	"& img": { maxW: "full", borderRadius: "md" },
	"& input[type=checkbox]": { mr: "1.5" },
});
const muted = css({ color: "fg.subtle", fontStyle: "italic" });
</script>

<template>
	<!-- eslint-disable-next-line vue/no-v-html -- sanitised by DOMPurify in renderMarkdown -->
	<div v-if="props.source.trim()" :class="prose" v-html="html" />
	<p v-else :class="muted">{{ props.empty ?? "Nothing written yet." }}</p>
</template>
