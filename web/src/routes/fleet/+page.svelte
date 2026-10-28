<script lang="ts">
	// Fleet (ia.md "Fleet"): own hosts first, problems first; teammates collapsed per owner, with
	// name, kind and seen only. Held and stuck are the only coloured items.
	import { onMount } from 'svelte';
	import { board, refreshFleet, short } from '#lib/board.svelte.ts';
	import { orderFleet, type OwnAgent } from '#lib/fleet-order.ts';
	import { ago } from '#lib/time.ts';
	import Disclosure from '#lib/ui/Disclosure.svelte';
	import EmptyState from '#lib/ui/EmptyState.svelte';
	import ErrorLine from '#lib/ui/ErrorLine.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';

	// Heartbeats announce nothing: while this page is open it refetches, so a host stays online
	// as long as its bridge answers pings.
	onMount(() => {
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible') void refreshFleet();
		}, 30_000);
		return () => clearInterval(timer);
	});

	const now = $derived(board.now + board.skewMs);
	const view = $derived(board.fleet && board.session ? orderFleet(board.fleet, board.session.user, now) : null);
	const ttl = $derived(board.fleet?.online_ttl_s ?? 90);
	const asOf = $derived(board.fleet ? Date.parse(board.fleet.now) : null);
	// The column headers' one-line explanations (under 720 px a key above the table says them).
	const cols = $derived([
		{ label: 'Host, agent', x: `online: heartbeat within ${ttl} s` },
		{ label: 'Seen', x: 'its bridge reports the session' },
		{ label: 'Last reached', x: 'last delivery to it' },
		{ label: 'Held', x: 'awaiting approval' },
		{ label: 'Stuck', x: 'undelivered over 1 min' }
	]);

	function reached(a: OwnAgent): string {
		if (a.last_reached) return ago(a.last_reached, now);
		return a.ever_reached ? 'time not recorded' : 'never';
	}
	const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;
</script>

