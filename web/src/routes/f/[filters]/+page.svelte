<script lang="ts">
	// A filtered Feed's link (`#/f/agent=desk;dir=out`): the Feed with those filters.
	import { page } from '$app/state';
	import { enterFeed } from '#lib/FeedPage.svelte';

	$effect(() => void enterFeed(page.params.filters ?? '', null));
</script>
