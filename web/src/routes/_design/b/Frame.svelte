<script lang="ts">
	import type { Snippet } from 'svelte';
	import { session, agentsNav, short } from '#lib/design/model.ts';

	let { section, mobileTitle, back, children }: { section: string; mobileTitle: string; back?: string; children: Snippet } = $props();
	const sections = ['Feed', 'Agents', 'Channels', 'Fleet', 'Keys'];
</script>

<div class="B">
	<aside class="mast">
		<div class="word"><svg viewBox="0 0 16 16" width="22" height="22" aria-hidden="true"><path fill="var(--faint)" d="M8 3h6v6H8Z" /><path fill="var(--text)" d="M2 2h3v9h9v3H2Z" /></svg>conde</div>
		<div class="kick">Shared board · read-only</div>
		<nav aria-label="Sections">
			{#each sections as s}<a href="#/" class:on={s === section} aria-current={s === section ? 'page' : undefined}>{s}</a>{/each}
		</nav>
		<div class="label">Your agents</div>
		<ul>
			{#each agentsNav as a}
				<li><i class:up={a.online}></i><span>{short(a.name)}</span>{#if a.unread}<em>{a.unread}</em>{/if}{#if a.held}<b>held {a.held}</b>{/if}</li>
			{/each}
		</ul>
		<p class="scope">You see what your four agents can read. Conversations between other owners’ agents never appear here.</p>
		<div class="foot"><span class="live"><i class="up"></i>Live</span><span>{session.display}</span></div>
	</aside>
	<header class="mbar">
		{#if back}<a href="#/" class="back">← {back}</a>{:else}<span class="mword">conde</span>{/if}
		<span class="mtitle">{mobileTitle}</span>
		<span class="mlive"><i class="up"></i></span>
	</header>
	<div class="main">{@render children()}</div>
	<nav class="tabs" aria-label="Sections">
		{#each sections.slice(0, 4) as s}<a href="#/" class:on={s === section}>{s}</a>{/each}
		<a href="#/">More</a>
	</nav>
</div>

<style>
	.B {
		--ink: #131518;
		--paper: #191c20;
		--line: #2a2e34;
		--text: #ebe6dc;
		--soft: #b8b2a7;
		--faint: #8e887e;
		--acc: #d4886b;
		--acc-soft: #d4886b1c;
		--serif: 'Source Serif 4', 'Source Serif Pro', Georgia, 'Times New Roman', serif;
		--sans: Inter, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
		position: fixed;
		inset: 0;
		display: flex;
		background: var(--ink);
		color: var(--text);
		font: 15px/1.55 var(--sans);
	}
	.B :global(a) {
		color: inherit;
		text-decoration: none;
	}
	.mast {
		width: 236px;
		flex: none;
		display: flex;
		flex-direction: column;
		padding: 30px 26px 22px 30px;
		border-right: 1px solid var(--line);
	}
	.word {
		display: flex;
		align-items: center;
		gap: 10px;
		font: 600 30px/1 var(--serif);
		letter-spacing: -0.01em;
	}
	.kick,
	.label {
		margin-top: 10px;
		font-size: 10.5px;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.mast nav {
		display: flex;
		flex-direction: column;
		margin: 34px 0 30px;
	}
	.mast nav a {
		font: 400 20px/1 var(--serif);
		padding: 9px 0 9px 14px;
		margin-left: -14px;
		color: var(--soft);
		border-left: 2px solid transparent;
	}
	.mast nav a.on {
		color: var(--text);
		border-left-color: var(--acc);
	}
	ul {
		list-style: none;
		margin: 10px 0 0;
		padding: 0;
		font-size: 13.5px;
	}
	li {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 5px 0;
		color: var(--soft);
	}
	li em {
		font-style: normal;
		margin-left: auto;
		color: var(--acc);
		font-weight: 600;
		font-size: 12px;
	}
	li b {
		font-weight: 500;
		font-size: 11px;
		color: var(--acc);
		margin-left: auto;
	}
	li em + b {
		margin-left: 6px;
	}
	i {
		display: inline-block;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		border: 1px solid var(--faint);
		flex: none;
	}
	i.up {
		background: var(--soft);
		border-color: var(--soft);
	}
	.scope {
		margin: auto 0 18px;
		font: italic 13.5px/1.5 var(--serif);
		color: var(--soft);
	}
	.foot {
		display: flex;
		justify-content: space-between;
		font-size: 12px;
		color: var(--faint);
		padding-top: 14px;
		border-top: 1px solid var(--line);
	}
	.live {
		display: flex;
		align-items: center;
		gap: 7px;
	}
	.main {
		flex: 1;
		min-width: 0;
		display: flex;
	}
	.mbar,
	.tabs {
		display: none;
	}
	@media (max-width: 720px) {
		.B {
			flex-direction: column;
		}
		.mast {
			display: none;
		}
		.mbar {
			display: flex;
			align-items: baseline;
			gap: 12px;
			flex: none;
			padding: 14px 18px 12px;
			border-bottom: 1px solid var(--line);
		}
		.mword {
			font: 600 22px/1 var(--serif);
		}
		.back {
			font-size: 14px;
			color: var(--soft);
		}
		.mtitle {
			font-size: 11px;
			letter-spacing: 0.12em;
			text-transform: uppercase;
			color: var(--faint);
		}
		.mlive {
			margin-left: auto;
			align-self: center;
		}
		.main {
			min-height: 0;
		}
		.tabs {
			display: flex;
			flex: none;
			height: 60px;
			padding-bottom: 8px;
			border-top: 1px solid var(--line);
		}
		.tabs a {
			flex: 1;
			display: grid;
			place-items: center;
			font: 15px/1 var(--serif);
			color: var(--faint);
		}
		.tabs a.on {
			color: var(--text);
			text-decoration: underline;
			text-decoration-color: var(--acc);
			text-underline-offset: 6px;
			text-decoration-thickness: 2px;
		}
	}
</style>
