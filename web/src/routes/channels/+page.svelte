<script lang="ts">
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { rpc } from '#lib/api.ts';
	import { board, changed } from '#lib/board.svelte.ts';
	import type { ChannelSummary } from '#lib/types.ts';

	let list = $state<ChannelSummary[]>([]);
	let error = $state('');
	let name = $state('');
	let topic = $state('');
	const mine = $derived(new Set(board.rail?.channels.map((c) => c.channel)));

	async function load() {
		try {
			list = (await rpc<{ channels: ChannelSummary[] }>('channel.list')).channels;
		} catch (e) {
			error = (e as Error).message;
		}
	}
	onMount(load);

	function open(channel: string) {
		void goto(`#/channel/${encodeURIComponent(channel.slice(1))}`);
	}

	async function join(channel: string) {
		try {
			await rpc('channel.join', { channel });
			changed(['messages']);
			open(channel);
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function create(e: SubmitEvent) {
		e.preventDefault();
		const channel = '#' + name.trim().replace(/^#/, '');
		try {
			await rpc('channel.create', topic.trim() ? { channel, topic: topic.trim() } : { channel });
			changed(['messages']);
			open(channel);
		} catch (err) {
			error = (err as Error).message;
		}
	}
</script>

<section class="page">
	<header class="head">
		<a class="back" href="#/" aria-label="All conversations">‹</a>
		<h1>Channels</h1>
	</header>
	<div class="body">
		{#if error}<p class="error">{error}</p>{/if}
		<table>
			<thead><tr><th>Channel</th><th>Topic</th><th>Members</th><th></th></tr></thead>
			<tbody>
				{#each list as c (c.channel)}
					<tr>
						<td>{c.channel}</td>
						<td class="muted topic">{c.topic ?? ''}</td>
						<td class="muted">{c.members}</td>
						<td>
							{#if mine.has(c.channel)}
								<button class="quiet" onclick={() => open(c.channel)}>Open</button>
							{:else}
								<button onclick={() => join(c.channel)}>Join</button>
							{/if}
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
		<h2>New channel</h2>
		<form onsubmit={create}>
			<input type="text" placeholder="#name" bind:value={name} required />
			<input type="text" placeholder="Topic (optional)" bind:value={topic} />
			<button disabled={!name.trim()}>Create</button>
		</form>
	</div>
</section>

<style>
	.page {
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
	h2 {
		font-size: 14px;
		margin: 20px 0 8px;
	}
	.back {
		display: none;
		font-size: 24px;
		text-decoration: none;
		color: inherit;
	}
	.body {
		flex: 1;
		overflow: auto;
		padding: 12px 16px;
	}
	table {
		width: 100%;
		border-collapse: collapse;
	}
	th,
	td {
		text-align: left;
		padding: 6px;
		border-bottom: 1px solid var(--line);
	}
	th {
		font-weight: 500;
		font-size: 13px;
		color: var(--muted);
	}
	.topic {
		max-width: 320px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	form {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		max-width: 560px;
	}
	form input {
		flex: 1 1 160px;
	}
	@media (max-width: 720px) {
		.back {
			display: block;
		}
		th:nth-child(2),
		td:nth-child(2) {
			display: none;
		}
	}
</style>
