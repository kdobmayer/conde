<script lang="ts">
	// A channel's history, read through the first own agent that is a member (ia.md "Channels"):
	// the result is the same through any of them. Opened at a message on `#/c/<name>/m/<id>`.
	import { page } from '$app/state';
	import { rpc } from '#lib/api.ts';
	import { board } from '#lib/board.svelte.ts';
	import { channelAgent } from '#lib/conversation.ts';
	import Conversation from '#lib/Conversation.svelte';
	import EmptyState from '#lib/ui/EmptyState.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';
	import type { ChannelSummary } from '#lib/types.ts';

	const at = $derived(page.params.id ?? null);
	const channel = $derived(`#${page.params.name}`);
	const agent = $derived(channelAgent(board.rail, channel));
	let members = $state<number | null>(null);

	$effect(() => {
		const asked = channel;
		members = null;
		rpc<{ channels: ChannelSummary[] }>('channel.list')
			.then((r) => {
				if (asked === channel) members = r.channels.find((c) => c.channel === asked)?.members ?? null;
			})
			.catch(() => {}); // the count is a nicety; the page stands without it
	});
</script>

{#if !board.rail}
	<SkeletonRows count={6} height={72} />
{:else if !agent}
	<EmptyState text={`None of your agents is in ${channel}. A channel shows here while one of your agents is a member.`} />
{:else}
	{#key `${agent} ${channel} ${at}`}
		<Conversation {agent} target={channel} {members} {at} back={{ href: '#/channels', label: 'Channels' }} />
	{/key}
{/if}
