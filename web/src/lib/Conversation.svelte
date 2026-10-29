<script lang="ts">
	import { tick } from 'svelte';
	import { RpcError, rpc } from './api';
	import { board, onBoard, sameOwner, short } from './board.svelte';
	import { showBack, type Back } from './back.svelte';
	import { KIND, firstLine, replyContext } from './conversation';
	import { acceptEarlier, acceptNewest, emptyThread } from './thread';
	import { day } from './time';
	import type { History, Message as Msg } from './types';
	import Message from './Message.svelte';
	import { plainText } from './markdown';
	import Announcer from './ui/Announcer.svelte';
	import EmptyState from './ui/EmptyState.svelte';
	import ErrorLine from './ui/ErrorLine.svelte';
	import SkeletonRows from './ui/SkeletonRows.svelte';

	/** One of the owner's agents, and its counterpart: `#name` for a channel, else an agent's
	 * canonical name. Read-only: the owner sees what the agent can read. `back` is the phone's way
	 * back to the list this conversation was opened from (shown in the shell's top bar); `members`
	 * a channel's member count; `at` a message to open at instead of the newest. */
	let {
		agent,
		target,
		back = null,
		members = null,
		at = null
	}: { agent: string; target: string; back?: Back | null; members?: number | null; at?: string | null } = $props();
	const isChannel = $derived(target.startsWith('#'));

	let messages = $state<Msg[]>([]);
	let thread = emptyThread();
	let head = $state<History | null>(null);
	let more = $state(false);
	let loading = $state(true);
	let fetching = $state(false);
	let error = $state('');
	let asOf = $state<number | null>(null);
	let notMember = $state(false);
	let announce = $state('');
	let scroller: HTMLElement | undefined = $state();

	const PAGE = 100;
	/** How far back opening at a message pages before it gives up: 20 pages of 100. */
	const REACH_PAGES = 20;
	/** The message opened at, marked once it is on screen; `far` when it was not reached. */
	let marked = $state<string | null>(null);
	let far = $state(false);
	let reaching = false;

	$effect(() => showBack(back));

	function query(extra: Record<string, unknown> = {}) {
		return { agent, ...(isChannel ? { channel: target } : { with: target }), ...extra };
	}

	function nearBottom(): boolean {
		if (!scroller) return true;
		return scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;
	}

	async function scrollToEnd() {
		await tick();
		if (scroller) scroller.scrollTop = scroller.scrollHeight;
	}

	let generation = 0;

	/** Newest page; older pages already loaded are kept. An answer overtaken by a later
	 * request is dropped. */
	async function refresh(first = false) {
		const mine = ++generation;
		try {
			const h = await rpc<History>('owner.history', query({ limit: PAGE }));
			if (mine !== generation || !sameOwner()) return;
			const stick = first || !thread.paged || nearBottom();
			const known = thread.messages.at(-1)?.seq ?? Infinity;
			const arrived = h.messages.filter((m) => m.seq > known).length;
			if (arrived > 0) announce = arrived === 1 ? '1 new message' : `${arrived} new messages`;
			thread = acceptNewest(thread, h);
			messages = thread.messages;
			more = thread.more;
			head = h;
			error = '';
			asOf = Date.now();
			notMember = false;
			loading = false; // render the messages before scrolling to them
			if (first && at) await openAt(at);
			else if (stick) await scrollToEnd();
		} catch (e) {
			if (mine !== generation) return;
			if (e instanceof RpcError && e.code === 'forbidden' && isChannel && e.message.startsWith('not a member'))
				notMember = true; // the agent left the channel
			else error = (e as Error).message;
		} finally {
			loading = false;
		}
	}

	/** The page before the oldest loaded message, keeping the reading position. */
	async function earlier() {
		const first = messages[0];
		if (!first || !scroller || fetching) return;
		fetching = true;
		const height = scroller.scrollHeight;
		try {
			const h = await rpc<History>('owner.history', query({ before: first.seq, limit: PAGE }));
			if (!sameOwner()) return;
			thread = acceptEarlier(thread, h);
			messages = thread.messages;
			more = thread.more;
			await tick();
			scroller.scrollTop = scroller.scrollHeight - height;
		} catch (e) {
			error = (e as Error).message;
		} finally {
			fetching = false;
		}
	}

	/** Pages back until `id` is loaded (at most REACH_PAGES), then brings it into view and marks
	 * it; when it is not reached the conversation opens at the newest with a line saying so. */
	async function openAt(id: string) {
		reaching = true;
		try {
			const has = () => messages.some((m) => m.message_id === id);
			for (let i = 0; i < REACH_PAGES && !has() && more && !error; i++) {
				const n = messages.length;
				await earlier();
				if (messages.length === n) break;
			}
			if (!has()) {
				far = true;
				await scrollToEnd();
				return;
			}
			marked = id;
			await tick();
			document.getElementById(`m-${id}`)?.scrollIntoView({ block: 'center' });
		} finally {
			reaching = false;
		}
	}

	function onscroll() {
		if (reaching) return;
		if (more && !fetching && !error && scroller && scroller.scrollTop < 400) void earlier();
	}

	$effect(() => {
		agent;
		target;
		messages = [];
		thread = emptyThread();
		more = false;
		head = null;
		asOf = null;
		marked = null;
		far = false;
		loading = true;
		void refresh(true);
		return onBoard((topics) => {
			if (topics.includes('messages') || topics.includes('delivery')) void refresh();
		});
	});

	const own = $derived(new Set((board.rail?.agents ?? []).map((a) => a.name)));
	/** DM speakers by bare name (the header names the pair); channel speakers as `short` gives them. */
	const speaker = $derived(isChannel ? short : (n: string) => n.slice(n.indexOf('/') + 1));
	const days = $derived.by(() => {
		const out: { day: string; items: { m: Msg; i: number }[] }[] = [];
		messages.forEach((m, i) => {
			const d = day(m.date);
			if (out.at(-1)?.day !== d) out.push({ day: d, items: [] });
			out.at(-1)!.items.push({ m, i });
		});
		return out;
	});
	function parentOf(i: number) {
		const id = replyContext(messages, i);
		if (!id) return null;
		const p = messages.find((m) => m.message_id === id);
		return p ? { from: speaker(p.from), line: firstLine(plainText(p.text), 60) } : 'earlier';
	}

	const self = $derived(short(agent));
	const counterpart = $derived.by(() => {
		const fa = board.fleet?.agents.find((a) => a.name === target);
		const owner = target.slice(0, target.indexOf('/'));
		const online = fa ? board.fleet!.hosts.some((h) => h.owner === fa.owner && h.host === fa.host && h.online) : null;
		const kind = KIND[head?.kind ?? fa?.kind ?? ''] ?? null;
		const whose = owner === board.session?.user ? 'your' : `${owner}’s`;
		return { name: speaker(target), whose, kind, host: fa?.host ?? null, online };
	});
	const ownMembers = $derived((board.rail?.agents ?? []).filter((a) => a.channels.some((c) => c.channel === target)).map((a) => short(a.name)));