<section class="fleet">
	<header class="head">
		<h1>Fleet</h1>
		<p class="deck">Which agents exist on which hosts of the shared board, yours first. No message content.</p>
	</header>

	<div class="body">
		{#if !view}
			{#if board.fleetError}
				<ErrorLine error={board.fleetError} retry={() => void refreshFleet()} />
			{:else}
				<SkeletonRows count={6} height={40} />
			{/if}
		{:else}
			{#if board.fleetError}
				<ErrorLine error={board.fleetError} asOf={asOf === null ? null : asOf - board.skewMs} retry={() => void refreshFleet()} />
			{/if}

			<h2><span>Your hosts</span></h2>
			{#if view.own.length === 0}
				<EmptyState text="You have no agents on the shared board yet. Agents you publish from your machines appear here." />
			{:else}
				<dl class="key">
					{#each cols as c (c.label)}<div><dt>{c.label}</dt><dd>{c.x}</dd></div>{/each}
				</dl>
				<table class="grid">
					<colgroup><col /><col class="c-seen" /><col class="c-reach" /><col class="c-num" /><col class="c-num" /></colgroup>
					<thead>
						<tr>
							{#each cols as c, i (c.label)}
								<th scope="col" class:num={i > 2}>{c.label}<span class="x">{c.x}</span></th>
							{/each}
						</tr>
					</thead>
					{#each view.own as h (h.host)}
						<tbody>
							<tr class="host">
								<th scope="colgroup" colspan="5">
									<i class:on={h.online} aria-hidden="true"></i>
									<span class="hname">{h.host}</span>
									<span class="hstate">
										{#if !h.bridge}
											no bridge yet
										{:else if h.online}
											online · heartbeat {ago(h.heartbeat, now)}
										{:else}
											offline · last seen {ago(h.last_seen, now)}
										{/if}
									</span>
								</th>
							</tr>
							{#each h.agents as a (a.name)}
								<tr>
									<th scope="row"><span class="aname">{short(a.name)}</span><span class="kind">{a.kind}</span></th>
									<td class:faint={!a.seen}>{a.kind === 'human' ? '' : a.seen ? 'seen' : 'not seen'}</td>
									<td class:faint={!a.last_reached}>{a.kind === 'human' ? '' : reached(a)}</td>
									<td class="num" class:hot={a.held > 0}>{a.held}</td>
									<td class="num" class:hot={a.stuck > 0}>{a.stuck}</td>
								</tr>
							{:else}
								<tr><td class="none" colspan="5">No registered agents.</td></tr>
							{/each}
						</tbody>
					{/each}
				</table>
			{/if}

			{#if view.teammates.length > 0}
				<h2><span>Teammates</span></h2>
				<p class="note">Their hosts and agents only: the board shares no address or delivery health of another owner’s agents.</p>
				{#each view.teammates as t (t.owner)}
					{@const agents = t.hosts.reduce((n, h) => n + h.agents.length, 0)}
					{#snippet label()}
						<span class="owner">{t.owner}</span>
						<span class="count">{plural(t.hosts.length, 'host')} · {plural(agents, 'agent')}</span>
					{/snippet}
					<Disclosure {label}>
						<table class="grid team">
							<colgroup><col class="c-team" /><col class="c-seen" /></colgroup>
							<thead class="sr-only">
								<tr><th scope="col">Host, agent</th><th scope="col">Seen</th></tr>
							</thead>
							{#each t.hosts as h (h.host)}
								<tbody>
									<tr class="host">
										<th scope="colgroup" colspan="2">
											<i class:on={h.online} aria-hidden="true"></i>
											<span class="hname">{h.host}</span>
											<span class="hstate">{!h.bridge ? 'no bridge yet' : h.online ? 'online' : 'offline'}</span>
										</th>
									</tr>
									{#each h.agents as a (a.name)}
										<tr>
											<th scope="row"><span class="aname">{a.name.startsWith(`${t.owner}/`) ? a.name.slice(t.owner.length + 1) : a.name}</span><span class="kind">{a.kind}</span></th>
											<td class:faint={!a.seen}>{a.kind === 'human' ? '' : a.seen ? 'seen' : 'not seen'}</td>
										</tr>
									{:else}
										<tr><td class="none" colspan="2">No registered agents.</td></tr>
									{/each}
								</tbody>
							{/each}
						</table>
					</Disclosure>
				{/each}
			{/if}
		{/if}
	</div>
</section>

<style>
	.head {
		padding: 34px 48px 18px;
		border-bottom: 1px solid var(--line);
	}
	h1 {
		margin: 0;
		font: 600 44px/1 var(--serif);
		letter-spacing: -0.015em;
	}
	.deck {
		margin: 10px 0 4px;
		max-width: 560px;
		font: 17px/1.45 var(--serif);
		color: var(--muted);
	}
	.body {
		padding: 6px 48px 40px;
		max-width: 1000px;
	}
	h2 {
		display: flex;
		align-items: center;
		gap: 14px;
		margin: 26px 0 8px;
		font: italic 400 17px/1.3 var(--serif);
		color: var(--muted);
	}
	h2::after {
		content: '';
		flex: 1;
		border-top: 1px solid var(--line);
	}
	.note {
		margin: 0 0 8px;
		font-size: 13px;
		color: var(--faint);
	}
	.grid {
		width: 100%;
		border-collapse: collapse;
		table-layout: fixed;
		font-size: 14px;
	}
	.c-seen {
		width: 210px;
	}
	.c-reach {
		width: 160px;
	}
	.c-num {
		width: 150px;
	}
	.key {
		display: none;
	}
	thead th {
		padding: 6px 12px 10px 0;
		text-align: left;
		vertical-align: top;
		font-size: 11px;
		font-weight: 500;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--muted);
		border-bottom: 1px solid var(--line);
	}
	.x {
		display: block;
		white-space: nowrap;
		margin-top: 2px;
		font-size: 12px;
		letter-spacing: 0;
		text-transform: none;
		font-weight: 400;
		color: var(--faint);
	}
	.num {
		text-align: right;
		padding-right: 0;
	}
	thead th.num {
		padding-right: 0;
	}
	tbody th,
	tbody td {
		padding: 7px 12px 7px 0;
		text-align: left;
		font-weight: 400;
		color: var(--muted);
		vertical-align: baseline;
	}
	tbody td.num {
		text-align: right;
		padding-right: 0;
		color: var(--faint);
		font-variant-numeric: tabular-nums;
	}
	tr.host th {
		padding: 18px 0 6px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.grid tbody:first-of-type tr.host th {
		padding-top: 14px;
	}
	.hname {
		font: 600 19px/1.2 var(--serif);
		color: var(--fg);
		margin: 0 10px 0 6px;
	}
	.hstate {
		font-size: 12.5px;
		color: var(--faint);
	}
	i {
		display: inline-block;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		border: 1px solid var(--faint);
		vertical-align: 2px;
	}
	i.on {
		background: var(--muted);
		border-color: var(--muted);
	}
	tbody tr:not(.host) > * {
		border-top: 1px solid var(--line);
	}
	tbody tr.host + tr > * {
		border-top: none;
	}
	tbody th[scope='row'] {
		padding-left: 19px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.aname {
		color: var(--fg);
		font-weight: 500;
	}
	.kind {
		margin-left: 10px;
		font-size: 12px;
		color: var(--faint);
	}
	.faint {
		color: var(--faint) !important;
	}
	td.hot {
		color: var(--accent) !important;
		font-weight: 600;
	}
	.none {
		padding-left: 19px;
		color: var(--faint);
	}
	.owner {
		font: 600 17px/1.3 var(--serif);
	}
	.count {
		margin-left: 10px;
		font-size: 12.5px;
		color: var(--faint);
	}
	.grid.team {
		width: auto;
	}
	.c-team {
		width: 340px;
	}
	.team tr.host th {
		padding-top: 10px;
	}
	.team tbody:first-of-type tr.host th {
		padding-top: 4px;
	}
	.fleet :global(.disclosure) {
		border-bottom: 1px solid var(--line);
	}
	.fleet :global(.disclosure .toggle) {
		padding: 10px 4px;
	}
	@media (max-width: 1100px) {
		.head,
		.body {
			padding-left: 36px;
			padding-right: 36px;
		}
	}
	@media (max-width: 720px) {
		.head {
			padding: 18px 18px 12px;
		}
		h1 {
			font-size: 30px;
		}
		.deck {
			font-size: 14px;
			margin: 6px 0 2px;
		}
		.body {
			padding: 0 18px 30px;
		}
		.grid {
			font-size: 13px;
		}
		.c-seen {
			width: 64px;
		}
		.c-reach {
			width: 76px;
		}
		.c-num {
			width: 40px;
		}
		.c-team {
			width: 220px;
		}
		thead th {
			font-size: 10px;
			letter-spacing: 0.04em;
			padding-right: 6px;
			vertical-align: bottom;
		}
		.x {
			display: none;
		}
		.key {
			display: grid;
			gap: 1px;
			margin: 0 0 8px;
			font-size: 12px;
			color: var(--faint);
		}
		.key div {
			display: flex;
			gap: 8px;
		}
		.key dt {
			flex: none;
			width: 84px;
			color: var(--muted);
		}
		.key dd {
			margin: 0;
		}
		tbody th,
		tbody td {
			padding-right: 6px;
		}
		tbody th[scope='row'] {
			padding-left: 0;
			white-space: normal;
		}
		.kind {
			display: block;
			margin: 0;
			font-size: 11px;
		}
		.none {
			padding-left: 0;
		}
	}
</style>
