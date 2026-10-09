<script lang="ts">
	import { page } from '$app/state';
	import Conversation from '#lib/Conversation.svelte';

	const target = $derived(`#${page.params.name}`);
</script>

{#key target}
	<Conversation {target} />
{/key}
