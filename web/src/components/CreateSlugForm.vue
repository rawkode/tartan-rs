<script setup lang="ts">
/** A one-field form for naming a new org or repository, checked against the forge's slug rule. */
import { computed, nextTick, onMounted, ref, useId } from "vue";
import { css } from "styled-system/css";
import { button, field } from "styled-system/recipes";
import { slugProblem } from "../lib/slug.ts";
import Icon from "./ui/Icon.vue";
import ProblemAlert from "./ui/ProblemAlert.vue";

const props = defineProps<{
	label: string;
	placeholder: string;
	submit: string;
	create: (slug: string) => Promise<unknown>;
}>();
const emit = defineEmits<{ cancel: [] }>();

const id = useId();
const slug = ref("");
const touched = ref(false);
const busy = ref(false);
const error = ref<unknown>();
const input = ref<HTMLInputElement>();

const problem = computed(() => slugProblem(slug.value));

onMounted(() => nextTick(() => input.value?.focus()));

const onSubmit = async () => {
	touched.value = true;
	if (problem.value || busy.value) return;
	busy.value = true;
	error.value = undefined;
	try {
		await props.create(slug.value);
		slug.value = "";
		touched.value = false;
	} catch (cause) {
		error.value = cause;
	} finally {
		busy.value = false;
	}
};

const styles = {
	form: css({
		display: "grid",
		gap: "3",
		p: "4",
		bg: "bg.surface",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "lg",
		boxShadow: "xs",
	}),
	row: css({ display: "flex", gap: "2", flexWrap: { base: "wrap", sm: "nowrap" }, alignItems: "flex-start" }),
	label: css({ fontWeight: "medium", fontSize: "sm" }),
	hint: css({ fontSize: "xs", color: "fg.muted", mt: "1" }),
	err: css({ fontSize: "xs", color: "danger.fg", mt: "1" }),
	grow: css({ flex: "1", minW: "48" }),
};
</script>

<template>
	<form :class="styles.form" novalidate @submit.prevent="onSubmit" @keydown.esc="emit('cancel')">
		<label :for="id" :class="styles.label">{{ props.label }}</label>
		<div :class="styles.row">
			<div :class="styles.grow">
				<input
					:id="id"
					ref="input"
					v-model.trim="slug"
					:class="field({ mono: true })"
					:placeholder="props.placeholder"
					autocomplete="off"
					spellcheck="false"
					:aria-invalid="touched && !!problem"
					:aria-describedby="`${id}-hint`"
					@blur="touched = slug.length > 0"
				/>
				<p v-if="touched && problem" :id="`${id}-hint`" :class="styles.err">{{ problem }}</p>
				<p v-else :id="`${id}-hint`" :class="styles.hint">
					Lowercase letters, digits and -, up to 64 characters.
				</p>
			</div>
			<button type="submit" :class="button({ variant: 'solid' })" :disabled="busy">
				<Icon name="plus" /> {{ busy ? "Creating…" : props.submit }}
			</button>
			<button type="button" :class="button({ variant: 'ghost' })" @click="emit('cancel')">Cancel</button>
		</div>
		<ProblemAlert v-if="error" :error="error" compact no-retry />
	</form>
</template>
