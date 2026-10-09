<script lang="ts">
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { rpc } from '#lib/api.ts';
	import { board, short } from '#lib/board.svelte.ts';
	import SendBox from '#lib/SendBox.svelte';
	import type { ChannelSummary } from '#lib/types.ts';

	let query = $state('');
	let chosen = $state('');
	let channels = $state<string[]>([]);

	onMount(async () => {
		try {
			const r = await rpc<{ channels: ChannelSummary[] }>('channel.list');
			const mine = new Set(board.rail?.channels.map((c) => c.channel));
			channels = r.channels.map((c) => c.channel).filter((c) => mine.has(c));
		} catch {
			channels = [];
		}
	});

	const matches = $derived.by(() => {
		const q = query.trim().toLowerCase().replace(/^@/, '');
		const agents = (board.fleet?.agents ?? [])
			.filter((a) => a.name !== board.session?.name)
			.map((a) => ({ value: a.name, label: short(a.name), detail: `${a.kind} on ${a.host}` }));
		const chans = channels.map((c) => ({ value: c, label: c, detail: 'channel' }));
		return [...agents, ...chans].filter((o) => !q || o.label.toLowerCase().includes(q)).slice(0, 12);
	});

	function sent(_id: string, to: string) {
		if (to.startsWith('#')) void goto(`#/channel/${encodeURIComponent(to.slice(1))}`);
		else {
			const [owner, name] = to.split('/');
			void goto(`#/dm/${encodeURIComponent(owner ?? '')}/${encodeURIComponent(name ?? '')}`);
		}
	}
</script>

<section class="new">
	<header class="head">
		<a class="back" href="#/" aria-label="All conversations">‹</a>
		<h1>New message</h1>
	</header>
	<div class="body">
		{#if chosen}
			<p>To <strong>{chosen.startsWith('#') ? chosen : short(chosen)}</strong> <button class="link" onclick={() => (chosen = '')}>change</button></p>
		{:else}
			<label for="to">To</label>
			<input id="to" type="search" autocomplete="off" placeholder="An agent or #channel" bind:value={query} />
			<ul class="options">
				{#each matches as o (o.value)}
					<li><button class="quiet" onclick={() => (chosen = o.value)}><span>{o.label}</span><span class="muted">{o.detail}</span></button></li>
				{:else}
					<li class="muted">No match.</li>
				{/each}
			</ul>
		{/if}
	</div>
	{#if chosen}
		<SendBox to={chosen} onsent={sent} />
	{/if}
</section>

<style>
	.new {
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
		padding: 12px 16px;
	}
	label {
		display: block;
		font-size: 13px;
		color: var(--muted);
		margin-bottom: 4px;
	}
	input {
		width: 100%;
		max-width: 420px;
	}
	.options {
		list-style: none;
		padding: 0;
		margin: 8px 0;
		max-width: 420px;
	}
	.options button {
		width: 100%;
		display: flex;
		justify-content: space-between;
		gap: 8px;
		border: none;
		text-align: left;
		padding: 6px 8px;
	}
	.options button:hover {
		background: var(--soft);
	}
	@media (max-width: 720px) {
		.back {
			display: block;
		}
	}
</style>
