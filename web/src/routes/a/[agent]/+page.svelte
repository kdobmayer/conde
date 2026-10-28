<script lang="ts">
	// On a wide screen the agent page opens its most recent conversation beside the list; on the
	// phone the list (in the layout) is the page.
	import { page } from '$app/state';
	import { board } from '#lib/board.svelte.ts';
	import { latestOf } from '#lib/conversation.ts';
	import Conversation from '#lib/Conversation.svelte';

	const PHONE = '(max-width: 720px)';
	let phone = $state(matchMedia(PHONE).matches);
	$effect(() => {
		const q = matchMedia(PHONE);
		const on = () => (phone = q.matches);
		q.addEventListener('change', on);
		return () => q.removeEventListener('change', on);
	});

	const agent = $derived(`${board.session?.user}/${page.params.agent}`);
	const target = $derived.by(() => {
		const me = board.rail?.agents.find((a) => a.name === agent);
		return me ? latestOf(me) : null;
	});
</script>

{#if !phone && target}
	{#key `${agent} ${target}`}
		<Conversation {agent} {target} />
	{/key}
{/if}
