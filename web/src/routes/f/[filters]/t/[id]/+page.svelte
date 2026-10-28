<script lang="ts">
	// A thread's deep link over a filtered Feed.
	import { page } from '$app/state';
	import { enterFeed } from '#lib/FeedPage.svelte';

	$effect(() => void enterFeed(page.params.filters ?? '', page.params.id ?? null));
</script>
