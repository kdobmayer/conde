<script lang="ts" module>
	import type { Message } from './types';

	/** "Thread in #channel" or "Thread in a DM", from the selected message once it is loaded. */
	export function threadTitle(messages: Message[], id: string | null): string {
		const m = messages.find((x) => x.message_id === id);
		if (!m) return 'Thread';
		return m.place === 'dm' ? 'Thread in a DM' : `Thread in ${m.place}`;
	}
</script>

<script lang="ts">
	// The thread view (ia.md "Thread view"): the segment around the selected message as the
	// server answered it, in tree order; a header computed from the returned messages only
	// (place, count, participants, time span, the link to the whole conversation); each message
	// in full with its attachments, reactions and its delivery (Delivery.svelte, as in the
	// conversation; the selected message's detail starts open). Beside the Feed it has its own kicker and Close; in the sheet the sheet has them.
	import { tick } from 'svelte';
	import type { ThreadView } from './thread-view.svelte';
	import { treeOrder } from './thread-tree';
	import { board, short } from './board.svelte';
	import { bytes, clock } from './time';
	import { headline, timeSpan } from './feed-view';
	import Delivery from './Delivery.svelte';
	import Markdown from './Markdown.svelte';
	import { plainText } from './markdown';
	import ErrorLine from './ui/ErrorLine.svelte';
	import SkeletonRows from './ui/SkeletonRows.svelte';

	let {
		view,
		selected,
		inSheet = false,
		link,
		onopen,
		onclose
	}: {
		view: ThreadView;
		selected: string;
		inSheet?: boolean;
		link: (id: string) => string;
		onopen: (id: string) => void;
		onclose?: () => void;
	} = $props();

	const s = $derived(view.state);
	const lines = $derived(s.top ? treeOrder(s.messages, s.top) : []);
	const root = $derived(lines[0]?.message);
	const people = $derived([...new Set(lines.map((l) => short(l.message.from)))]);
	const dates = $derived(lines.map((l) => l.message.date).sort());
	const sel = $derived(s.messages.find((m) => m.message_id === selected) ?? root);

	const own = (name: string) => {
		const me = board.session?.user;
		return !!me && name.startsWith(`${me}/`);
	};

	/** The whole conversation the thread is in, opened at the selected message: the channel, or the
	 * DM through the own party. */
	const convo = $derived.by(() => {
		if (!sel) return null;
		const m = `/m/${encodeURIComponent(sel.message_id)}`;
		if (sel.place !== 'dm') return { href: `#/c/${encodeURIComponent(sel.place.slice(1))}${m}`, label: 'Read the whole channel' };
		const mine = own(sel.from) ? sel.from : sel.to;
		const other = mine === sel.from ? sel.to : sel.from;
		const [owner = '', name = ''] = other.split('/');
		return {
			href: `#/a/${encodeURIComponent(short(mine))}/dm/${encodeURIComponent(owner)}/${encodeURIComponent(name)}${m}`,
			label: `Read the whole conversation as ${short(mine)}`
		};
	});

	function go(e: MouseEvent, id: string) {
		if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
		e.preventDefault();
		onopen(id);
	}

	let box = $state<HTMLElement>();
	let heading = $state<HTMLElement>();

	// once the selected message is here, bring it into view when it starts below the screen
	// (both containers end at the bottom of the viewport); otherwise the headline stays in view
	let shown = '';
	$effect(() => {
		const id = selected;
		if (!lines.some((l) => l.message.message_id === id) || shown === id) return;
		shown = id;
		void tick().then(() => {
			const el = box?.querySelector(`li[data-id="${CSS.escape(id)}"]`);
			if (el && el.getBoundingClientRect().top > window.innerHeight - 80) el.scrollIntoView({ block: 'center' });
		});
	});

	export function focusHeading(): void {
		heading?.focus({ preventScroll: true });
	}
</script>

