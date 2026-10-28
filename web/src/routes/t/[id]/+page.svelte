<script lang="ts">
	// A thread's deep link: the Feed with that thread open.
	import { page } from '$app/state';
	import { enterFeed } from '#lib/FeedPage.svelte';

	$effect(() => void enterFeed('', page.params.id ?? null));
</script>
