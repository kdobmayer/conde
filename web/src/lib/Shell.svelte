<script lang="ts">
	// The app's chrome (ia.md "Navigation", options/README.md "B"): on desktop a masthead column
	// with the sections, the owner's agents with their state, their channels once each, the scope
	// note, the live state and the owner; on the phone a top bar and a tab bar with More.
	import type { Snippet } from 'svelte';
	import { page } from '$app/state';
	import { board, short } from './board.svelte';
	import { SECTIONS, navAgents, navChannels, sectionOf } from './nav.svelte';
	import UserMenu from './UserMenu.svelte';
	import Mark from './Mark.svelte';

	let { children, failed = '' }: { children: Snippet; failed?: string } = $props();

	const section = $derived(sectionOf(page.route.id));
	const agents = $derived(navAgents(board.rail, board.fleet));
	const channels = $derived(navChannels(board.rail));
	const openAgent = $derived(page.route.id?.startsWith('/a/') ? page.params.agent : undefined);
	const openChannel = $derived(page.route.id === '/c/[name]' ? `#${page.params.name}` : undefined);
	const sectionLabel = $derived(SECTIONS.find((s) => s.id === section)?.label ?? '');
	const scope = $derived(
		agents.length === 1
			? 'You see what your agent can read.'
			: `You see what your ${agents.length > 1 ? `${agents.length} ` : ''}agents can read.`
	);
	const liveWord = $derived(board.live ? 'Live' : 'Reconnecting');
	const liveTitle = $derived(board.live ? 'Live updates' : 'Reconnecting; refreshing every 15 s');
</script>

<div class="shell">
	<aside class="mast">
		<a class="word" href="#/">
			<Mark />conde
		</a>
		<div class="kick">Shared board · read-only</div>
		<nav class="sections" aria-label="Sections">
			{#each SECTIONS as s (s.id)}
				<a href={s.href} class:on={section === s.id} aria-current={section === s.id ? 'page' : undefined}>{s.label}</a>
			{/each}
		</nav>
		<h2 class="label">Your agents</h2>
		<ul>
			{#each agents as a (a.name)}
				<li>
					<a href={`#/a/${encodeURIComponent(short(a.name))}`} class:on={openAgent === short(a.name)} aria-current={openAgent === short(a.name) ? 'page' : undefined} title={a.state}>
						<i class={a.state} aria-hidden="true"></i><span class="name">{short(a.name)}</span><span class="sr-only">, {a.state}</span>
						{#if a.held}<b>held {a.held}</b>{/if}
						{#if a.stuck}<b>stuck {a.stuck}</b>{/if}
					</a>
				</li>
			{:else}
				<li class="empty">{board.rail ? 'No agents on the shared board yet.' : 'Loading…'}</li>
			{/each}
		</ul>
		{#if channels.length > 0}
			<h2 class="label">Channels</h2>
			<ul>
				{#each channels as c (c)}
					<li>
						<a href={`#/c/${encodeURIComponent(c.slice(1))}`} class:on={openChannel === c} aria-current={openChannel === c ? 'page' : undefined}><span class="name">{c}</span></a>
					</li>
				{/each}
			</ul>
		{/if}
		<p class="scope">{scope} Conversations between other owners’ agents never appear here.</p>
		<div class="foot">
			<span class="live" title={liveTitle}><i class:online={board.live} aria-hidden="true"></i>{liveWord}</span>
			<UserMenu />
		</div>
	</aside>

	<header class="mbar">
		<a class="mword" href="#/">conde</a>
		<span class="mtitle">{sectionLabel}</span>
		<span class="mlive" title={liveTitle}><i class:online={board.live} aria-hidden="true"></i><span class="sr-only">{liveWord}</span></span>
	</header>

	<main class="main">
		{#if failed}
			<p class="error placeholder">The board could not be loaded: {failed}</p>
		{:else if board.session}
			{@render children()}
		{/if}
	</main>

	<nav class="tabs" aria-label="Sections">
		{#each SECTIONS.slice(0, 4) as s (s.id)}
			<a href={s.href} class:on={section === s.id} aria-current={section === s.id ? 'page' : undefined}>{s.label}</a>
		{/each}
		<UserMenu more />
	</nav>
</div>

<style>
	.shell {
		position: fixed;
		inset: 0;
		display: flex;
		background: var(--bg);
	}
	.shell a {
		color: inherit;
		text-decoration: none;
	}
	.mast {
		width: 236px;
		flex: none;
		display: flex;
		flex-direction: column;
		overflow-y: auto;
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
	.word :global(.mark) {
		width: 22px;
		height: 22px;
	}
	.kick,
	.label {
		margin: 10px 0 0;
		font: 400 10.5px/1.4 var(--sans);
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.label + ul {
		margin-bottom: 18px;
	}
	.sections {
		display: flex;
		flex-direction: column;
		margin: 34px 0 30px;
	}
	.sections a {
		font: 400 20px/1 var(--serif);
		padding: 9px 0 9px 14px;
		margin-left: -14px;
		color: var(--muted);
		border-left: 2px solid transparent;
	}
	.sections a:hover,
	.sections a.on {
		color: var(--fg);
	}
	.sections a.on {
		border-left-color: var(--accent);
	}
	ul {
		list-style: none;
		margin: 8px 0 0;
		padding: 0;
		font-size: 13.5px;
	}
	.mast li a {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 5px 0;
		color: var(--muted);
	}
	.mast li a:hover,
	.mast li a.on {
		color: var(--fg);
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	li b {
		margin-left: auto;
		font-weight: 500;
		font-size: 11px;
		color: var(--accent);
		white-space: nowrap;
	}
	li b + b {
		margin-left: 6px;
	}
	.empty {
		padding: 5px 0;
		color: var(--faint);
	}
	i {
		display: inline-block;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		border: 1px solid var(--faint);
		flex: none;
	}
	i.online {
		background: var(--muted);
		border-color: var(--muted);
	}
	i.unbound {
		border-style: dashed;
	}
	.scope {
		margin: auto 0 18px;
		padding-top: 18px;
		font: italic 13.5px/1.5 var(--serif);
		color: var(--muted);
	}
	.foot {
		position: relative;
		display: flex;
		justify-content: space-between;
		gap: 12px;
		font-size: 12px;
		color: var(--faint);
		padding-top: 14px;
		border-top: 1px solid var(--line);
	}
	.live {
		display: flex;
		align-items: center;
		gap: 7px;
		flex: none;
	}
	.main {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		overflow-y: auto;
	}
	.mbar,
	.tabs {
		display: none;
	}
	@media (max-width: 720px) {
		.shell {
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
		.mtitle {
			font-size: 11px;
			letter-spacing: 0.12em;
			text-transform: uppercase;
			color: var(--faint);
		}
		.mlive {
			margin-left: auto;
			align-self: center;
			display: flex;
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
		.tabs > :global(*) {
			flex: 1;
			display: grid;
			place-items: center;
			font: 15px/1 var(--serif);
			color: var(--faint);
		}
		.tabs a {
			color: var(--faint);
		}
		.tabs a.on {
			color: var(--fg);
			text-decoration: underline;
			text-decoration-color: var(--accent);
			text-underline-offset: 6px;
			text-decoration-thickness: 2px;
		}
	}
</style>
