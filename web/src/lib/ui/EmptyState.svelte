<script lang="ts">
	// What a page says when a complete answer has nothing in it (ia.md "Loading, empty, error and
	// connection states"); never shown for a failed call.
	import type { Snippet } from 'svelte';

	let { text, children }: { text: string; children?: Snippet } = $props();
</script>

<div class="empty">
	<p>{text}</p>
	{#if children}{@render children()}{/if}
</div>

<style>
	.empty {
		padding: 32px 16px;
		color: var(--muted);
		text-align: center;
	}
	p {
		margin: 0 auto 8px;
		max-width: 46ch;
	}
</style>