</script>

<section class="conv">
	<header class="head">
		<div class="title">
			{#if isChannel}
				<h1>{target}</h1>
				<p class="mdeck">{members !== null ? `${members} members · ` : ''}yours: {ownMembers.join(', ') || self}</p>
				<p class="deck">
					{#if head?.topic}{head.topic}<br />{/if}
					<span class="facts">{members !== null ? `${members} members; ` : ''}yours: {ownMembers.join(', ') || self}. You read the channel while one of your agents is a member; nothing is sent from here.</span>
				</p>
			{:else}
				{@const c = counterpart}
				<h1>{self} <span class="amp">&amp;</span> {c.name}</h1>
				<p class="mdeck">{c.whose} {c.kind ?? 'agent'}{c.host ? ` on ${c.host}` : ''}{c.online !== null ? ` · ${c.online ? 'online' : 'offline'}` : ''}</p>
				<p class="deck">
					Direct messages with {c.whose} {c.kind ? `${c.kind} agent` : 'agent'}{c.host ? ` on ${c.host}` : ''}{c.online !== null ? `, ${c.online ? 'online now' : 'offline'}` : ''}.
					You read what {self} reads; nothing is sent from here.
				</p>
			{/if}
		</div>
	</header>

	<div class="scroll" bind:this={scroller} {onscroll}>
		<div class="spacer"></div>
		{#if loading}
			<SkeletonRows count={6} height={72} />
		{:else if notMember}
			<EmptyState text={`${self} is not in ${target} any more.`} />
		{:else}
			{#if more}
				<div class="earlier">
					<button class="quiet" onclick={earlier} disabled={fetching}>{fetching ? 'Loading earlier messages' : 'Earlier messages'}</button>
				</div>
			{/if}
			{#if far}
				<p class="far">The message this link points to is further back than the loaded messages, or not visible to {self}.</p>
			{/if}
			{#if messages.length === 0 && !error}
				<EmptyState text="No messages yet." />
			{/if}
			{#each days as g (g.day)}
				<section class="day" aria-label={g.day}>
					<h2>{g.day}</h2>
					<ol>
						{#each g.items as { m, i } (m.message_id)}
							<li><Message message={m} {speaker} mine={own.has(m.from)} parent={parentOf(i)} selected={m.message_id === marked} /></li>
						{/each}
					</ol>
				</section>
			{/each}
		{/if}
		{#if error}<ErrorLine {error} asOf={messages.length > 0 ? asOf : null} retry={() => refresh()} />{/if}
	</div>
	<Announcer message={announce} />
</section>

<style>
	.conv {
		--pad: 56px;
		display: flex;
		flex-direction: column;
		flex: 1;
		height: 100%;
		min-width: 0;
		min-height: 0;
	}
	.head {
		display: flex;
		align-items: flex-start;
		gap: 10px;
		flex: none;
		padding: 34px var(--pad) 20px;
		border-bottom: 1px solid var(--line);
	}
	.title {
		min-width: 0;
	}
	h1 {
		margin: 0;
		font: 600 36px/1.15 var(--serif);
		letter-spacing: -0.015em;
		overflow-wrap: anywhere;
	}
	.amp {
		font-style: italic;
		font-weight: 400;
		color: var(--faint);
	}
	.mdeck {
		display: none;
	}
	.deck {
		margin: 10px 0 0;
		max-width: 660px;
		font: 16px/1.5 var(--serif);
		color: var(--muted);
	}
	.facts {
		color: var(--muted);
	}
	.scroll {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		padding: 0 var(--pad) 44px;
	}
	.spacer {
		margin-top: auto;
	}
	.earlier {
		padding: 18px 0 0;
	}
	.far {
		margin: 18px 0 0;
		font: italic 14px/1.5 var(--serif);
		color: var(--muted);
	}
	.day h2 {
		margin: 30px 0 4px;
		font: italic 400 15px/1 var(--serif);
		color: var(--muted);
	}
	ol {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	@media (max-width: 1280px) {
		.conv {
			--pad: 32px;
			--margin: 140px;
		}
		.head {
			padding-top: 28px;
		}
		h1 {
			font-size: 30px;
		}
	}
	@media (max-width: 720px) {
		.conv {
			--pad: 18px;
		}
		.head {
			align-items: center;
			padding: 10px var(--pad) 10px;
		}
		h1 {
			font-size: 22px;
			white-space: nowrap;
			overflow: hidden;
			text-overflow: ellipsis;
		}
		.deck {
			display: none;
		}
		.mdeck {
			display: block;
			margin: 2px 0 0;
			font-size: 13px;
			color: var(--muted);
			white-space: nowrap;
			overflow: hidden;
			text-overflow: ellipsis;
		}
		.scroll {
			padding-bottom: 24px;
		}
		.day h2 {
			margin-top: 22px;
		}
	}
</style>
