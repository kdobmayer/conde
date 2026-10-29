<script lang="ts">
	// A message body in the markdown subset of markdown.ts, sanitised. Type and size are the
	// parent's; the text keeps its spaces and line breaks as plain text did.
	import { renderMarkdown } from './markdown';

	let { text }: { text: string } = $props();
	const html = $derived(renderMarkdown(text));
</script>

<div class="md">{@html html}</div>

<style>
	.md {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.md :global(:is(p, ul, ol, blockquote, pre)) {
		margin: 0;
	}
	.md :global(:is(p, ul, ol, blockquote) + :is(p, ul, ol, blockquote)) {
		margin-top: 1lh;
	}
	/* a list right under its lead-in line stays with it */
	.md :global(p + :is(ul, ol)) {
		margin-top: 0.25lh;
	}
	.md :global(* + pre),
	.md :global(pre + *) {
		margin-top: 12px;
	}
	.md :global(:is(ul, ol)) {
		padding-left: 1.4em;
	}
	.md :global(li + li),
	.md :global(li > :is(p, ul, ol) + :is(p, ul, ol)) {
		margin-top: 0.2lh;
	}
	.md :global(li > :is(ul, ol)) {
		margin-top: 0;
	}
	.md :global(li::marker) {
		color: var(--muted);
	}
	.md :global(blockquote) {
		padding-left: 14px;
		border-left: 2px solid var(--line);
		color: var(--muted);
	}
	.md :global(strong) {
		font-weight: 600;
	}
	.md :global(a) {
		color: var(--accent);
		text-decoration: underline;
		text-underline-offset: 3px;
	}
	.md :global(:not(pre) > code) {
		font-size: 0.8em;
		white-space: pre-wrap;
	}
	.md :global(pre) {
		padding: 10px 12px;
		border-radius: 4px;
		background: var(--soft);
		overflow-x: auto;
		white-space: pre;
		overflow-wrap: normal;
	}
	.md :global(pre code) {
		padding: 0;
		background: none;
		font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
	}
</style>
