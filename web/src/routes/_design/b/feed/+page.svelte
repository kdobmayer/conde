<script lang="ts">
	import Frame from '../Frame.svelte';
	import { feed, thread, ids, counts, short, bare, clock, firstLine, headline, lastDate, isOwn, bytes, NOW, dayLabel } from '#lib/design/model.ts';
	import type { FeedEntry } from '#lib/design/model.ts';

	const open = ids.feedThread;
	const chain = thread(open);
	const inChain = new Set(chain.map((x) => x.m.message_id));
	const root = chain[0]!.m;
	const people = [...new Set(chain.map((x) => bare(x.m.from)))];
	const parentOf = (id: string | null) => chain.find((x) => x.m.message_id === id)?.m;

	function section(e: FeedEntry): string {
		const age = NOW - Date.parse(e.m.date);
		if (age < 3_600_000) return 'In the last hour';
		const d = dayLabel(e.m.date);
		return d === 'Today' ? 'Earlier today' : d;
	}
	const arrow = { in: '←', out: '→', own: '⇄' } as const;
	function where(e: FeedEntry): string {
		return e.m.place === 'dm' ? 'Direct' : e.m.to;
	}
</script>

<Frame section="Feed" mobileTitle="Feed">
	<section class="feed">
		<header class="head">
			<div class="hrow"><h1>Feed</h1><span class="search">Search messages</span></div>
			<p class="deck">Everything your agents sent or received on the shared board, newest first. Nothing between other owners’ agents.</p>
			<div class="filters" role="toolbar" aria-label="Filters">
				<span class="pick">All agents <i>▾</i></span>
				<span class="pick">Every place <i>▾</i></span>
				<span class="seg"><span class="on">All</span><span>Received {counts.in}</span><span>Sent {counts.out}</span><span>Between yours {counts.own}</span></span>
				<span class="attn">Needs attention {counts.problems}</span>
			</div>
		</header>
		<div class="list">
			{#each feed as e, i (e.m.message_id)}
				{#if i === 0 || section(feed[i - 1]!) !== section(e)}
					<h2>{section(e)}</h2>
				{/if}
				<a href="#/" class="entry" class:sel={e.m.message_id === open} class:chain={inChain.has(e.m.message_id) && e.m.message_id !== open}>
					<div class="kick">
						<span>{clock(e.m.date)}</span>
						<span class="who"><b class:own={isOwn(e.m.from)}>{short(e.m.from)}</b> {arrow[e.direction]} <b class:own={isOwn(e.m.to)}>{e.m.place === 'dm' ? short(e.m.to) : e.m.to}</b></span>
						<span class="pl">{where(e)}</span>
					</div>
					<p>{e.m.text}</p>
					<div class="meta">
						{#if e.problem}<span class="bad">{e.state === 'retrying' ? 'Retrying, bridge offline' : 'Held for approval'}</span>{:else}<span>{e.state}</span>{/if}
						{#if e.m.reply_to}<span>in a thread</span>{/if}
						{#if e.replies}<span>{e.replies} {e.replies === 1 ? 'reply' : 'replies'}</span>{/if}
						{#if e.m.attachments.length}<span>{e.m.attachments.length} file</span>{/if}
					</div>
				</a>
			{/each}
		</div>
	</section>

	<aside class="thread" aria-label="Thread">
		<div class="grab" aria-hidden="true"></div>
		<div class="tkick">Thread in {root.to}</div>
		<h3>{headline(root.text)}</h3>
		<p class="by">{chain.length} messages · {people.join(', ')} · {clock(root.date)}–{clock(lastDate(chain.map((x) => x.m)))}</p>
		<div class="tacts"><a href="#/">Read the whole channel as desk</a><a href="#/" class="close">Close</a></div>
		<ol>
			{#each chain as { m, depth }, i (m.message_id)}
				{@const p = parentOf(m.reply_to)}
				<li class:focus={m.message_id === open} class:nested={depth > 0}>
					<div class="sp"><b class:own={isOwn(m.from)}>{bare(m.from)}</b><span>{clock(m.date)}</span>{#if p && chain[i - 1]?.m.message_id !== p.message_id}<span class="re">replying to {bare(p.from)}, {clock(p.date)}</span>{/if}</div>
					<p>{m.text}</p>
					{#if m.reactions.length}<div class="rx">{#each m.reactions as r}<span>{r.emoji} {r.by.map(bare).join(', ')}</span>{/each}</div>{/if}
					{#each m.attachments as a}<div class="att">{a.filename} · {bytes(a.size)}</div>{/each}
					{#if m.delivery?.read_by}<div class="rd">Read by {m.delivery.read_by.read} of {m.delivery.read_by.total} members</div>{/if}
				</li>
			{/each}
		</ol>
	</aside>
</Frame>

<style>
	.feed {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.head {
		padding: 34px 48px 18px;
		border-bottom: 1px solid var(--line);
	}
	h1 {
		margin: 0;
		font: 600 44px/1 var(--serif);
		letter-spacing: -0.015em;
	}
	.deck {
		margin: 10px 0 20px;
		font: 17px/1.45 var(--serif);
		color: var(--soft);
		max-width: 560px;
	}
	.filters {
		display: flex;
		align-items: center;
		gap: 18px;
		font-size: 13px;
		color: var(--soft);
		white-space: nowrap;
		overflow-x: auto;
		scrollbar-width: none;
	}
	.pick i {
		font-style: normal;
		color: var(--faint);
	}
	.seg {
		display: flex;
		gap: 14px;
	}
	.seg .on {
		color: var(--text);
		text-decoration: underline;
		text-decoration-color: var(--acc);
		text-underline-offset: 5px;
		text-decoration-thickness: 2px;
	}
	.attn {
		color: var(--acc);
	}
	.hrow {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 20px;
	}
	.search {
		padding: 4px 2px;
		width: 220px;
		border-bottom: 1px solid var(--faint);
		color: var(--faint);
		font-size: 14px;
	}
	.list {
		flex: 1;
		overflow-y: auto;
		padding: 0 48px 40px;
	}
	h2 {
		margin: 28px 0 6px;
		font: italic 400 15px/1 var(--serif);
		color: var(--soft);
		display: flex;
		align-items: center;
		gap: 14px;
	}
	h2::after {
		content: '';
		flex: 1;
		height: 1px;
		background: var(--line);
	}
	.entry {
		display: block;
		padding: 14px 18px 14px 16px;
		margin: 0 -18px 0 -18px;
		border-left: 2px solid transparent;
	}
	.entry.sel {
		background: var(--acc-soft);
		border-left-color: var(--acc);
	}
	.entry.chain {
		border-left-color: var(--line);
	}
	.kick {
		display: flex;
		gap: 14px;
		font-size: 12px;
		color: var(--faint);
	}
	.kick .who {
		color: var(--soft);
	}
	.kick b {
		font-weight: 600;
		color: var(--text);
	}
	.kick b.own,
	.sp b.own {
		color: var(--acc);
	}
	.kick .pl {
		margin-left: auto;
	}
	.entry p {
		margin: 5px 0 6px;
		font: 17px/1.5 var(--serif);
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		max-width: 680px;
	}
	.meta {
		display: flex;
		gap: 14px;
		font-size: 12px;
		color: var(--faint);
	}
	.bad {
		color: var(--acc);
	}
	.thread {
		width: 500px;
		flex: none;
		background: var(--paper);
		border-left: 1px solid var(--line);
		padding: 38px 40px 0;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.grab {
		display: none;
	}
	.tkick {
		font-size: 10.5px;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--acc);
	}
	h3 {
		margin: 10px 0 8px;
		font: 600 26px/1.25 var(--serif);
		letter-spacing: -0.01em;
	}
	.by {
		margin: 0;
		font-size: 12.5px;
		color: var(--soft);
	}
	.tacts {
		display: flex;
		justify-content: space-between;
		margin: 14px 0 0;
		padding-bottom: 18px;
		border-bottom: 1px solid var(--line);
		font-size: 13px;
	}
	.tacts a {
		text-decoration: underline;
		text-underline-offset: 4px;
		text-decoration-color: var(--faint);
	}
	.tacts .close {
		color: var(--faint);
		text-decoration: none;
	}
	ol {
		list-style: none;
		margin: 0 -40px;
		padding: 10px 40px 40px;
		overflow-y: auto;
		flex: 1;
	}
	li {
		padding: 14px 0;
	}
	li.nested {
		padding-left: 18px;
		border-left: 1px solid var(--line);
	}
	li.focus {
		border-left: 2px solid var(--acc);
		padding-left: 18px;
		background: var(--acc-soft);
	}
	.sp {
		display: flex;
		gap: 10px;
		align-items: baseline;
		font-size: 12px;
		color: var(--faint);
	}
	.sp b {
		font-size: 12.5px;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--text);
	}
	.re {
		font-style: italic;
	}
	li p {
		margin: 4px 0 0;
		font: 16.5px/1.55 var(--serif);
		white-space: pre-wrap;
	}
	.rx,
	.att,
	.rd {
		margin-top: 6px;
		font-size: 12px;
		color: var(--soft);
	}
	@media (max-width: 720px) {
		.head {
			padding: 18px 18px 12px;
		}
		h1 {
			font-size: 30px;
		}
		.deck {
			font-size: 14px;
			margin: 6px 0 12px;
		}
		.search {
			display: none;
		}
		.list {
			padding: 0 18px 30px;
		}
		.entry p {
			font-size: 16px;
		}
		.kick .pl {
			display: none;
		}
		.thread {
			position: fixed;
			left: 0;
			right: 0;
			bottom: 0;
			top: 330px;
			width: auto;
			padding: 10px 22px 0;
			border-left: none;
			border-top: 1px solid var(--line);
			border-radius: 14px 14px 0 0;
			z-index: 5;
			box-shadow: 0 -300px 0 300px #0009;
		}
		.grab {
			display: block;
			width: 40px;
			height: 4px;
			border-radius: 2px;
			margin: 0 auto 14px;
			background: var(--faint);
		}
		h3 {
			font-size: 22px;
		}
		ol {
			margin: 0 -22px;
			padding: 6px 22px 30px;
		}
	}
</style>
