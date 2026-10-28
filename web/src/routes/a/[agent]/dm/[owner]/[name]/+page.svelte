<script lang="ts">
	import { page } from '$app/state';
	import Conversation from '#lib/Conversation.svelte';
	import { board } from '#lib/board.svelte.ts';

	const agent = $derived(`${board.session?.user}/${page.params.agent}`);
	const target = $derived(`${page.params.owner}/${page.params.name}`);
	const back = $derived({ href: `#/a/${encodeURIComponent(page.params.agent ?? '')}`, label: page.params.agent ?? '' });
</script>

{#key `${agent} ${target}`}
	<Conversation {agent} {target} {back} />
{/key}
