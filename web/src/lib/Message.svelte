<script lang="ts">
	// One message as a line of a script (options/README.md "B"): speaker and time in the left
	// margin, the text in a reading measure, then one delivery word and the reactions. Reply
	// context only when the conversation passes it (the parent is not the message above); it
	// opens the thread.
	import type { Message } from './types';
	import { bytes, clock } from './time';
	import Delivery from './Delivery.svelte';
	import Markdown from './Markdown.svelte';

	let {
		message,
		speaker,
		mine = false,
		selected = false,
		parent = null
	}: {
		message: Message;
		speaker: (name: string) => string;
		mine?: boolean;
		/** The message the conversation was opened at. */
		selected?: boolean;
		/** The replied-to message's speaker and first line, or `earlier` when it is not loaded. */
		parent?: { from: string; line: string } | 'earlier' | null;
	} = $props();
</script>

<article id={`m-${message.message_id}`} class="msg" class:mine class:selected aria-current={selected ? 'true' : undefined} aria-label={`${speaker(message.from)}, ${clock(message.date)}`}>
	<div class="side">
		<b title={message.from}>{speaker(message.from)}</b>
		<time datetime={message.date} title={new Date(message.date).toLocaleString()}>{clock(message.date)}</time>
	</div>
	<div class="say">
		{#if parent}
			<a class="re" href={`#/t/${encodeURIComponent(message.message_id)}`}>
				{#if parent === 'earlier'}Replying to an earlier message{:else}Replying to {parent.from}: “{parent.line}”{/if}
			</a>
		{/if}
		{#if message.text}<div class="body"><Markdown text={message.text} /></div>{/if}
		{#each message.attachments as a (a.attachment_id)}
			<a class="att" href={`/v1/attachments/${encodeURIComponent(a.attachment_id)}`} download={a.filename}>
				<span class="file" aria-hidden="true"></span>{a.filename}<span class="size">{bytes(a.size)}</span>
			</a>
		{/each}
		<div class="foot">
			<Delivery {message} />
			{#each message.reactions as r (r.emoji)}
				<span class="react" title={r.by.join(', ')}>{r.emoji} {r.by.map(speaker).join(', ')}</span>
			{/each}
		</div>
	</div>
</article>

<style>
	.msg {
		display: grid;
		grid-template-columns: var(--margin, 200px) minmax(0, 660px);
		gap: 28px;
		padding: 14px 0;
		scroll-margin: 40px;
	}
	.selected {
		margin: 0 -18px;
		padding-right: 16px;
		padding-left: 16px;
		border-left: 2px solid var(--accent);
		background: var(--mine);
	}
	.side {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		min-width: 0;
		padding-top: 4px;
	}
	.side b {
		max-width: 100%;
		font: 600 11.5px/1.5 var(--sans);
		letter-spacing: 0.04em;
		text-transform: uppercase;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mine .side b {
		color: var(--accent);
	}
	time {
		font: 12px/1.6 var(--sans);
		color: var(--faint);
	}
	.body {
		font: 17px/1.6 var(--serif);
	}
	.re {
		display: block;
		margin-bottom: 4px;
		font: italic 13.5px/1.4 var(--serif);
		color: var(--faint);
		text-decoration: none;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.re:hover {
		color: var(--muted);
		text-decoration: underline;
	}
	.att {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		max-width: 100%;
		margin: 10px 10px 0 0;
		padding: 7px 12px;
		border: 1px solid var(--line);
		font-size: 13px;
		color: var(--fg);
		text-decoration: none;
		overflow-wrap: anywhere;
	}
	.size {
		color: var(--faint);
		white-space: nowrap;
	}
	.file {
		width: 10px;
		height: 13px;
		flex: none;
		border: 1.5px solid var(--muted);
	}
	.foot {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0 16px;
		margin-top: 4px;
	}
	.react {
		font: 12.5px/1.4 var(--sans);
		color: var(--faint);
	}
	@media (max-width: 720px) {
		.msg {
			grid-template-columns: minmax(0, 1fr);
			gap: 2px;
			padding: 12px 0;
		}
		.side {
			flex-direction: row;
			align-items: baseline;
			gap: 10px;
			padding-top: 0;
		}
		.body {
			font-size: 16px;
		}
	}
</style>
