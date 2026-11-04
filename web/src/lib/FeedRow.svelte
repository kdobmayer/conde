<script lang="ts">
	// B's Feed row (options/README.md "B"): a kicker (time, a direction word, sender → recipient
	// always in that order, place) above two lines of the text, then the one delivery word and
	// the reply marker. Own agents carry the accent; other owners' agents read owner/name.
	import type { FeedMessage } from './types';
	import { board, short } from './board.svelte';
	import { clock } from './time';
	import { DIRECTION_WORD } from './feed-view';
	import { plainText } from './markdown';
	import StateWord from './ui/StateWord.svelte';

	let {
		message: m,
		href,
		selected = false,
		chain = false,
		onopen
	}: { message: FeedMessage; href: string; selected?: boolean; chain?: boolean; onopen: (id: string) => void } = $props();

	const own = (name: string) => {
		const me = board.session?.user;
		return !!me && name.startsWith(`${me}/`);
	};
	const dm = $derived(m.place === 'dm');

	function click(e: MouseEvent) {
		// a new tab or window keeps the link's own deep link
		if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
		e.preventDefault();
		onopen(m.message_id);
	}
</script>

<a class="row" class:sel={selected} class:chain {href} data-id={m.message_id} aria-current={selected ? 'true' : undefined} onclick={click}>
	<span class="kick">
		<span class="when"><time datetime={m.date}>{clock(m.date)}</time> <span class="dir">{DIRECTION_WORD[m.direction]}</span></span>
		<span class="who"
			><b class:own={own(m.from)}>{short(m.from)}</b><svg class="arrow" viewBox="0 0 12 12" width="11" height="11" aria-hidden="true"
				><path d="M1.5 6h8.5M6.8 2.6 10.2 6 6.8 9.4" /></svg
			><span class="sr-only">{' to '}</span><b class:own={dm && own(m.to)}>{dm ? short(m.to) : m.to}</b></span
		>
		<span class="place" title={dm ? undefined : m.place}>{dm ? 'Direct' : m.place}</span>
	</span>
	<span class="text">{plainText(m.text)}</span>
	<span class="meta">
		<StateWord message={m} />
		{#if m.reply_to}<span>reply</span>{/if}
		{#if m.attachments.length}<span>{m.attachments.length === 1 ? '1 file' : `${m.attachments.length} files`}</span>{/if}
	</span>
</a>

<style>
	.row {
		display: block;
		padding: 14px 18px 14px 16px;
		margin: 0 -18px;
		border-left: 2px solid transparent;
		color: inherit;
		text-decoration: none;
	}
	.row:hover .text {
		color: var(--fg);
	}
	.row:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
	}
	.row.sel {
		background: var(--mine);
		border-left-color: var(--accent);
	}
	.row.chain {
		border-left-color: var(--line);
	}
	.kick {
		display: flex;
		align-items: baseline;
		gap: 14px;
		white-space: nowrap;
		font: 12px/1.4 var(--sans);
		color: var(--faint);
	}
	.when {
		display: contents;
	}
	.dir {
		flex: none;
	}
	.who {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		color: var(--muted);
	}
	.who b {
		font-weight: 600;
		color: var(--fg);
	}
	.who b.own {
		color: var(--accent);
	}
	.arrow {
		margin: 0 5px;
		vertical-align: -1px;
		fill: none;
		stroke: var(--muted);
		stroke-width: 1.3;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
	.place {
		margin-left: auto;
		padding-left: 8px;
	}
	.text {
		display: -webkit-box;
		margin: 5px 0 6px;
		max-width: 680px;
		overflow: hidden;
		font: 17px/1.5 var(--serif);
		color: var(--fg);
		overflow-wrap: anywhere;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}
	.meta {
		display: flex;
		gap: 14px;
		font: 12px/1.4 var(--sans);
		color: var(--faint);
	}
	.meta :global(.state) {
		color: var(--faint);
	}
	.meta :global(.state.problem) {
		color: var(--warn);
	}
	/* desktop rows sit closer than the phone's */
	@media (min-width: 721px) {
		.row {
			padding: 11px 18px 11px 16px;
		}
		.text {
			margin: 3px 0 4px;
		}
	}
	/* a wide column reads like the conversation's script: when and who in a margin (the time
	   over the direction word, the sender over the recipient), the text beside it in a long
	   measure, the place over the delivery at the far edge; a row is two lines tall */
	@container feed (min-width: 900px) {
		.row {
			display: grid;
			grid-template-columns: 84px clamp(270px, 19cqi, 300px) minmax(0, 840px) minmax(180px, 1fr);
			grid-template-rows: auto 1fr;
			column-gap: 0;
			align-items: start;
			padding: 9px 18px 9px 16px;
		}
		.kick {
			display: contents;
			line-height: 17px;
		}
		/* 5 px down: the 12 px lines' baseline on the text's first */
		.when {
			grid-column: 1;
			grid-row: 1 / 3;
			display: flex;
			flex-direction: column;
			margin-top: 5px;
		}
		.who {
			grid-column: 2;
			grid-row: 1 / 3;
			display: grid;
			grid-template-columns: auto minmax(0, 1fr);
			align-content: start;
			margin: 5px 28px 0 0;
		}
		.who b {
			overflow: hidden;
			text-overflow: ellipsis;
		}
		.who b:first-child {
			grid-column: 1 / 3;
		}
		.arrow {
			margin: 0 5px 0 0;
			align-self: center;
		}
		.text {
			grid-column: 3;
			grid-row: 1 / 3;
			margin: 0;
			max-width: none;
			font-size: 16px;
			line-height: 1.55;
		}
		/* the last column fits a long channel name and the delivery words; a longer name is cut
		   and the words wrap, so neither runs into the text */
		.place {
			grid-column: 4;
			grid-row: 1;
			justify-self: end;
			max-width: 100%;
			margin: 5px 0 0;
			padding: 0 0 0 20px;
			overflow: hidden;
			text-overflow: ellipsis;
		}
		.meta {
			grid-column: 4;
			grid-row: 2;
			justify-self: end;
			flex-wrap: wrap;
			justify-content: flex-end;
			gap: 0 10px;
			max-width: 100%;
			padding-left: 20px;
			line-height: 17px;
		}
	}
	@media (max-width: 720px) {
		.row {
			padding: 12px 18px 12px 16px;
		}
		.place {
			display: none;
		}
		.text {
			font-size: 16px;
		}
	}
</style>
