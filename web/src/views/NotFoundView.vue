<script setup lang="ts">
import { RouterLink } from "vue-router";
import { button } from "styled-system/recipes";
import EmptyState from "../components/ui/EmptyState.vue";
</script>

<template>
	<EmptyState icon="search" title="Page not found" description="Nothing lives at this address.">
		<RouterLink to="/" :class="button({ variant: 'solid' })">Go to orgs</RouterLink>
	</EmptyState>
</template>
