<script lang="ts">
	// The owner's agents (ia.md "Agents"): per agent its kind, host, online state, last reached,
	// held and stuck (from `fleet.list`).
	import { board, refreshFleet, refreshRail, short } from '#lib/board.svelte.ts';
	import { KIND, agentFacts } from '#lib/conversation.ts';
	import { ago } from '#lib/time.ts';
	import EmptyState from '#lib/ui/EmptyState.svelte';
	import ErrorLine from '#lib/ui/ErrorLine.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';

	const agents = $derived((board.rail?.agents ?? []).map((a) => ({ name: short(a.name), facts: agentFacts(board.fleet, a.name) })));
</script>

<section class="index">
	<h1>Agents</h1>
	<p class="deck">Your agents on the shared board. Visible here: their own DMs and their channels.</p>
	<!-- a failed read keeps the last good rows on screen, with its age -->
	{#if board.error}
		<ErrorLine error={board.error} asOf={board.rail ? board.railAt : null} retry={refreshRail} />
	{/if}
	{#if board.fleetError}
		<ErrorLine error={board.fleetError} asOf={board.fleet ? board.fleetAt : null} retry={refreshFleet} />
	{/if}
	{#if !board.rail}
		{#if !board.error}<SkeletonRows count={4} height={80} />{/if}
	{:else if agents.length === 0}
		<EmptyState text="You have no agents on the shared board yet. Agents you publish from your machines appear here." />
	{:else}
		<ul>
			{#each agents as a (a.name)}
				<li>
					<a href={`#/a/${encodeURIComponent(a.name)}`}>
						<span class="top">
							<span class="name"><i class:online={a.facts?.online} aria-hidden="true"></i>{a.name}</span>
							{#if a.facts?.held}<b>{a.facts.held} held</b>{/if}
							{#if a.facts?.stuck}<b>{a.facts.stuck} stuck</b>{/if}
						</span>
						<span class="meta">
							{#if a.facts}
								{KIND[a.facts.kind] ?? a.facts.kind} on {a.facts.host} · {a.facts.online ? 'online' : 'offline'} · last reached {ago(a.facts.last_reached, board.now)}
							{:else if board.fleet && !board.fleetError}
								Not on any host yet
							{:else if board.fleetError}
								Host not known: the fleet could not be read
							{/if}
						</span>
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
		align-items: baseline;
		gap: 12px;
	}
	.name {
		display: flex;
		align-items: center;
		gap: 10px;
		min-width: 0;
		font: 600 22px/1.25 var(--serif);
		overflow-wrap: anywhere;
	}
	b {
		font: 600 12.5px/1.4 var(--sans);
		color: var(--accent);
		white-space: nowrap;
	}
	.top b:first-of-type {
		margin-left: auto;
	}
	i {
		display: inline-block;
		width: 8px;
		height: 8px;
		flex: none;
		border-radius: 50%;
		border: 1px solid var(--faint);
	}
	i.online {
		background: var(--muted);
		border-color: var(--muted);
	}
	.meta {
		padding-left: 18px;
		font: 13px/1.5 var(--sans);
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
