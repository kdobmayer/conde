<script lang="ts">
	// A button that shows and hides the region below it (`aria-expanded`, `aria-controls`).
	import type { Snippet } from 'svelte';

	let {
		label,
		open = $bindable(false),
		children
	}: { label: Snippet | string; open?: boolean; children: Snippet } = $props();
	const id = $props.id();
</script>

<div class="disclosure">
	<button type="button" class="toggle" aria-expanded={open} aria-controls="{id}-region" onclick={() => (open = !open)}>
		<span class="mark" aria-hidden="true">{open ? '▾' : '▸'}</span>
		{#if typeof label === 'string'}{label}{:else}{@render label()}{/if}
	</button>
	<div id="{id}-region" class="region" hidden={!open}>{@render children()}</div>
</div>

<style>
	.toggle {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		min-height: 32px;
		padding: 4px 8px;
		border: none;
		border-radius: 4px;
		background: none;
		color: var(--fg);
		text-align: left;
	}
	.toggle:hover {
		background: var(--soft);
	}
	.mark {
		width: 1em;
		color: var(--muted);
	}
	.region {
		padding: 4px 8px 8px 28px;
	}
</style>
