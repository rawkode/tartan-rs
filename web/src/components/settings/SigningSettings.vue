<script setup lang="ts">
/**
 * How to sign commits with `tt` (design D15, *Settings*), with this forge's address and the
 * person's DID filled in. Signatures are records in the person's own ATProto repository, so
 * there is nothing to manage on the forge: no keys are stored or listed.
 */
import { computed } from "vue";
import { css } from "styled-system/css";
import { card } from "styled-system/recipes";
import { signedIn } from "../../lib/account.ts";
import CodeBlock from "../ui/CodeBlock.vue";

const forge = computed(() => globalThis.location?.origin ?? "https://forge.example");
const did = computed(() => signedIn.value?.did ?? "<your DID>");

const login = computed(() => `tt login --forge ${forge.value}`);
const setup = "tt sign --setup --global";
const writes = computed(() =>
	[
		"gpg.format=x509",
		"gpg.x509.program=tt",
		`user.signingKey=${did.value}`,
		"commit.gpgSign=true",
		"tag.gpgSign=true",
	].join("\n")
);
const hook = "# in each repository where you push work signed offline\ntt sign --setup --hook\n# or, before pushing\ntt sign --flush";
const revoke = "tt sign revoke <commit>";

const cardStyles = card();
const styles = {
	body: css({ display: "grid", gap: "4" }),
	step: css({ display: "grid", gap: "2" }),
	head: css({ fontWeight: "semibold", fontSize: "sm" }),
	help: css({ fontSize: "sm", color: "fg.muted", textWrap: "pretty" }),
};
</script>

<template>
	<section :class="cardStyles.root" aria-labelledby="signing-title">
		<div :class="cardStyles.header"><h2 id="signing-title" :class="cardStyles.title">Signing</h2></div>
		<div :class="[cardStyles.body, styles.body]" data-signing>
			<p :class="styles.help">
				Sign commits and tags with <code>tt</code>: each signature is a record in your own ATProto
				repository that the commit refers to. There is nothing to manage here: the forge stores and
				lists no keys.
			</p>
			<div :class="styles.step">
				<div :class="styles.head">1. Install <code>tt</code> (a single binary) and sign in to this forge</div>
				<CodeBlock language="shell" :code="login" />
				<p :class="styles.help">
					<code>tt login</code> also signs <code>tt</code> in to your PDS for signing, as {{ did }}.
				</p>
			</div>
			<div :class="styles.step">
				<div :class="styles.head">2. Make Git sign with <code>tt</code> (on a personal machine)</div>
				<CodeBlock language="shell" :code="setup" />
				<p :class="styles.help">It sets, in your global Git configuration:</p>
				<CodeBlock :code="writes" />
				<p :class="styles.help">
					<code>gpg.x509.program</code> may be <code>tt</code>'s absolute path instead.
				</p>
			</div>
			<div :class="styles.step">
				<div :class="styles.head">3. Publish signatures made offline</div>
				<CodeBlock language="shell" :code="hook" />
				<p :class="styles.help">
					<code>--global</code> cannot install the per-repository <code>pre-push</code> hook that publishes queued
					signature records.
				</p>
			</div>
			<div :class="styles.step">
				<div :class="styles.head">Withdraw a signature</div>
				<CodeBlock language="shell" :code="revoke" />
			</div>
		</div>
	</section>
</template>
