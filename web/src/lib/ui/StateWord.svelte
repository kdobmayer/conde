<script lang="ts">
	// A message's one delivery word in its tone; problems are the only coloured words.
	import type { Message } from '../types';
	import { stateWord } from './state';

	let { message }: { message: Pick<Message, 'place' | 'delivery'> } = $props();
	const w = $derived(stateWord(message));
</script>

{#if w}<span class="state {w.tone}">{w.word}</span>{/if}

<style>
	.state {
		white-space: nowrap;
		color: var(--muted);
	}
	.pending {
		font-style: italic;
	}
	.problem {
		color: var(--danger);
	}
</style>
