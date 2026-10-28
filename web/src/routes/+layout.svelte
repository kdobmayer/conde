<script lang="ts">
	import '../app.css';
	import { start } from '#lib/board.svelte.ts';
	import Shell from '#lib/Shell.svelte';

	let { children } = $props();
	let failed = $state('');

	$effect(() => {
		start().catch((e: Error) => (failed = e.message));
	});
</script>

<Shell {failed}>{@render children()}</Shell>
