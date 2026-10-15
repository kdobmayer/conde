<script lang="ts">
	// A polite live region: screen readers read `message` when it changes ("2 new messages").
	let { message }: { message: string } = $props();
</script>

<div class="sr-only" aria-live="polite" aria-atomic="true">{message}</div>
