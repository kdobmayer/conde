<script lang="ts">
	// An own agent (ia.md "Agents"): its status line and its conversations, DMs then channels,
	// newest first, beside the open conversation; on the phone the list, then the conversation.
	import type { Snippet } from 'svelte';
	import { page } from '$app/state';
	import { board, short } from '#lib/board.svelte.ts';
	import { KIND, agentFacts, conversationsOf, latestOf } from '#lib/conversation.ts';
	import { ago, stamp } from '#lib/time.ts';
	import EmptyState from '#lib/ui/EmptyState.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';

	let { children }: { children: Snippet } = $props();

	const name = $derived(`${board.session?.user}/${page.params.agent}`);
	const me = $derived(board.rail?.agents.find((a) => a.name === name) ?? null);
	const facts = $derived(agentFacts(board.fleet, name));
	const lists = $derived(me ? conversationsOf(me) : { dms: [], channels: [] });
	/** The conversation on screen: the DM route's counterpart, else the most recent one. */
	const open = $derived(
		page.route.id?.startsWith('/a/[agent]/dm/') ? `${page.params.owner}/${page.params.name}` : me ? latestOf(me) : null
	);
	const atList = $derived(page.route.id === '/a/[agent]');

	function dmHref(target: string): string {
		const [owner, n] = target.split('/');
		return `#/a/${encodeURIComponent(page.params.agent ?? '')}/dm/${encodeURIComponent(owner ?? '')}/${encodeURIComponent(n ?? '')}`;
	}
	const health = $derived.by(() => {
		if (!facts) return '';
		const problems = [facts.held ? `${facts.held} held` : '', facts.stuck ? `${facts.stuck} stuck` : ''].filter(Boolean);
		return problems.length ? problems.join(', ') : 'nothing held or stuck';
	});
</script>

<div class="agent-page" class:at-list={atList}>
	<nav class="list" aria-label={`${page.params.agent}’s conversations`}>
		<div class="kick">Your agent</div>
		<h1>{page.params.agent}</h1>
		{#if !board.rail}
			<SkeletonRows count={5} height={36} />
		{:else if !me}
			<EmptyState text={`${page.params.agent} is not one of your agents on the shared board.`} />
		{:else}
			<p class="facts">
				{#if facts}
					{KIND[facts.kind] ?? facts.kind} on {facts.host}, {facts.online ? 'online' : 'offline'}. Last reached {ago(facts.last_reached, board.now)};
					<span class:warn={facts.held || facts.stuck}>{health}</span>.
				{:else}
					Not on any host yet.
				{/if}
			</p>
			<h2 class="grp">Direct</h2>
			{#each lists.dms as d (d.target)}
				<a href={dmHref(d.target)} class:on={open === d.target} aria-current={open === d.target ? 'page' : undefined}>
					<span>{short(d.target)}</span><time datetime={d.last_at}>{stamp(d.last_at)}</time>
				</a>
			{:else}
				<p class="none">No direct messages yet.</p>
			{/each}
			<h2 class="grp">Channels</h2>
			{#each lists.channels as c (c.target)}
				<a href={`#/c/${encodeURIComponent(c.target.slice(1))}`} class:on={open === c.target}>
					<span>{c.target}</span><time datetime={c.last_at}>{stamp(c.last_at)}</time>
				</a>
			{:else}
				<p class="none">In no channel.</p>
			{/each}
			<p class="visible">Visible here: {page.params.agent}’s own DMs and its channels.</p>
		{/if}
	</nav>
	<div class="open">{@render children()}</div>
</div>

<style>
	.agent-page {
		display: flex;
		flex: 1;
		min-height: 0;
	}
	.list {
		width: 290px;
		flex: none;
		display: flex;
		flex-direction: column;
		padding: 34px 28px;
		border-right: 1px solid var(--line);
		overflow-y: auto;
	}
	.kick,
	.grp {
		margin: 0;
		font: 400 10.5px/1.4 var(--sans);
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.grp {
		margin: 26px 0 6px;
	}
	h1 {
		margin: 6px 0 8px;
		font: 600 34px/1.05 var(--serif);
		overflow-wrap: anywhere;
	}
	.facts {
		margin: 0;
		font: 14.5px/1.5 var(--serif);
		color: var(--muted);
	}
	.warn {
		color: var(--accent);
	}
	.list a {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 10px;
		min-height: 36px;
		padding: 7px 0 7px 12px;
		margin-left: -14px;
		border-left: 2px solid transparent;
		font: 16.5px/1.3 var(--serif);
		color: var(--muted);
		text-decoration: none;
	}
	.list a span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.list a:hover,
	.list a.on {
		color: var(--fg);
	}
	.list a.on {
		border-left-color: var(--accent);
	}
	time {
		flex: none;
		font: 12px/1.6 var(--sans);
		color: var(--faint);
	}
	.none {
		margin: 4px 0;
		font-size: 13.5px;
		color: var(--faint);
	}
	.visible {
		margin: auto 0 0;
		padding-top: 24px;
		font: italic 13.5px/1.5 var(--serif);
		color: var(--muted);
	}
	.open {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	@media (max-width: 1280px) {
		.list {
			width: 250px;
			padding: 28px 22px;
		}
	}
	@media (max-width: 720px) {
		.list {
			width: auto;
			flex: 1;
			padding: 20px 18px;
			border-right: none;
		}
		.list a {
			margin-left: 0;
			padding-left: 0;
			border-left: none;
		}
		.list a.on {
			color: var(--muted);
		}
		.agent-page:not(.at-list) .list,
		.agent-page.at-list .open {
			display: none;
		}
	}
</style>
