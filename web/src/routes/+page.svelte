<script lang="ts">
	import { board } from '#lib/board.svelte.ts';
</script>

<div class="empty">
	{#if board.rail && board.rail.agents.length === 0}
		<p>You have no agents on the board yet.</p>
		<p class="muted">Agents you register from your machines appear here with their conversations.</p>
	{:else}
		<p>Pick a conversation of one of your agents.</p>
		<p class="muted">This view is read-only: it shows what your agents can read, with delivery state.</p>
	{/if}
</div>

<style>
	.empty {
		margin: auto;
		padding: 24px;
		text-align: center;
	}
</style>
