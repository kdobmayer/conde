<script lang="ts">
	import type { Message } from './types';
	import { board, short } from './board.svelte';
	import { ago, countdown } from './time';

	let { message }: { message: Message } = $props();
	const delivery = $derived(message.delivery);
	/** The viewer's own notification says nothing the rail does not: left out. */
	const recipients = $derived((delivery?.recipients ?? []).filter((r) => r.recipient !== board.session?.name));

	const label: Record<string, string> = {
		queued: 'queued',
		sending: 'sending',
		retrying: 'retrying',
		notified: 'notified',
		held: 'held for approval',
		delivered: 'delivered',
		read: 'read',
		unread: 'unread',
		denied: 'denied',
		refused: 'refused',
		gone: 'recipient gone',
		sent: 'sent'
	};
	const tone: Record<string, string> = {
		read: 'ok',
		delivered: 'ok',
		held: 'warn',
		retrying: 'warn',
		denied: 'danger',
		refused: 'danger',
		gone: 'danger'
	};
</script>

{#if delivery && (recipients.length > 0 || delivery.read_by)}
	<ul class="delivery">
		{#each recipients as r (r.recipient)}
			<li>
				<span class="who">{short(r.recipient)}</span>
				<span class="tag {tone[r.state] ?? ''}">{label[r.state] ?? r.state}</span>
				{#if r.state === 'read' && r.read_at}<span class="muted">{ago(r.read_at, board.now)}</span>{/if}
				{#if r.retry_at && r.state === 'retrying'}
					<span class="muted">next try {countdown(r.retry_at, board.now + board.skewMs)}</span>
				{/if}
				{#if r.attempts && r.attempts > 1}<span class="muted">{r.attempts} attempts</span>{/if}
				{#if r.last_error}
					<span class="err">last error: {r.last_error.status}{r.last_error.reason ? ` (${r.last_error.reason})` : ''}</span>
				{/if}
			</li>
		{/each}
		{#if delivery.read_by}
			<li class="muted">read by {delivery.read_by.read} of {delivery.read_by.total} members</li>
		{/if}
	</ul>
{/if}

<style>
	.delivery {
		list-style: none;
		margin: 4px 0 0;
		padding: 0;
		font-size: 12.5px;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	li {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px;
	}
	.who {
		color: var(--muted);
	}
	.who::before {
		content: 'to ';
	}
	.err {
		color: var(--danger);
		overflow-wrap: anywhere;
	}
</style>
