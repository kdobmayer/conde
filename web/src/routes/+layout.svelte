<script lang="ts">
	import '../app.css';
	import { page } from '$app/state';
	import { start } from '#lib/board.svelte.ts';
	import Shell from '#lib/Shell.svelte';

	let { children } = $props();
	let failed = $state('');

	// Design-only option pages (routes/_design) render bare, on fixture data, with no session.
	const design = $derived(page.route.id?.startsWith('/_design') ?? false);

	$effect(() => {
		if (design) return;
		start().catch((e: Error) => (failed = e.message));
	});
</script>

{#if design}
	{@render children()}
{:else}
	<Shell {failed}>{@render children()}</Shell>
{/if}
