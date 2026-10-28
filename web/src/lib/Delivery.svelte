<script lang="ts">
	// A message's one delivery word; expanding it shows the per-recipient detail and the message
	// id with a copy button (ia.md "DM"). The id is never printed inline.
	import type { Message } from './types';
	import { board, short } from './board.svelte';
	import { deliveryWord } from './conversation';
	import { ago, clock, countdown } from './time';
	import Disclosure from './ui/Disclosure.svelte';
	import StateWord from './ui/StateWord.svelte';

	let { message }: { message: Message } = $props();
	const w = $derived(deliveryWord(message, board.now + board.skewMs));
	const recipients = $derived(message.delivery?.recipients ?? []);
	const readBy = $derived(message.delivery?.read_by ?? null);

	const stateLabel: Record<string, string> = {
		held: 'held for approval',
		gone: 'recipient gone'
	};

	let copied = $state('');
	let idEl: HTMLElement | undefined = $state();

	async function copy() {
		try {
			await navigator.clipboard.writeText(message.message_id);
			copied = 'Copied';
		} catch {
			// No clipboard (an insecure origin, a denied permission): select the id instead.
			const sel = getSelection();
			if (idEl && sel) sel.selectAllChildren(idEl);
			copied = 'Selected; press Ctrl+C';
		}
		setTimeout(() => (copied = ''), 2500);
	}
</script>

<div class="delivery">
	<Disclosure>
		{#snippet label()}
			{#if w}
				<span class="word"><StateWord {message} />{#if w.detail}{' '}<span class:bad={w.tone === 'problem'}>{w.detail}</span>{/if}</span>
			{:else}
				<span class="word">details</span>
			{/if}
		{/snippet}
		<ul>
			{#each recipients as r (r.recipient)}
				<li>
					to {short(r.recipient)}: <span class:bad={['retrying', 'held', 'denied', 'refused', 'gone'].includes(r.state)}>{stateLabel[r.state] ?? r.state}</span>
					{#if r.state === 'read' && r.read_at}at {clock(r.read_at)}, {ago(r.read_at, board.now)}{/if}
					{#if r.state === 'retrying' && r.retry_at}· next try {countdown(r.retry_at, board.now + board.skewMs)}{/if}
					{#if r.attempts && r.attempts > 1}· {r.attempts} attempts{/if}
					{#if r.last_error}· last error: {r.last_error.status}{r.last_error.reason ? ` (${r.last_error.reason})` : ''}{/if}
				</li>
			{/each}
			{#if readBy}<li>Read by {readBy.read} of {readBy.total} members.</li>{/if}
			<li class="id">
				Message id <code bind:this={idEl}>{message.message_id}</code>
				<button type="button" class="quiet" onclick={copy}>Copy</button>
				<span role="status">{copied}</span>
			</li>
		</ul>
	</Disclosure>
</div>

<style>
	/* The toggle and its region join the message's foot line; the region takes a row of its own. */
	.delivery,
	.delivery :global(.disclosure) {
		display: contents;
	}
	.delivery :global(.toggle) {
		width: auto;
		min-height: 28px;
		margin-left: -6px;
		padding: 0 6px;
		font: 12.5px/1.4 var(--sans);
		color: var(--faint);
	}
	.delivery :global(.region) {
		flex-basis: 100%;
		order: 1;
		padding: 2px 0 4px;
	}
	.word {
		color: var(--muted);
		white-space: nowrap;
	}
	.bad {
		color: var(--danger);
	}
	ul {
		list-style: none;
		margin: 0;
		padding: 8px 12px;
		border-left: 2px solid var(--line);
		font: 12.5px/1.6 var(--sans);
		color: var(--muted);
	}
	li {
		overflow-wrap: anywhere;
	}
	.id {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 4px 8px;
		margin-top: 4px;
	}
	.id button {
		min-height: 28px;
		padding: 0 10px;
		font-size: 12px;
	}
</style>
