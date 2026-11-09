<script setup lang="ts">
/**
 * Settings: the person's access tokens, how to sign with `tt`, their sessions, and this
 * browser's own preferences (the `cue` address, the Git host token, theme, refresh and
 * sandbox). The forge knows the browser by its session cookie; no forge credential is kept
 * here.
 */
import { computed, ref } from "vue";
import { css } from "styled-system/css";
import { badge, button, card, field } from "styled-system/recipes";
import SessionSettings from "../components/settings/SessionSettings.vue";
import SigningSettings from "../components/settings/SigningSettings.vue";
import TokenSettings from "../components/settings/TokenSettings.vue";
import Icon from "../components/ui/Icon.vue";
import PageHeader from "../components/ui/PageHeader.vue";
import {
	cueRegistry,
	DEV_GIT_TOKEN,
	gatewayOrigin,
	settings,
	type ThemePreference,
} from "../lib/settings.ts";

const showGitToken = ref(false);

const origin = computed(() => gatewayOrigin());
const registry = computed(() => cueRegistry(origin.value));
const themes: { value: ThemePreference; label: string }[] = [
	{ value: "system", label: "System" },
	{ value: "light", label: "Light" },
	{ value: "dark", label: "Dark" },
];

const cardStyles = card();
const styles = {
	stack: css({ display: "grid", gap: "4", maxW: "3xl" }),
	row: css({ display: "grid", gap: "1.5" }),
	label: css({ fontSize: "sm", fontWeight: "semibold" }),
	help: css({ fontSize: "xs", color: "fg.muted", textWrap: "pretty", "& code": { bg: "bg.subtle", px: "1", borderRadius: "sm" } }),
	inline: css({ display: "flex", gap: "2", alignItems: "center" }),
	body: css({ display: "grid", gap: "5" }),
	result: css({ display: "flex", alignItems: "center", gap: "2", fontSize: "sm", color: "success.fg" }),
	radios: css({ display: "flex", gap: "2", flexWrap: "wrap" }),
	radio: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "2",
		h: "9",
		px: "3",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "md",
		cursor: "pointer",
		_hover: { borderColor: "border.strong" },
		"&:has(input:checked)": { borderColor: "accent.solid", bg: "accent.subtle" },
		"&:has(input:focus-visible)": { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px" },
	}),
	check: css({ display: "flex", gap: "3", alignItems: "flex-start", cursor: "pointer", "& input": { mt: "1" } }),
	number: css({ w: "24 !important" }),
};
</script>

<template>
	<PageHeader title="Settings" description="Your tokens, signing and sessions, and this browser's preferences." />
	<div :class="styles.stack">
		<TokenSettings />
		<SigningSettings />
		<SessionSettings />
		<section :class="cardStyles.root" aria-labelledby="gw-title">
			<div :class="cardStyles.header"><h2 id="gw-title" :class="cardStyles.title">This browser</h2></div>
			<div :class="[cardStyles.body, styles.body]">
				<div :class="styles.row">
					<label for="address" :class="styles.label">Gateway address for <code>cue</code></label>
					<input
						id="address"
						v-model.trim="settings.gatewayAddress"
						:class="field({ mono: true })"
						:placeholder="origin"
						spellcheck="false"
						aria-describedby="address-help"
					/>
					<p id="address-help" :class="styles.help">
						Where <code>cue</code> on your machine reaches this forge's registry. The setup steps use
						<code>CUE_REGISTRY={{ registry }}</code>.
					</p>
				</div>
				<div :class="styles.row">
					<label for="git-token" :class="styles.label">Git host token</label>
					<div :class="styles.inline">
						<input
							id="git-token"
							v-model.trim="settings.gitToken"
							:type="showGitToken ? 'text' : 'password'"
							:class="field({ mono: true })"
							autocomplete="off"
							spellcheck="false"
							aria-describedby="git-token-help"
						/>
						<button
							type="button"
							:class="button({ variant: 'ghost', size: 'md' })"
							:aria-label="showGitToken ? 'Hide the Git host token' : 'Show the Git host token'"
							:aria-pressed="showGitToken"
							@click="showGitToken = !showGitToken"
						>
							<Icon :name="showGitToken ? 'eye-off' : 'eye'" />
						</button>
					</div>
					<p id="git-token-help" :class="styles.help">
						Shown in the <code>git push</code> command of a repository's setup steps; the Git host checks it, not
						the gateway. <code>scripts/dev.sh</code> configures <code>{{ DEV_GIT_TOKEN }}</code>.
					</p>
				</div>
			</div>
		</section>

		<section :class="cardStyles.root" aria-labelledby="ui-title">
			<div :class="cardStyles.header"><h2 id="ui-title" :class="cardStyles.title">Interface</h2></div>
			<div :class="[cardStyles.body, styles.body]">
				<fieldset :class="styles.row">
					<legend :class="styles.label">Theme</legend>
					<div :class="styles.radios">
						<label v-for="t in themes" :key="t.value" :class="styles.radio">
							<input v-model="settings.theme" type="radio" name="theme" :value="t.value" /> {{ t.label }}
						</label>
					</div>
				</fieldset>
				<div :class="styles.row">
					<label for="poll" :class="styles.label">Board refresh, in seconds</label>
					<input id="poll" v-model.number="settings.pollSeconds" type="number" min="1" max="60" :class="[field(), styles.number]" />
					<p :class="styles.help">Boards update from Issues' events, which arrive a moment after a change.</p>
				</div>
				<label :class="styles.check">
					<input v-model="settings.sandbox" type="checkbox" aria-describedby="sandbox-help" />
					<span>
						<span :class="styles.label">Sandbox</span>
						<span v-if="settings.sandbox" :class="badge({ tone: 'warning' })" style="margin-left: 8px">on</span>
						<p id="sandbox-help" :class="styles.help">
							Answer every request from sample data in this browser instead of the gateway: an org
							<code>acme</code> with a repository <code>api</code> that has Issues and Kanban. Changes last until the
							page reloads.
						</p>
					</span>
				</label>
			</div>
		</section>
	</div>
</template>
