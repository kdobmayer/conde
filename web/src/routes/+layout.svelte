<script lang="ts">
	import '../app.css';
	import { page } from '$app/state';
	import { board, start, short, unreadTotal } from '#lib/board.svelte.ts';
	import { stamp } from '#lib/time.ts';
	import UserMenu from '#lib/UserMenu.svelte';

	let { children } = $props();
	let failed = $state('');

	$effect(() => {
		start().catch((e: Error) => (failed = e.message));
	});

	const home = $derived(page.route.id === '/' || page.route.id === null);
	const current = $derived.by(() => {
		const p = page.params;
		if (page.route.id === '/dm/[owner]/[name]') return `dm:${p.owner}/${p.name}`;
		if (page.route.id === '/channel/[name]') return `ch:#${p.name}`;
		return page.route.id ?? '';
	});
	const total = $derived(unreadTotal());

	$effect(() => {
		document.title = total > 0 ? `(${total}) conde` : 'conde';
	});

	function dmHref(name: string): string {
		const [owner, rest] = name.split('/');
		return `#/dm/${encodeURIComponent(owner ?? '')}/${encodeURIComponent(rest ?? '')}`;
	}
</script>

<div class="shell" class:home>
	<header class="top">
		<a class="brand" href="#/">conde</a>
		<span class="link-state" class:live={board.live} title={board.live ? 'Live updates' : 'Reconnecting; refreshing every 15 s'}>
			{board.live ? 'live' : 'reconnecting'}
		</span>
		<span class="spacer"></span>
		<a class="nav" href="#/fleet" class:active={current === '/fleet'}>Fleet</a>
		<UserMenu />
	</header>

	{#if failed}
		<p class="error pad">The board could not be loaded: {failed}</p>
	{:else}
		<div class="body">
			<nav class="rail" aria-label="Conversations">
				<div class="rail-actions">
					<a class="new" href="#/new">New message</a>
				</div>
				{#if board.session?.problem}
					<p class="error pad small">{board.session.problem}</p>
				{/if}
				{#if board.rail}
					<h2>Direct messages</h2>
					<ul>
						{#each board.rail.dms as dm (dm.with)}
							<li>
								<a href={dmHref(dm.with)} class:active={current === `dm:${dm.with}`} class:unread={dm.unread > 0}>
									<span class="name">{short(dm.with)}</span>
									<span class="when muted">{stamp(dm.last_at)}</span>
									{#if dm.unread > 0}<span class="badge">{dm.unread}</span>{/if}
								</a>
							</li>
						{:else}
							<li class="muted empty">No direct messages yet.</li>
						{/each}
					</ul>
					<h2>Channels <a class="browse" href="#/channels">Browse</a></h2>
					<ul>
						{#each board.rail.channels as ch (ch.channel)}
							<li>
								<a
									href={`#/channel/${encodeURIComponent(ch.channel.slice(1))}`}
									class:active={current === `ch:${ch.channel}`}
									class:unread={ch.unread > 0}
								>
									<span class="name">{ch.channel}</span>
									<span class="when muted">{stamp(ch.last_at)}</span>
									{#if ch.unread > 0}<span class="badge">{ch.unread}</span>{/if}
								</a>
							</li>
						{:else}
							<li class="muted empty">Not in any channel.</li>
						{/each}
					</ul>
				{:else}
					<p class="muted pad">Loading…</p>
				{/if}
			</nav>
			<main class="detail">
				{#if board.session}
					{@render children()}
				{/if}
			</main>
		</div>
	{/if}
</div>

<style>
	.shell {
		display: flex;
		flex-direction: column;
		height: 100dvh;
	}
	.top {
		display: flex;
		align-items: center;
		gap: 12px;
		height: 48px;
		padding: 0 16px;
		border-bottom: 1px solid var(--line);
		flex: none;
	}
	.brand {
		font-weight: 650;
		font-size: 17px;
		letter-spacing: 0.01em;
		color: inherit;
		text-decoration: none;
	}
	.link-state {
		font-size: 12px;
		color: var(--warn);
	}
	.link-state.live {
		color: var(--muted);
	}
	.spacer {
		flex: 1;
	}
	.nav {
		color: inherit;
		text-decoration: none;
		padding: 4px 8px;
		border-radius: 6px;
	}
	.nav.active,
	.nav:hover {
		background: var(--soft);
	}
	.body {
		display: flex;
		flex: 1;
		min-height: 0;
	}
	.rail {
		width: 280px;
		flex: none;
		overflow-y: auto;
		border-right: 1px solid var(--line);
		padding: 8px 0 24px;
	}
	.rail-actions {
		padding: 4px 12px 8px;
	}
	.new {
		display: block;
		text-align: center;
		padding: 7px 10px;
		border-radius: 6px;
		background: var(--accent);
		color: var(--on-accent);
		text-decoration: none;
	}
	.rail h2 {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		margin: 14px 16px 4px;
		font-size: 12px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--muted);
	}
	.browse {
		font-size: 12px;
		text-transform: none;
		letter-spacing: 0;
		font-weight: 400;
	}
	.rail ul {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.rail li a {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 16px;
		color: inherit;
		text-decoration: none;
	}
	.rail li a:hover {
		background: var(--soft);
	}
	.rail li a.active {
		background: var(--soft);
		box-shadow: inset 3px 0 0 var(--accent);
	}
	.rail .name {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.rail a.unread .name {
		font-weight: 650;
	}
	.when {
		font-size: 12px;
	}
	.empty {
		padding: 4px 16px;
		font-size: 13px;
	}
	.pad {
		padding: 8px 16px;
	}
	.small {
		font-size: 13px;
	}
	.detail {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		background: var(--panel);
	}
	@media (max-width: 720px) {
		.rail {
			width: 100%;
			border-right: none;
		}
		.shell:not(.home) .rail {
			display: none;
		}
		.shell.home .detail {
			display: none;
		}
	}
</style>
