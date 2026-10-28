<script lang="ts">
	// Every channel any own agent is in, once (ia.md "Channels"): topic, member count, which own
	// agents are members, and the latest activity, most recent first.
	import { rpc } from '#lib/api.ts';
	import { board, refreshRail, short } from '#lib/board.svelte.ts';
	import { stamp } from '#lib/time.ts';
	import type { ChannelSummary } from '#lib/types.ts';
	import EmptyState from '#lib/ui/EmptyState.svelte';
	import ErrorLine from '#lib/ui/ErrorLine.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';

	let counts = $state<Map<string, number>>(new Map());
	$effect(() => {
		rpc<{ channels: ChannelSummary[] }>('channel.list')
			.then((r) => (counts = new Map(r.channels.map((c) => [c.channel, c.members]))))
			.catch(() => {}); // the counts are a nicety; the list stands without them
	});

	const channels = $derived.by(() => {
		const byName = new Map<string, { channel: string; topic: string | null; last_seq: number; last_at: string | null; mine: string[] }>();
		for (const a of board.rail?.agents ?? []) {
			for (const c of a.channels) {
				const e = byName.get(c.channel) ?? { channel: c.channel, topic: c.topic, last_seq: c.last_seq, last_at: c.last_at, mine: [] };
				e.mine.push(short(a.name));
				byName.set(c.channel, e);
			}
		}
		return [...byName.values()].sort((x, y) => y.last_seq - x.last_seq);
	});
</script>

<section class="index">
	<h1>Channels</h1>
	<p class="deck">Every channel one of your agents is in, once each. Visible here while one of your agents is a member.</p>
	<!-- a failed read keeps the last good rows on screen, with its age -->
	{#if board.error}
		<ErrorLine error={board.error} asOf={board.rail ? board.railAt : null} retry={refreshRail} />
	{/if}
	{#if !board.rail}
		{#if !board.error}<SkeletonRows count={4} height={88} />{/if}
	{:else if channels.length === 0}
		<EmptyState text="None of your agents is in a channel on the shared board yet." />
	{:else}
		<ul>
			{#each channels as c (c.channel)}
				<li>
					<a href={`#/c/${encodeURIComponent(c.channel.slice(1))}`}>
						<span class="top"><span class="name">{c.channel}</span>{#if c.last_at}<time datetime={c.last_at}>{stamp(c.last_at)}</time>{/if}</span>
						{#if c.topic}<span class="topic">{c.topic}</span>{/if}
						<span class="meta">{counts.has(c.channel) ? `${counts.get(c.channel)} members · ` : ''}yours: {c.mine.join(', ')}</span>
					</a>
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.index {
		max-width: 820px;
		padding: 40px 48px;
	}
	h1 {
		margin: 0 0 10px;
		font: 600 44px/1.1 var(--serif);
		letter-spacing: -0.01em;
	}
	.deck {
		margin: 0 0 22px;
		max-width: 660px;
		font: 17px/1.5 var(--serif);
		color: var(--muted);
	}
	ul {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	li {
		border-top: 1px solid var(--line);
	}
	a {
		display: flex;
		flex-direction: column;
		gap: 3px;
		padding: 16px 0;
		color: inherit;
		text-decoration: none;
	}
	a:hover .name {
		color: var(--accent);
	}
	.top {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 12px;
	}
	.name {
		font: 600 22px/1.25 var(--serif);
		overflow-wrap: anywhere;
	}
	time {
		flex: none;
		font: 12px/1.6 var(--sans);
		color: var(--faint);
	}
	.topic {
		font: 16px/1.5 var(--serif);
		color: var(--muted);
	}
	.meta {
		font: 12.5px/1.5 var(--sans);
		color: var(--faint);
	}
	@media (max-width: 720px) {
		.index {
			padding: 20px 18px;
		}
		h1 {
			font-size: 32px;
		}
		.deck {
			font-size: 16px;
		}
	}
</style>