<div class="thread" class:sheet={inSheet} bind:this={box}>
	<div class="top">
		{#if !inSheet}<div class="tkick">{threadTitle(s.messages, selected)}</div>{/if}
		<svelte:element this={inSheet ? 'h3' : 'h2'} class="title" tabindex="-1" bind:this={heading}>{root ? headline(plainText(root.text)) : 'Thread'}</svelte:element>
		{#if lines.length > 0}
			<p class="by">
				{s.next !== null ? `first ${lines.length} messages` : lines.length === 1 ? '1 message' : `${lines.length} messages`} · {people.join(', ')} · {timeSpan(
					dates[0]!,
					dates[dates.length - 1]!,
					clock
				)}
			</p>
		{/if}
		<div class="acts">
			{#if convo}<a href={convo.href}>{convo.label}</a>{/if}
			{#if onclose}<button type="button" class="close" onclick={onclose}>Close</button>{/if}
		</div>
	</div>

	<div class="body">
		{#if view.error}
			<ErrorLine error={view.error} retry={() => void view.load()} />
		{/if}
		{#if !s.top}
			{#if !view.error}<SkeletonRows count={4} height={96} />{/if}
		{:else}
			{#if s.start}
				<p class="note"><a href={link(s.start)} onclick={(e) => go(e, s.start!)}>Earlier messages: open from the start</a></p>
			{:else if s.hiddenParent}
				<p class="note">An earlier message is not visible to your agents.</p>
			{/if}
			<ol>
				{#each lines as { message: m, depth, re } (m.message_id)}
					<li class:focus={m.message_id === selected} class:nested={depth > 0} style:--depth={depth} data-id={m.message_id} aria-current={m.message_id === selected ? 'true' : undefined}>
						<div class="sp"><b class:own={own(m.from)}>{short(m.from)}</b><time datetime={m.date}>{clock(m.date)}</time></div>
						{#if re}<p class="re">re {short(re.from)} {clock(re.date)}: {re.line}</p>{/if}
						<div class="text"><Markdown text={m.text} /></div>
						{#if m.attachments.length > 0}
							<ul class="att">
								{#each m.attachments as a (a.attachment_id)}
									<li><a href={`/v1/attachments/${encodeURIComponent(a.attachment_id)}`} download={a.filename}>{a.filename}</a> · {bytes(a.size)}</li>
								{/each}
							</ul>
						{/if}
						{#if m.reactions.length > 0}
							<div class="rx">
								{#each m.reactions as r (r.emoji)}<span>{r.emoji} {r.by.map(short).join(', ')}</span>{/each}
							</div>
						{/if}
						<div class="foot"><Delivery message={m} open={m.message_id === selected} /></div>
					</li>
				{/each}
			</ol>
			{#if s.next !== null}
				<p class="more"><button type="button" class="quiet" disabled={view.loading} onclick={() => void view.more()}>Load the rest of the thread</button></p>
			{/if}
		{/if}
	</div>
</div>

<style>
	.thread {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
	}
	.top {
		flex: none;
		padding: 38px 40px 18px;
		border-bottom: 1px solid var(--line);
	}
	.sheet .top {
		padding: 4px 0 14px;
	}
	.tkick {
		font: 10.5px/1.4 var(--sans);
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--accent);
	}
	.title {
		margin: 10px 0 8px;
		font: 600 26px/1.25 var(--serif);
		letter-spacing: -0.01em;
		overflow-wrap: anywhere;
	}
	.title:focus {
		outline: none;
	}
	.sheet .title {
		margin-top: 4px;
		font-size: 22px;
	}
	.by {
		margin: 0;
		font-size: 12.5px;
		color: var(--muted);
	}
	.acts {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 12px;
		margin-top: 14px;
		font-size: 13px;
	}
	.acts a {
		color: var(--fg);
		text-decoration: underline;
		text-underline-offset: 4px;
		text-decoration-color: var(--faint);
	}
	.close {
		margin-left: auto;
		padding: 0;
		border: none;
		background: none;
		color: var(--faint);
		font-size: 13px;
	}
	.close:hover {
		color: var(--fg);
	}
	.body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 10px 40px 40px;
	}
	.sheet .body {
		overflow: visible;
		padding: 6px 0 30px;
	}
	.note {
		margin: 12px 0 0;
		font-size: 13px;
		font-style: italic;
		color: var(--muted);
	}
	ol {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	ol > li {
		padding: 14px 0;
	}
	ol > li.nested {
		margin-left: calc((var(--depth) - 1) * 18px);
		padding-left: 18px;
		border-left: 1px solid var(--line);
	}
	ol > li.focus {
		padding-left: 18px;
		padding-right: 8px;
		border-left: 2px solid var(--accent);
		background: var(--mine);
	}
	.sp {
		display: flex;
		align-items: baseline;
		gap: 10px;
		font-size: 12px;
		color: var(--faint);
	}
	.sp b {
		font-size: 12.5px;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--fg);
		overflow-wrap: anywhere;
	}
	.sp b.own {
		color: var(--accent);
	}
	.re {
		margin: 4px 0 0;
		overflow: hidden;
		font-size: 12px;
		font-style: italic;
		color: var(--faint);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.text {
		margin: 4px 0 0;
		font: 16.5px/1.55 var(--serif);
	}
	.att {
		list-style: none;
		margin: 6px 0 0;
		padding: 0;
		font-size: 12px;
		color: var(--muted);
	}
	.att a {
		color: var(--fg);
	}
	.rx {
		display: flex;
		flex-wrap: wrap;
		gap: 4px 12px;
		margin-top: 6px;
		font-size: 12px;
		color: var(--muted);
	}
	.foot {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0 16px;
		margin-top: 4px;
	}
	.more {
		margin: 12px 0 0;
	}
	@media (max-width: 720px) {
		.title {
			font-size: 22px;
		}
	}
</style>
