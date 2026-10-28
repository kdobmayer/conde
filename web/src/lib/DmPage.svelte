<script lang="ts">
	// A DM of an own agent; on `.../m/<id>` opened at that message instead of the newest.
	import { page } from '$app/state';
	import Conversation from '#lib/Conversation.svelte';
	import { board } from '#lib/board.svelte.ts';

	const agent = $derived(`${board.session?.user}/${page.params.agent}`);
	const target = $derived(`${page.params.owner}/${page.params.name}`);
	const at = $derived(page.params.id ?? null);
	const back = $derived({ href: `#/a/${encodeURIComponent(page.params.agent ?? '')}`, label: page.params.agent ?? '' });
</script>

{#key `${agent} ${target} ${at}`}
	<Conversation {agent} {target} {back} {at} />
{/key}
