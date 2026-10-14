<script lang="ts">
	import Frame from '../Frame.svelte';
	import { feed, thread, ids, counts, short, bare, clock, firstLine, headline, lastDate, isOwn, bytes, agentInfo } from '#lib/design/model.ts';

	const open = ids.feedThread;
	const chain = thread(open);
	const root = chain[0]!.m;
	const people = [...new Set(chain.map((x) => x.m.from))];
	const parentOf = (id: string | null) => chain.find((x) => x.m.message_id === id)?.m;
	const today = feed.filter((e) => Date.parse(e.m.date) > Date.parse('2026-10-13T22:00:00Z')).length;

	function ini(name: string): string {
		const b = bare(name);
		const parts = b.split('-');
		return (parts.length > 1 ? parts[0]![0]! + parts[1]![0]! : b.slice(0, 2)).toUpperCase();
	}
	const dirWord = { in: 'Received', out: 'Sent', own: 'Between yours' } as const;
</script>

<Frame nav="feed" title="Feed">
	<section class="center">
		<header class="bar">
			<h1>Feed</h1>
			<span class="sub">All messages of your agents · newest first</span>
			<span class="search"><span class="ic"></span>Search messages</span>
		</header>
		<div class="tiles" role="toolbar" aria-label="Filter by direction">
			<a href="#/" class="tile on"><b>{today}</b><span>Today</span></a>
			<a href="#/" class="tile"><b>{counts.in}</b><span>Received</span></a>
			<a href="#/" class="tile"><b>{counts.out}</b><span>Sent</span></a>
			<a href="#/" class="tile"><b>{counts.own}</b><span>Between yours</span></a>
			<a href="#/" class="tile warn"><b>{counts.problems}</b><span>Needs attention</span></a>
		</div>
		<div class="chips">
			<span class="chip">Agent: All<i>▾</i></span>
			<span class="chip">Place: All<i>▾</i></span>
			<span class="chip">Counterpart: Any<i>▾</i></span>
			<span class="hint">Showing {counts.all} · live</span>
		</div>
		<div class="cards">
			{#each feed as e (e.m.message_id)}
				<a href="#/" class="card" class:sel={e.m.message_id === open}>
					<span class="av" class:own={isOwn(e.m.from)}>{ini(e.m.from)}</span>
					<div class="cb">
						<div class="ch">
							<b>{short(e.m.from)}</b><span class="arrow">→</span><span class="to">{e.m.place === 'dm' ? short(e.m.to) : ''}</span>
							<span class="place" class:dm={e.m.place === 'dm'}>{e.m.place === 'dm' ? 'DM' : e.m.to}</span>
							<time>{clock(e.m.date)}</time>
						</div>
						<p>{firstLine(e.m.text, 200)}</p>
						<div class="cf">
							<span class="tag" class:bad={e.problem}>{e.problem ? (e.state === 'held' ? 'Held for approval' : 'Retrying · bridge offline') : e.state}</span>
							<span class="tag ghost">{dirWord[e.direction]}</span>
							{#if e.replies}<span class="tag ghost">{e.replies} {e.replies === 1 ? 'reply' : 'replies'}</span>{/if}
							{#if e.m.reply_to}<span class="tag ghost">in thread</span>{/if}
							{#if e.m.attachments.length}<span class="tag ghost">1 file</span>{/if}
						</div>
					</div>
				</a>
			{/each}
		</div>
	</section>

	<aside class="insp" aria-label="Thread">
		<div class="grab" aria-hidden="true"></div>
		<div class="ih">
			<div class="tabs"><span class="on">Thread</span><span>Participants {people.length}</span></div>
			<span class="x">✕</span>
		</div>
		<div class="iti">
			<span class="place">{root.to}</span>
			<h2>{headline(root.text)}</h2>
			<div class="ppl">
				{#each people as p}<span class="pp"><i class="dot" class:up={agentInfo(p).online}></i>{bare(p)}</span>{/each}
			</div>
		</div>
		<ol>
			{#each chain as { m, depth }, i (m.message_id)}
				{@const p = parentOf(m.reply_to)}
				<li class:focus={m.message_id === open} class:nested={depth > 0}>
					<span class="av" class:own={isOwn(m.from)}>{ini(m.from)}</span>
					<div class="bub">
						<div class="bh"><b>{bare(m.from)}</b><time>{clock(m.date)}</time>{#if m.delivery?.read_by}<span class="rd">read {m.delivery.read_by.read}/{m.delivery.read_by.total}</span>{/if}</div>
						{#if p && chain[i - 1]?.m.message_id !== p.message_id}<div class="q">↳ {bare(p.from)}: {firstLine(p.text, 48)}</div>{/if}
						<p>{m.text}</p>
						{#if m.reactions.length}<div class="rx">{#each m.reactions as r}<span>{r.emoji} {r.by.length}</span>{/each}</div>{/if}
						{#each m.attachments as a}<div class="att">{a.filename} · {bytes(a.size)}</div>{/each}
					</div>
				</li>
			{/each}
		</ol>
		<a href="#/" class="openin">Open {root.to} as desk</a>
	</aside>
</Frame>

<style>
	time,
	.tile b,
	.rd {
		font-variant-numeric: tabular-nums;
	}
	.center {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.bar {
		display: flex;
		align-items: center;
		gap: 14px;
		height: 60px;
		flex: none;
		padding: 0 24px;
		border-bottom: 1px solid var(--line);
	}
	h1 {
		margin: 0;
		font-size: 18px;
		font-weight: 650;
	}
	.sub {
		color: var(--faint);
	}
	.search {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 8px;
		width: 260px;
		height: 32px;
		padding: 0 10px;
		border-radius: 7px;
		background: var(--panel);
		border: 1px solid var(--line);
		color: var(--faint);
	}
	.search .ic {
		width: 10px;
		height: 10px;
		border: 1.5px solid currentColor;
		border-radius: 50%;
	}
	.tiles {
		display: flex;
		gap: 10px;
		padding: 16px 24px 0;
		flex: none;
		overflow-x: auto;
		scrollbar-width: none;
	}
	.tile {
		flex: 1;
		min-width: 104px;
		padding: 10px 14px;
		border-radius: 9px;
		background: var(--panel);
		border: 1px solid var(--line);
		display: flex;
		flex-direction: column;
		gap: 1px;
	}
	.tile b {
		font-size: 22px;
		font-weight: 650;
		letter-spacing: -0.01em;
	}
	.tile span {
		font-size: 12px;
		color: var(--soft);
	}
	.tile.on {
		border-color: var(--acc);
		background: var(--acc-soft);
	}
	.tile.warn b {
		color: var(--bad);
	}
	.chips {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 12px 24px;
		flex: none;
		border-bottom: 1px solid var(--line);
		white-space: nowrap;
		overflow-x: auto;
		scrollbar-width: none;
	}
	.chip {
		height: 28px;
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 0 10px;
		border-radius: 14px;
		border: 1px solid var(--line);
		color: var(--soft);
		font-size: 12.5px;
	}
	.chip i {
		font-style: normal;
		color: var(--faint);
	}
	.hint {
		margin-left: auto;
		color: var(--faint);
		font-size: 12px;
	}
	.cards {
		flex: 1;
		overflow-y: auto;
		padding: 12px 16px 24px;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.card {
		display: flex;
		gap: 12px;
		padding: 11px 12px;
		border-radius: 9px;
		border: 1px solid transparent;
	}
	.card:hover {
		background: var(--panel);
	}
	.card.sel {
		background: var(--acc-soft);
		border-color: #7fa7cd55;
	}
	.cb {
		flex: 1;
		min-width: 0;
	}
	.ch {
		display: flex;
		align-items: baseline;
		gap: 7px;
		white-space: nowrap;
	}
	.ch b {
		font-weight: 600;
	}
	.arrow {
		color: var(--faint);
	}
	.to {
		color: var(--soft);
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.place {
		padding: 0 7px;
		border-radius: 5px;
		background: var(--raise);
		color: var(--soft);
		font-size: 11.5px;
		line-height: 19px;
	}
	.place.dm {
		color: var(--faint);
	}
	.ch time {
		margin-left: auto;
		color: var(--faint);
		font-size: 12px;
	}
	.card p {
		margin: 3px 0 7px;
		color: var(--soft);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.cf {
		display: flex;
		gap: 6px;
	}
	.tag {
		font-size: 11.5px;
		line-height: 20px;
		padding: 0 7px;
		border-radius: 5px;
		background: var(--raise);
		color: var(--soft);
	}
	.tag.ghost {
		background: none;
		border: 1px solid var(--line);
		color: var(--faint);
	}
	.tag.bad {
		background: #d8826f22;
		color: var(--bad);
	}
	.insp {
		width: 440px;
		flex: none;
		background: var(--panel);
		border-left: 1px solid var(--line);
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.grab {
		display: none;
	}
	.ih {
		display: flex;
		align-items: center;
		height: 60px;
		flex: none;
		padding: 0 18px;
		border-bottom: 1px solid var(--line);
	}
	.tabs {
		display: flex;
		gap: 4px;
	}
	.tabs span {
		padding: 5px 10px;
		border-radius: 6px;
		color: var(--faint);
		font-weight: 500;
	}
	.tabs .on {
		background: var(--raise);
		color: var(--text);
	}
	.x {
		margin-left: auto;
		color: var(--faint);
	}
	.iti {
		padding: 16px 18px 14px;
		border-bottom: 1px solid var(--line);
	}
	h2 {
		margin: 8px 0 10px;
		font-size: 16px;
		font-weight: 600;
		line-height: 1.35;
	}
	.ppl {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}
	.pp {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 2px 8px;
		border-radius: 12px;
		background: var(--raise);
		font-size: 12px;
		color: var(--soft);
	}
	ol {
		list-style: none;
		margin: 0;
		padding: 10px 14px 16px;
		overflow-y: auto;
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	li {
		display: flex;
		gap: 10px;
		padding: 8px 6px;
		border-radius: 8px;
	}
	li.nested {
		margin-left: 22px;
	}
	li.focus {
		background: var(--acc-soft);
	}
	.bub {
		flex: 1;
		min-width: 0;
	}
	.bh {
		display: flex;
		align-items: baseline;
		gap: 8px;
	}
	.bh b {
		font-weight: 600;
	}
	.bh time {
		font-size: 11.5px;
	}
	.rd {
		margin-left: auto;
		font-size: 11.5px;
		color: var(--faint);
	}
	.q {
		margin-top: 3px;
		padding-left: 8px;
		border-left: 2px solid var(--line);
		color: var(--faint);
		font-size: 12px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	li p {
		margin: 3px 0 0;
		white-space: pre-wrap;
		color: var(--text);
	}
	.rx {
		margin-top: 5px;
		display: flex;
		gap: 6px;
		font-size: 12px;
		color: var(--soft);
	}
	.att {
		margin-top: 5px;
		font-size: 12px;
		color: var(--acc);
	}
	.openin {
		flex: none;
		margin: 0 18px 16px;
		height: 36px;
		display: grid;
		place-items: center;
		border-radius: 8px;
		border: 1px solid var(--line);
		color: var(--acc);
		font-weight: 500;
	}
	@media (max-width: 720px) {
		.bar {
			display: none;
		}
		.tiles {
			padding: 12px 14px 0;
			gap: 8px;
		}
		.tile {
			min-width: 92px;
			padding: 8px 12px;
		}
		.tile b {
			font-size: 19px;
		}
		.chips {
			padding: 10px 14px;
		}
		.hint {
			display: none;
		}
		.cards {
			padding: 8px 6px 20px;
		}
		.ch .to {
			display: none;
		}
		.insp {
			position: fixed;
			left: 0;
			right: 0;
			bottom: 0;
			top: 250px;
			width: auto;
			border-left: none;
			border-top: 1px solid var(--line);
			border-radius: 16px 16px 0 0;
			z-index: 5;
			box-shadow: 0 -300px 0 300px #000a;
		}
		.grab {
			display: block;
			width: 40px;
			height: 4px;
			border-radius: 2px;
			margin: 8px auto 0;
			background: var(--faint);
			flex: none;
		}
		.ih {
			height: 46px;
		}
	}
</style>
