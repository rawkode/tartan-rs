<script setup lang="ts">
/** One property's value, read only: its title and its value, a person by handle. */
import { css } from "styled-system/css";
import { formatValue, propertyTitle } from "../../lib/properties.ts";
import type { PropertyDef, PropertyValue } from "../../lib/types.ts";
import PersonDisplay from "../PersonDisplay.vue";

const props = defineProps<{ def: PropertyDef; value: PropertyValue | undefined }>();

const styles = {
	row: css({ display: "grid", gap: "0.5", mb: "3" }),
	title: css({ fontSize: "xs", fontWeight: "semibold", color: "fg.muted" }),
};
</script>

<template>
	<div :class="styles.row">
		<dt :class="styles.title">{{ propertyTitle(props.def) }}</dt>
		<dd>
			<PersonDisplay v-if="props.def.kind === 'person' && typeof props.value === 'string'" :did="props.value" />
			<template v-else>{{ formatValue(props.def, props.value) }}</template>
		</dd>
	</div>
</template>
