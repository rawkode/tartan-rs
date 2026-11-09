<script setup lang="ts">
/**
 * Who did something (Issues' `author`): a person by handle, an agent as `@handle (agent)` for
 * the person it acts for, and an id recorded before people were DIDs as plain text.
 */
import type { Principal } from "../lib/types.ts";
import PersonDisplay from "./PersonDisplay.vue";

const props = defineProps<{ principal: Principal }>();
</script>

<template>
	<PersonDisplay v-if="props.principal.kind === 'agent' && props.principal.onBehalfOf" :did="props.principal.onBehalfOf" agent />
	<span v-else-if="props.principal.onBehalfOf">
		<PersonDisplay :did="props.principal.id" /> for <PersonDisplay :did="props.principal.onBehalfOf" />
	</span>
	<PersonDisplay v-else :did="props.principal.id" />
</template>
