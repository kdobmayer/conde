<script lang="ts">
	import { board, unreadTotal } from '#lib/board.svelte.ts';
	const total = $derived(unreadTotal());
</script>

<div class="empty">
	{#if board.session?.problem}
		<p class="error">{board.session.problem}</p>
	{:else}
		<p>{total > 0 ? `${total} unread` : 'Nothing unread.'}</p>
		<p class="muted">Pick a conversation, or <a href="#/new">write a new message</a>.</p>
	{/if}
</div>

<style>
	.empty {
		margin: auto;
		padding: 24px;
		text-align: center;
	}
</style>
