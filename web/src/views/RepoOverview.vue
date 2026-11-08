<script setup lang="ts">
/**
 * A repository's overview: its installed extensions, and the files and commands that
 * configure it. Extensions are installed only by the repository's committed CUE, so the
 * onboarding shows what to commit rather than offering an install button.
 */
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { css } from "styled-system/css";
import { badge, button, card } from "styled-system/recipes";
import CodeBlock from "../components/ui/CodeBlock.vue";
import Icon from "../components/ui/Icon.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { ISSUES_ID, KANBAN_ID } from "../lib/api.ts";
import { moduleCue, pushCommands, tartanCue } from "../lib/onboarding.ts";
import { useRepo } from "../lib/repo-context.ts";
import { cueRegistry, gatewayOrigin, settings } from "../lib/settings.ts";

const { org, repo, info, extensions, installed } = useRepo();

const registry = computed(() => cueRegistry(gatewayOrigin()));
const commands = computed(() =>
	pushCommands({
		cloneUrl: info.data.value?.clone_url ?? `<clone URL of ${org.value}/${repo.value}>`,
		repo: repo.value,
		registry: registry.value,
		gitToken: settings.gitToken,
	})
);
const configured = computed(() => (extensions.data.value?.length ?? 0) > 0);

const names: Record<string, string> = {
	[ISSUES_ID]: "Issues",
	[KANBAN_ID]: "Kanban",
};

const cardStyles = card();
const styles = {
	grid: css({
		display: "grid",
		gridTemplateColumns: { base: "1fr", lg: "minmax(0, 1fr) 340px" },
		gap: "6",
		alignItems: "start",
	}),
	stack: css({ display: "grid", gap: "4" }),
	lead: css({ color: "fg.muted", textWrap: "pretty" }),
	steps: css({ display: "grid", gap: "5" }),
	step: css({ display: "grid", gap: "2" }),
	stepHead: css({ display: "flex", alignItems: "center", gap: "2", fontWeight: "semibold" }),
	num: css({
		display: "grid",
		placeItems: "center",
		w: "6",
		h: "6",
		borderRadius: "full",
		bg: "accent.subtle",
		color: "accent.fg",
		fontSize: "xs",
		fontWeight: "bold",
	}),
	note: css({ color: "fg.muted", fontSize: "xs", "& code": { fontSize: "xs", bg: "bg.subtle", px: "1", borderRadius: "sm" } }),
	ext: css({
		display: "grid",
		gridTemplateColumns: "auto 1fr auto",
		alignItems: "center",
		gap: "3",
		py: "2.5",
		"& + &": { borderTopWidth: "1px", borderColor: "border.default" },
	}),
	extIcon: css({
		display: "grid",
		placeItems: "center",
		w: "8",
		h: "8",
		borderRadius: "md",
		bg: "accent.subtle",
		color: "accent.fg",
	}),
	extName: css({ fontWeight: "medium" }),
	extId: css({ fontFamily: "mono", fontSize: "xs", color: "fg.muted" }),
	digest: css({ fontFamily: "mono", fontSize: "2xs", color: "fg.subtle", wordBreak: "break-all" }),
	empty: css({ color: "fg.muted", fontSize: "sm", textWrap: "pretty" }),
	dl: css({
		display: "grid",
		gridTemplateColumns: "auto 1fr",
		gap: "2",
		columnGap: "4",
		fontSize: "sm",
		"& dt": { color: "fg.muted" },
		"& dd": { fontFamily: "mono", fontSize: "xs", wordBreak: "break-all", alignSelf: "center" },
	}),
	quick: css({ display: "flex", gap: "2", flexWrap: "wrap" }),
	registry: css({
		display: "flex",
		alignItems: "center",
		gap: "2",
		flexWrap: "wrap",
		p: "3",
		bg: "accent.subtle",
		borderWidth: "1px",
		borderColor: "accent.border",
		borderRadius: "md",
		fontSize: "sm",
		"& code": { fontWeight: "semibold" },
	}),
};
</script>

