<script lang="ts">
	import { tick } from 'svelte';
	import { RpcError, rpc } from './api';
	import { changed, onBoard, short } from './board.svelte';
	import { bytes, clock, day } from './time';
	import type { History, Message } from './types';
	import Delivery from './Delivery.svelte';
	import SendBox from './SendBox.svelte';

	/** `#name` for a channel, else an agent's canonical name. */
	let { target }: { target: string } = $props();
	const isChannel = $derived(target.startsWith('#'));

	let messages = $state<Message[]>([]);
	let head = $state<History | null>(null);
	let more = $state(false);
	let loading = $state(true);
	let error = $state('');
	let notMember = $state(false);
	let replyTo = $state<Message | null>(null);
	let quoted = $state<Record<string, Message | 'missing'>>({});
	let flash = $state('');
	let scroller: HTMLElement | undefined = $state();

	const PAGE = 100;

	function query(extra: Record<string, unknown> = {}) {
		return { ...(isChannel ? { channel: target } : { with: target }), ...extra };
	}

	function nearBottom(): boolean {
		if (!scroller) return true;
		return scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;
	}

	async function scrollToEnd() {
		await tick();
		if (scroller) scroller.scrollTop = scroller.scrollHeight;
	}

	/** Newest page; older pages already loaded are kept. */
	async function refresh(first = false) {
		const stick = first || nearBottom();
		try {
			const h = await rpc<History>('conversation.history', query({ limit: PAGE }));
			const oldest = h.messages[0]?.seq ?? Infinity;
			const older = messages.filter((m) => m.seq < oldest);
			messages = [...older, ...h.messages];
			if (first) more = h.more;
			head = h;
			error = '';
			notMember = false;
			await markRead(h);
			if (stick) await scrollToEnd();
		} catch (e) {
			if (e instanceof RpcError && e.code === 'forbidden' && isChannel) notMember = true;
			else error = (e as Error).message;
		} finally {
			loading = false;
		}
	}

	async function earlier() {
		const first = messages[0];
		if (!first || !scroller) return;
		const height = scroller.scrollHeight;
		try {
			const h = await rpc<History>('conversation.history', query({ before: first.seq, limit: PAGE }));
			messages = [...h.messages, ...messages];
			more = h.more;
			await tick();
			scroller.scrollTop = scroller.scrollHeight - height;
		} catch (e) {
			error = (e as Error).message;
		}
	}

	/** Marks what is on screen read, only when something is unread (each mark is a write). */
	async function markRead(h: History) {
		if (document.visibilityState !== 'visible') return;
		const last = h.messages[h.messages.length - 1];
		if (!last) return;
		const unread = isChannel ? (h.read_seq ?? 0) < last.seq : (h.unread ?? 0) > 0;
		if (!unread) return;
		try {
			await rpc('conversation.read', query({ up_to: last.seq }));
			changed(['messages']);
		} catch {
			// a failed mark leaves the badge; the next refresh tries again
		}
	}

	async function join() {
		try {
			await rpc('channel.join', { channel: target });
			changed(['messages']);
			loading = true;
			await refresh(true);
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function showReply(id: string) {
		const el = document.getElementById(`m-${id}`);
		if (el) {
			el.scrollIntoView({ block: 'center', behavior: 'smooth' });
			flash = id;
			setTimeout(() => (flash = ''), 1500);
			return;
		}
		if (quoted[id]) {
			const { [id]: _, ...rest } = quoted;
			quoted = rest;
			return;
		}
		try {
			const r = await rpc<{ message: Message }>('message.get', { id });
			quoted = { ...quoted, [id]: r.message };
		} catch {
			quoted = { ...quoted, [id]: 'missing' };
		}
	}

	function preview(id: string): string {
		const m = messages.find((x) => x.message_id === id);
		return m ? `${short(m.from)}: ${m.text.split('\n')[0]}` : 'an earlier message';
	}

	$effect(() => {
		target;
		messages = [];
		head = null;
		quoted = {};
		replyTo = null;
		loading = true;
		void refresh(true);
		return onBoard((topics) => {
			if (topics.includes('messages') || topics.includes('delivery')) void refresh();
		});
	});

	const title = $derived(isChannel ? target : short(target));
</script>

<section class="conv">
	<header class="head">
		<a class="back" href="#/" aria-label="All conversations">‹</a>
		<div class="title">
			<h1>{title}</h1>
			{#if head?.topic}<p class="muted topic">{head.topic}</p>{/if}
			{#if !isChannel && head?.kind}<p class="muted topic">{head.kind}</p>{/if}
		</div>
	</header>

	<div class="scroll" bind:this={scroller}>
		{#if loading}
			<p class="muted pad">Loading…</p>
		{:else if notMember}
			<div class="pad">
				<p>You are not in {target}. Its messages are visible to members only.</p>
				<button onclick={join}>Join {target}</button>
			</div>
		{:else}
			{#if more}
				<div class="pad center"><button class="quiet" onclick={earlier}>Load earlier messages</button></div>
			{/if}
			{#if messages.length === 0 && !error}
				<p class="muted pad">No messages yet.</p>
			{/if}
			{#each messages as m, i (m.message_id)}
				{#if i === 0 || day(messages[i - 1]!.date) !== day(m.date)}
					<div class="day muted"><span>{day(m.date)}</span></div>
				{/if}
				<article id={`m-${m.message_id}`} class="msg" class:mine={m.from === head?.me} class:flash={flash === m.message_id}>
					<div class="meta">
						<span class="from">{short(m.from)}</span>
						<time datetime={m.date} title={new Date(m.date).toLocaleString()}>{clock(m.date)}</time>
						{#if m.reply_to}
							<button class="link reply-link" onclick={() => showReply(m.reply_to!)}>
								reply to {preview(m.reply_to)}
							</button>
						{/if}
					</div>
					{#if m.reply_to && quoted[m.reply_to]}
						{@const q = quoted[m.reply_to]}
						<blockquote>
							{#if q === 'missing'}
								<span class="muted">That message is not visible to you.</span>
							{:else if q}
								<span class="muted">{short(q.from)} · {day(q.date)} {clock(q.date)}</span>
								<div class="text">{q.text}</div>
							{/if}
						</blockquote>
					{/if}
					{#if m.text}<div class="text">{m.text}</div>{/if}
					{#if m.attachments.length > 0}
						<ul class="atts">
							{#each m.attachments as a (a.attachment_id)}
								<li>
									<a href={`/v1/attachments/${encodeURIComponent(a.attachment_id)}`} download={a.filename}>{a.filename}</a>
									<span class="muted">{bytes(a.size)}</span>
								</li>
							{/each}
						</ul>
					{/if}
					{#if m.reactions.length > 0}
						<div class="reactions">
							{#each m.reactions as r (r.emoji)}
								<span class="tag" title={r.by.join(', ')}>{r.emoji} {r.by.length}</span>
							{/each}
						</div>
					{/if}
					<Delivery message={m} />
					<div class="actions">
						<button class="link" onclick={() => (replyTo = m)}>Reply</button>
						<span class="muted id">{m.message_id}</span>
					</div>
				</article>
			{/each}
		{/if}
		{#if error}<p class="error pad">{error}</p>{/if}
	</div>

	{#if !notMember && !loading}
		<SendBox to={target} bind:replyTo />
	{/if}
</section>

<style>
	.conv {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
	}
	.head {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 8px 16px;
		border-bottom: 1px solid var(--line);
		flex: none;
	}
	.back {
		display: none;
		font-size: 24px;
		line-height: 1;
		text-decoration: none;
		color: inherit;
		padding: 0 6px 2px 0;
	}
	.title {
		min-width: 0;
	}
	h1 {
		font-size: 16px;
		font-weight: 650;
		margin: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.topic {
		margin: 0;
		font-size: 13px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.scroll {
		flex: 1;
		overflow-y: auto;
		padding: 8px 0 16px;
	}
	.pad {
		padding: 8px 16px;
	}
	.center {
		text-align: center;
	}
	.day {
		text-align: center;
		font-size: 12px;
		margin: 14px 0 6px;
	}
	.msg {
		padding: 6px 16px;
		border-left: 3px solid transparent;
	}
	.msg.mine {
		background: var(--mine);
	}
	.msg.flash {
		border-left-color: var(--accent);
	}
	.meta {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 8px;
		font-size: 13px;
	}
	.from {
		font-weight: 650;
	}
	time {
		color: var(--muted);
		font-size: 12px;
	}
	.reply-link {
		font-size: 12.5px;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		text-align: left;
	}
	.text {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		margin-top: 2px;
	}
	blockquote {
		margin: 4px 0;
		padding: 4px 10px;
		border-left: 3px solid var(--line);
		font-size: 13.5px;
	}
	.atts {
		list-style: none;
		margin: 4px 0 0;
		padding: 0;
		font-size: 13.5px;
	}
	.atts a {
		overflow-wrap: anywhere;
	}
	.reactions {
		display: flex;
		gap: 4px;
		flex-wrap: wrap;
		margin-top: 4px;
	}
	.actions {
		display: flex;
		gap: 10px;
		align-items: baseline;
		font-size: 12px;
		margin-top: 2px;
	}
	.id {
		font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
		font-size: 11px;
	}
	@media (max-width: 720px) {
		.back {
			display: block;
		}
		.id {
			display: none;
		}
	}
</style>
