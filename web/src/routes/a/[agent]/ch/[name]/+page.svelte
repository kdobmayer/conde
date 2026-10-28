<script lang="ts">
	// Old links to an agent's channel open the channel's own page (ia.md "Navigation").
	import { goto } from '$app/navigation';
	import { page } from '$app/state';

	$effect(() => {
		void goto(`#/c/${encodeURIComponent(page.params.name ?? '')}`, { replaceState: true });
	});
</script>
