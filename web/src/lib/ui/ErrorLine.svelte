<script lang="ts">
	// A failed call, inline where the data would be: what failed, Retry, and, while the last good
	// data stays on screen, the time it is from.
	let { error, asOf = null, retry }: { error: string; asOf?: number | null; retry: () => void } = $props();

	const hhmm = (ms: number) => new Date(ms).toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit' });
</script>

<p class="error-line" role="alert">
	<span class="what">Could not load: {error}.</span>
	{#if asOf !== null}<span class="muted">Showing data as of {hhmm(asOf)}.</span>{/if}
	<button type="button" class="quiet" onclick={retry}>Retry</button>
</p>

<style>
	.error-line {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 4px 10px;
		margin: 0;
		padding: 8px 12px;
		border-left: 2px solid var(--danger);
		background: var(--panel);
	}
	.what {
		color: var(--danger);
	}
	button {
		padding: 2px 10px;
	}
</style>
