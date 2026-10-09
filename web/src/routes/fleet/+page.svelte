<script lang="ts">
	import { board, short } from '#lib/board.svelte.ts';
	import { ago } from '#lib/time.ts';
	import type { FleetAgent, FleetHost } from '#lib/types.ts';

	let filter = $state('');

	/** Online while the heartbeat is younger than the server's TTL, on the server's clock. */
	function online(h: FleetHost): boolean {
		if (!h.heartbeat || !board.fleet) return false;
		return Date.parse(h.heartbeat) + board.fleet.online_ttl_s * 1000 > board.now + board.skewMs;
	}

	const groups = $derived.by(() => {
		const fleet = board.fleet;
		if (!fleet) return [];
		const q = filter.trim().toLowerCase();
		const me = board.session?.user;
		const owners = new Map<string, { host: FleetHost; agents: FleetAgent[] }[]>();
		for (const h of fleet.hosts) {
			const agents = fleet.agents.filter(
				(a) => a.owner === h.owner && a.host === h.host && (!q || a.name.includes(q) || h.host.includes(q))
			);
			if (q && agents.length === 0) continue;
			if (!h.bridge && agents.length === 0) continue;
			const list = owners.get(h.owner) ?? [];
			list.push({ host: h, agents });
			owners.set(h.owner, list);
		}
		return [...owners.entries()].sort(([a], [b]) => (a === me ? -1 : b === me ? 1 : a.localeCompare(b)));
	});

	function dmHref(name: string): string {
		const [owner, rest] = name.split('/');
		return `#/dm/${encodeURIComponent(owner ?? '')}/${encodeURIComponent(rest ?? '')}`;
	}
</script>

<section class="fleet">
	<header class="head">
		<a class="back" href="#/" aria-label="All conversations">‹</a>
		<h1>Fleet</h1>
		<input type="search" placeholder="Filter" aria-label="Filter agents and hosts" bind:value={filter} />
	</header>
	<div class="body">
		<p class="muted legend">
			Online: the host's bridge answered within {board.fleet?.online_ttl_s ?? 90} s. Seen: the bridge reports the session
			live. Reached: the last notification delivered to it.
		</p>
		{#if !board.fleet}
			<p class="muted">Loading…</p>
		{/if}
		{#each groups as [owner, hosts] (owner)}
			<h2>{owner}</h2>
			{#each hosts as { host, agents } (host.host)}
				{@const up = online(host)}
				<div class="host">
					<div class="host-line">
						<span class="dot" class:up aria-hidden="true"></span>
						<strong>{host.host}</strong>
						{#if !host.bridge}
							<span class="muted">{host.host === 'web' ? 'browser' : 'no bridge yet'}</span>
						{:else if up}
							<span class="muted">online · heartbeat {ago(host.heartbeat, board.now + board.skewMs)}</span>
						{:else}
							<span class="muted">offline · last seen {ago(host.last_seen, board.now + board.skewMs)}</span>
						{/if}
					</div>
					{#if agents.length > 0}
						<ul>
							{#each agents as a (a.name)}
								<li>
									<a class="name" href={dmHref(a.name)}>{short(a.name)}</a>
									{#if a.display}<span class="muted">{a.display}</span>{/if}
									<span class="tag">{a.kind}</span>
									{#if a.pinned}<span class="tag" title={`pinned to ${a.pinned}`}>pinned</span>{/if}
									{#if a.kind !== 'human'}
										{#if a.discovered && up}
											<span class="tag ok" title={`reported ${ago(a.discovered, board.now + board.skewMs)}`}>seen</span>
										{:else}
											<span class="tag">not seen</span>
										{/if}
									{/if}
									{#if a.own && a.kind !== 'human'}
										<span class="muted reach">
											{a.last_reached ? `reached ${ago(a.last_reached, board.now + board.skewMs)}` : 'never reached'}
										</span>
									{/if}
									{#if a.held}<span class="tag warn">{a.held} held</span>{/if}
									{#if a.stuck}<span class="tag danger">{a.stuck} stuck</span>{/if}
								</li>
							{/each}
						</ul>
					{:else}
						<p class="muted none">No registered agents.</p>
					{/if}
				</div>
			{/each}
		{/each}
	</div>
</section>

<style>
	.fleet {
		display: flex;
		flex-direction: column;
		height: 100%;
	}
	.head {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 8px 16px;
		border-bottom: 1px solid var(--line);
	}
	h1 {
		font-size: 16px;
		margin: 0;
		flex: 1;
	}
	.head input {
		width: 180px;
	}
	.back {
		display: none;
		font-size: 24px;
		text-decoration: none;
		color: inherit;
	}
	.body {
		flex: 1;
		overflow-y: auto;
		padding: 8px 16px 24px;
	}
	.legend {
		font-size: 12.5px;
		margin: 4px 0 8px;
	}
	h2 {
		font-size: 13px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--muted);
		margin: 18px 0 6px;
	}
	.host {
		border: 1px solid var(--line);
		border-radius: 8px;
		padding: 8px 12px;
		margin-bottom: 8px;
	}
	.host-line {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px;
	}
	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--line);
		flex: none;
	}
	.dot.up {
		background: var(--ok);
	}
	ul {
		list-style: none;
		margin: 6px 0 0;
		padding: 0;
	}
	li {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px;
		padding: 4px 0;
		border-top: 1px solid var(--line);
		font-size: 14px;
	}
	.name {
		font-weight: 600;
		text-decoration: none;
		overflow-wrap: anywhere;
	}
	.reach {
		font-size: 12.5px;
	}
	.none {
		margin: 6px 0 0;
		font-size: 13px;
	}
	@media (max-width: 720px) {
		.back {
			display: block;
		}
		.head input {
			width: 120px;
		}
	}
</style>
