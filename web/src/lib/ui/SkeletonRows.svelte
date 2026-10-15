<script lang="ts">
	// Loading: rows of the final size in a flat tone, so nothing moves when data lands. Nothing
	// here animates.
	let { count = 6, height = 22 }: { count?: number; height?: number } = $props();
</script>

<div class="skeleton" role="status">
	<span class="sr-only">Loading…</span>
	{#each { length: count }, i (i)}
		<div class="row" style:height="{height}px" aria-hidden="true">
			<span style:width="{55 + ((i * 37) % 40)}%"></span>
		</div>
	{/each}
</div>

<style>
	.row {
		display: flex;
		align-items: center;
		padding: 0 12px;
		border-bottom: 1px solid var(--line);
	}
	.row span {
		height: 40%;
		border-radius: 3px;
		background: var(--soft);
	}
</style>
