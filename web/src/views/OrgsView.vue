<script setup lang="ts">
/** The forge's orgs, and creating one. */
import { ref } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { css } from "styled-system/css";
import { button, card } from "styled-system/recipes";
import CreateSlugForm from "../components/CreateSlugForm.vue";
import EmptyState from "../components/ui/EmptyState.vue";
import Icon from "../components/ui/Icon.vue";
import PageHeader from "../components/ui/PageHeader.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { client } from "../lib/client.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import { notify } from "../lib/toast.ts";

const router = useRouter();
const orgs = useResource((signal) => client.listOrgs(signal), [() => settings.sandbox]);
const creating = ref(false);

const create = async (slug: string) => {
	const org = await client.createOrg(slug);
	notify(`Created org ${org.slug}`, "success");
	await router.push({ name: "org", params: { org: org.slug } });
};

const cardStyles = card();
const styles = {
	grid: css({
		display: "grid",
		gridTemplateColumns: { base: "1fr", sm: "repeat(2, 1fr)", lg: "repeat(3, 1fr)" },
		gap: "3",
	}),
	tile: css({
		display: "flex",
		alignItems: "center",
		gap: "3",
		p: "4",
		bg: "bg.surface",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "lg",
		transition: "all 0.15s",
		_hover: { borderColor: "accent.border", boxShadow: "sm", transform: "translateY(-1px)" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px" },
	}),
	avatar: css({
		display: "grid",
		placeItems: "center",
		w: "10",
		h: "10",
		borderRadius: "md",
		bg: "accent.subtle",
		color: "accent.fg",
		fontWeight: "semibold",
		fontSize: "md",
		textTransform: "uppercase",
		flexShrink: 0,
	}),
	name: css({ fontWeight: "semibold", fontSize: "md" }),
	sub: css({ color: "fg.muted", fontSize: "xs" }),
	chevron: css({ ml: "auto", color: "fg.subtle" }),
	create: css({ mb: "6" }),
	intro: css({
		display: "grid",
		gridTemplateColumns: { base: "1fr", md: "2fr 1fr" },
		gap: "4",
		mb: "8",
	}),
	steps: css({
		display: "grid",
		gap: "3",
		counterReset: "step",
		"& li": { display: "flex", gap: "3", color: "fg.muted" },
		"& b": { color: "fg.default", fontWeight: "medium" },
	}),
	num: css({
		display: "grid",
		placeItems: "center",
		w: "6",
		h: "6",
		flexShrink: 0,
		borderRadius: "full",
		bg: "bg.subtle",
		borderWidth: "1px",
		borderColor: "border.default",
		fontSize: "xs",
		fontWeight: "semibold",
		color: "fg.default",
	}),
};
</script>

<template>
	<PageHeader
		title="Organisations"
		description="Every repository on this forge belongs to an org. Repositories configure their own extensions in committed CUE."
	>
		<template #actions>
			<button
				type="button"
				:class="button({ variant: 'solid' })"
				:aria-expanded="creating"
				@click="creating = !creating"
			>
				<Icon name="plus" /> New org
			</button>
		</template>
	</PageHeader>

	<div v-if="creating" :class="styles.create">
		<CreateSlugForm
			label="Org name"
			placeholder="acme"
			submit="Create org"
			:create="create"
			@cancel="creating = false"
		/>
	</div>

	<ProblemAlert v-if="orgs.error.value" :error="orgs.error.value" @retry="orgs.reload()" />
	<Skeleton v-else-if="orgs.loading.value && !orgs.data.value" :rows="3" variant="tiles" />
	<template v-else-if="orgs.data.value">
		<div v-if="orgs.data.value.length === 0" :class="styles.intro">
			<EmptyState
				icon="org"
				title="No orgs yet"
				description="Create an org, add a repository to it, then push a tartan.cue that installs Issues and Kanban."
			>
				<button type="button" :class="button({ variant: 'solid' })" @click="creating = true">
					<Icon name="plus" /> Create your first org
				</button>
			</EmptyState>
			<section :class="cardStyles.root" aria-labelledby="how-title">
				<div :class="cardStyles.header"><h2 id="how-title" :class="cardStyles.title">How it fits together</h2></div>
				<ol :class="[cardStyles.body, styles.steps]">
					<li><span :class="styles.num">1</span><span><b>Create an org</b> and a repository in it.</span></li>
					<li><span :class="styles.num">2</span><span><b>Push a <code>tartan.cue</code></b> that imports the extension modules this forge serves.</span></li>
					<li><span :class="styles.num">3</span><span><b>Use the extensions</b>: file issues with typed properties and drag them across boards.</span></li>
				</ol>
			</section>
		</div>
		<ul v-else :class="styles.grid" role="list">
			<li v-for="org in orgs.data.value" :key="org.slug">
				<RouterLink :to="{ name: 'org', params: { org: org.slug } }" :class="styles.tile">
					<span :class="styles.avatar" aria-hidden="true">{{ org.slug.slice(0, 2) }}</span>
					<span>
						<span :class="styles.name">{{ org.slug }}</span>
						<br />
						<span :class="styles.sub">Organisation</span>
					</span>
					<Icon name="chevron-right" :class="styles.chevron" />
				</RouterLink>
			</li>
		</ul>
	</template>
</template>
