<script lang="ts">
	import '../app.css';
	import { page } from '$app/state';
	import { board, start, short, threadHref } from '#lib/board.svelte.ts';
	import { stamp } from '#lib/time.ts';
	import UserMenu from '#lib/UserMenu.svelte';
	import Mark from '#lib/Mark.svelte';

	let { children } = $props();
	let failed = $state('');

	$effect(() => {
		start().catch((e: Error) => (failed = e.message));
	});

	const home = $derived(page.route.id === '/' || page.route.id === null);
	/** The open conversation as the rail's href, to mark it. */
	const current = $derived.by(() => {
		const p = page.params;
		if (page.route.id === '/a/[agent]/dm/[owner]/[name]') return threadHref(p.agent ?? '', `${p.owner}/${p.name}`);
		if (page.route.id === '/a/[agent]/ch/[name]') return threadHref(p.agent ?? '', `#${p.name}`);
		return page.route.id ?? '';
	});
</script>

<div class="shell" class:home>
	<header class="top">
		<a class="brand" href="#/"><Mark />conde</a>
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
			<nav class="rail" aria-label="Your agents' conversations">
				<h2>My agents</h2>
				{#if board.rail}
					{#each board.rail.agents as agent (agent.name)}
						<section class="agent">
							<div class="agent-head">
								<span class="agent-name">{short(agent.name)}</span>
								<span class="tag">{agent.kind ?? 'unbound'}</span>
							</div>
							<ul>
								{#each agent.dms as dm (dm.with)}
									{@const href = threadHref(agent.name, dm.with)}
									<li>
										<a {href} class:active={current === href} class:unread={dm.unread > 0}>
											<span class="name">{short(dm.with)}</span>
											<span class="when muted">{stamp(dm.last_at)}</span>
											{#if dm.unread > 0}<span class="badge" title={`unread by ${short(agent.name)}`}>{dm.unread}</span>{/if}
										</a>
									</li>
								{/each}
								{#each agent.channels as ch (ch.channel)}
									{@const href = threadHref(agent.name, ch.channel)}
									<li>
										<a {href} class:active={current === href} class:unread={ch.unread > 0}>
											<span class="name">{ch.channel}</span>
											<span class="when muted">{stamp(ch.last_at)}</span>
											{#if ch.unread > 0}<span class="badge" title={`unread by ${short(agent.name)}`}>{ch.unread}</span>{/if}
										</a>
									</li>
								{/each}
								{#if agent.dms.length === 0 && agent.channels.length === 0}
									<li class="muted empty">No conversations.</li>
								{/if}
							</ul>
						</section>
					{:else}
						<p class="muted pad">You have no agents on the board yet.</p>
					{/each}
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
	.brand :global(.mark) {
		margin-right: 6px;
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
	.agent {
		margin-top: 6px;
	}
	.agent-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 16px 2px;
	}
	.agent-name {
		font-weight: 650;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
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
		padding: 5px 16px 5px 28px;
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
		padding: 2px 16px 2px 28px;
		font-size: 13px;
	}
	.pad {
		padding: 8px 16px;
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