<template>
	<div :class="styles.grid">
		<section :class="cardStyles.root" aria-labelledby="setup-title">
			<div :class="cardStyles.header">
				<h2 id="setup-title" :class="cardStyles.title">
					{{ configured ? "Configuration" : "Set up this repository" }}
				</h2>
				<span :class="badge({ tone: configured ? 'success' : 'warning' })">
					{{ configured ? "configured" : "not configured" }}
				</span>
			</div>
			<div :class="[cardStyles.body, styles.steps]">
				<p :class="styles.lead">
					Extensions are installed by the repository's committed CUE. The forge evaluates the root
					package <code>tartan</code> exactly as <code>cue export</code> does, against its own
					registry, so what you check locally is what the forge applies.
				</p>
				<div :class="styles.registry">
					<Icon name="terminal" />
					<span>Point <code>cue</code> at this forge:</span>
					<code>CUE_REGISTRY={{ registry }}</code>
				</div>
				<div :class="styles.step">
					<div :class="styles.stepHead"><span :class="styles.num">1</span> Declare the module and its dependencies</div>
					<CodeBlock filename="cue.mod/module.cue" :code="moduleCue(org, repo)" />
				</div>
				<div :class="styles.step">
					<div :class="styles.stepHead"><span :class="styles.num">2</span> Install Issues and a Kanban board</div>
					<CodeBlock filename="tartan.cue" :code="tartanCue()" />
					<p :class="styles.note">
						A board's <code>columns</code> must be one of Issues' select properties, so a typo fails
						<code>cue export</code> before it reaches the forge.
					</p>
				</div>
				<div :class="styles.step">
					<div :class="styles.stepHead"><span :class="styles.num">3</span> Check it and push to <code>main</code></div>
					<CodeBlock language="shell" :code="commands" />
					<p :class="styles.note">
						The push uses the Git host token from <RouterLink :to="{ name: 'settings' }">Settings</RouterLink>.
						Extensions appear here a moment after the push, once the forge has applied the configuration.
					</p>
				</div>
			</div>
		</section>

		<div :class="styles.stack">
			<section :class="cardStyles.root" aria-labelledby="ext-title">
				<div :class="cardStyles.header">
					<h2 id="ext-title" :class="cardStyles.title">Installed extensions</h2>
					<button
						type="button"
						:class="button({ size: 'xs', variant: 'ghost' })"
						aria-label="Refresh extensions"
						@click="extensions.reload()"
					>
						<Icon name="refresh" :size="14" />
					</button>
				</div>
				<div :class="cardStyles.body">
					<ProblemAlert v-if="extensions.error.value" :error="extensions.error.value" compact @retry="extensions.reload()" />
					<Skeleton v-else-if="!extensions.data.value" :rows="2" />
					<p v-else-if="extensions.data.value.length === 0" :class="styles.empty">
						None yet. Push a <code>tartan.cue</code> that installs extensions, then refresh.
					</p>
					<template v-else>
						<div v-for="ext in extensions.data.value" :key="ext.installation_id" :class="styles.ext">
							<span :class="styles.extIcon"><Icon :name="ext.extension_id === KANBAN_ID ? 'board' : ext.extension_id === ISSUES_ID ? 'issue-open' : 'puzzle'" /></span>
							<div>
								<div :class="styles.extName">{{ names[ext.extension_id] ?? ext.extension_id }}</div>
								<div :class="styles.extId">{{ ext.extension_id }}</div>
								<div :class="styles.digest" :title="ext.digest">{{ ext.digest.slice(0, 19) }}…</div>
							</div>
							<span :class="badge({ tone: 'accent' })">v{{ ext.version }}</span>
						</div>
					</template>
				</div>
			</section>

			<section v-if="installed(ISSUES_ID) || installed(KANBAN_ID)" :class="cardStyles.root" aria-labelledby="go-title">
				<div :class="cardStyles.header"><h2 id="go-title" :class="cardStyles.title">Jump in</h2></div>
				<div :class="[cardStyles.body, styles.quick]">
					<RouterLink v-if="installed(ISSUES_ID)" :to="{ name: 'issue-new', params: { org, repo } }" :class="button({ variant: 'solid', size: 'sm' })">
						<Icon name="plus" :size="14" /> New issue
					</RouterLink>
					<RouterLink v-if="installed(ISSUES_ID)" :to="{ name: 'issues', params: { org, repo } }" :class="button({ size: 'sm' })">
						<Icon name="issue-open" :size="14" /> Issues
					</RouterLink>
					<RouterLink v-if="installed(KANBAN_ID)" :to="{ name: 'boards', params: { org, repo } }" :class="button({ size: 'sm' })">
						<Icon name="board" :size="14" /> Boards
					</RouterLink>
				</div>
			</section>

			<section v-if="info.data.value" :class="cardStyles.root" aria-labelledby="details-title">
				<div :class="cardStyles.header"><h2 id="details-title" :class="cardStyles.title">Details</h2></div>
				<dl :class="[cardStyles.body, styles.dl]">
					<dt>Path</dt><dd>{{ info.data.value.org_slug }}/{{ info.data.value.slug }}</dd>
					<dt>Actor</dt><dd>{{ info.data.value.id }}</dd>
					<dt>Pool</dt><dd>{{ info.data.value.hosted_in ?? "default" }}</dd>
				</dl>
			</section>
		</div>
	</div>
</template>
