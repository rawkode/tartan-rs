<script setup lang="ts">
/** An org: its repositories, and creating one. */
import { ref } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { css } from "styled-system/css";
import { badge, button } from "styled-system/recipes";
import CreateSlugForm from "../components/CreateSlugForm.vue";
import CopyButton from "../components/ui/CopyButton.vue";
import EmptyState from "../components/ui/EmptyState.vue";
import Icon from "../components/ui/Icon.vue";
import PageHeader from "../components/ui/PageHeader.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { client } from "../lib/client.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import { notify } from "../lib/toast.ts";

const props = defineProps<{ org: string }>();
const router = useRouter();

const status = useResource((signal) => client.getOrg(props.org, signal), [
	() => props.org,
	() => settings.sandbox,
]);
const repos = useResource((signal) => client.listRepos(props.org, signal), [
	() => props.org,
	() => settings.sandbox,
]);
const creating = ref(false);

const create = async (slug: string) => {
	const repo = await client.createRepo(props.org, slug);
	notify(`Created ${repo.org_slug}/${repo.slug}`, "success");
	await router.push({ name: "repo", params: { org: props.org, repo: repo.slug } });
};

const styles = {
	list: css({
		bg: "bg.surface",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "lg",
		overflow: "hidden",
		"& > li + li": { borderTopWidth: "1px", borderColor: "border.default" },
	}),
	row: css({
		display: "flex",
		alignItems: "center",
		gap: "3",
		px: "4",
		py: "3",
		flexWrap: { base: "wrap", md: "nowrap" },
		_hover: { bg: "bg.subtle" },
	}),
	link: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "2",
		fontWeight: "semibold",
		color: "fg.default",
		borderRadius: "sm",
		_hover: { color: "accent.fg", textDecoration: "underline" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px" },
	}),
	icon: css({ color: "fg.subtle" }),
	clone: css({
		ml: { md: "auto" },
		display: "flex",
		alignItems: "center",
		gap: "1",
		minW: 0,
		maxW: "full",
	}),
	url: css({
		fontFamily: "mono",
		fontSize: "xs",
		color: "fg.muted",
		bg: "bg.subtle",
		px: "2",
		py: "1",
		borderRadius: "md",
		overflow: "hidden",
		textOverflow: "ellipsis",
		whiteSpace: "nowrap",
	}),
	create: css({ mb: "6" }),
	eyebrow: css({ display: "flex", gap: "2", alignItems: "center", mb: "2", color: "fg.muted", fontSize: "xs" }),
	section: css({ fontSize: "sm", fontWeight: "semibold", color: "fg.muted", mb: "3", textTransform: "uppercase", letterSpacing: "0.04em" }),
	stack: css({ display: "grid", gap: "4" }),
};
</script>

<template>
	<ProblemAlert v-if="status.error.value" :error="status.error.value" @retry="status.reload()" />
	<template v-else>
		<PageHeader :title="props.org" description="Repositories in this org.">
			<template #eyebrow>
				<div :class="styles.eyebrow">
					<Icon name="org" :size="14" /> Organisation
					<span v-if="status.data.value?.pool" :class="badge()">pool {{ status.data.value.pool }}</span>
				</div>
			</template>
			<template #actions>
				<button
					type="button"
					:class="button({ variant: 'solid' })"
					:aria-expanded="creating"
					@click="creating = !creating"
				>
					<Icon name="plus" /> New repository
				</button>
			</template>
		</PageHeader>

		<div v-if="creating" :class="styles.create">
			<CreateSlugForm
				label="Repository name"
				placeholder="api"
				submit="Create repository"
				:create="create"
				@cancel="creating = false"
			/>
		</div>

		<ProblemAlert v-if="repos.error.value" :error="repos.error.value" @retry="repos.reload()" />
		<Skeleton v-else-if="repos.loading.value && !repos.data.value" :rows="3" />
		<template v-else-if="repos.data.value">
			<EmptyState
				v-if="repos.data.value.length === 0"
				icon="repo"
				title="No repositories yet"
				description="A repository is a Git repository on the forge's Git host. Its committed tartan.cue decides which extensions it runs."
			>
				<button type="button" :class="button({ variant: 'solid' })" @click="creating = true">
					<Icon name="plus" /> Create a repository
				</button>
			</EmptyState>
			<ul v-else :class="styles.list" role="list">
				<li v-for="repo in repos.data.value" :key="repo.slug" :class="styles.row">
					<RouterLink :to="{ name: 'repo', params: { org: props.org, repo: repo.slug } }" :class="styles.link">
						<Icon name="repo" :class="styles.icon" /> {{ repo.slug }}
					</RouterLink>
					<div :class="styles.clone">
						<code :class="styles.url" :title="repo.clone_url">{{ repo.clone_url }}</code>
						<CopyButton :text="repo.clone_url" :label="`Copy the clone URL of ${repo.path}`" />
					</div>
				</li>
			</ul>
		</template>
	</template>
</template>
