<script lang="ts">
	import Frame from '../Frame.svelte';
	import { feed, thread, ids, counts, short, bare, clock, dayLabel, firstLine, headline, lastDate, isOwn, bytes } from '#lib/design/model.ts';

	const open = ids.feedThread;
	const chain = thread(open);
	const inChain = new Set(chain.map((x) => x.m.message_id));
	const root = chain[0]!.m;
	const people = [...new Set(chain.map((x) => x.m.from))];
	const glyph = { in: '←', out: '→', own: '⇄' } as const;
	const parentOf = (id: string | null) => chain.find((x) => x.m.message_id === id)?.m;
</script>

<Frame tab="feed" crumb="feed">
	<section class="feed">
		<div class="filters" role="toolbar" aria-label="Filters">
			<span class="f"><b>agent</b> all<i>▾</i></span>
			<span class="f seg"><b>dir</b><span class="on">all {counts.all}</span><span>in {counts.in}</span><span>out {counts.out}</span><span>own {counts.own}</span></span>
			<span class="f"><b>place</b> all<i>▾</i></span>
			<span class="f warn">! problems {counts.problems}</span>
			<span class="search">/ search</span>
		</div>
		<div class="rows">
			{#each feed as e, i (e.m.message_id)}
				{#if i === 0 || dayLabel(feed[i - 1]!.m.date) !== dayLabel(e.m.date)}
					<div class="day">{dayLabel(e.m.date).toLowerCase()}</div>
				{/if}
				<a href="#/" class="row" class:sel={e.m.message_id === open} class:chain={inChain.has(e.m.message_id) && e.m.message_id !== open}>
					<span class="t">{clock(e.m.date)}</span>
					<span class="d d-{e.direction}" title={e.direction}>{glyph[e.direction]}</span>
					<span class="who"><span class:own={isOwn(e.m.from)}>{short(e.m.from)}</span></span>
					<span class="to"><span class:own={isOwn(e.m.to)}>{e.m.place === 'dm' ? short(e.m.to) : e.m.to}</span></span>
					<span class="txt">{#if e.m.reply_to}<span class="re">↳</span>{/if}{firstLine(e.m.text, 140)}</span>
					<span class="n">{e.replies ? `+${e.replies}` : ''}</span>
					<span class="st" class:bad={e.problem}>{e.problem ? `! ${e.state}` : e.state}</span>
				</a>
			{/each}
			<div class="end">end of loaded history · older on scroll</div>
		</div>
	</section>

	<aside class="thread" aria-label="Thread">
		<div class="grab" aria-hidden="true"></div>
		<header>
			<div class="kicker">thread · {root.to} · {chain.length} messages · {people.length} participants</div>
			<h2>{headline(root.text)}</h2>
			<div class="acts"><a href="#/">open {root.to} as desk →</a><a href="#/" class="x">close [esc]</a></div>
		</header>
		<ol>
			{#each chain as { m, depth }, i (m.message_id)}
				{@const p = parentOf(m.reply_to)}
				<li class:focus={m.message_id === open} style={`--d:${Math.min(depth, 3)}`}>
					<div class="mh">
						<span class="t">{clock(m.date)}</span>
						<span class="who" class:own={isOwn(m.from)}>{short(m.from)}</span>
						<span class="st">{m.delivery?.read_by ? `read ${m.delivery.read_by.read}/${m.delivery.read_by.total}` : ''}</span>
					</div>
					{#if p && chain[i - 1]?.m.message_id !== p.message_id}<div class="re">↳ re {bare(p.from)} {clock(p.date)}: {firstLine(p.text, 40)}</div>{/if}
					<p>{m.text}</p>
					{#if m.reactions.length}<div class="rx">{#each m.reactions as r}<span>{r.emoji} {r.by.length}</span>{/each}</div>{/if}
					{#if m.attachments.length}{#each m.attachments as a}<div class="att">[file] {a.filename} <span>{bytes(a.size)}</span></div>{/each}{/if}
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
	.filters {
		flex: none;
		display: flex;
		align-items: center;
		gap: 18px;
		height: 38px;
		padding: 0 16px;
		border-bottom: 1px solid var(--line);
		color: var(--dim);
		white-space: nowrap;
		overflow-x: auto;
		scrollbar-width: none;
	}
	.f b {
		font-weight: 400;
		color: var(--faint);
		margin-right: 6px;
	}
	.f i {
		font-style: normal;
		margin-left: 4px;
		color: var(--faint);
	}
	.seg span {
		padding: 2px 6px;
	}
	.seg .on {
		color: var(--fg);
		box-shadow: inset 0 -2px 0 var(--acc);
	}
	.warn {
		color: var(--bad);
	}
	.search {
		margin-left: auto;
		padding: 2px 10px;
		border: 1px solid var(--line);
		min-width: 200px;
		color: var(--faint);
	}
	.rows {
		flex: 1;
		overflow-y: auto;
	}
	.day {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 10px 16px 4px;
		color: var(--faint);
		font-size: 11.5px;
	}
	.day::after {
		content: '';
		flex: 1;
		height: 1px;
		background: var(--line);
	}
	.row {
		display: grid;
		grid-template-columns: 44px 14px 150px 170px 1fr 28px 84px;
		gap: 10px;
		align-items: baseline;
		height: 29px;
		padding: 5px 16px 0 13px;
		border-left: 3px solid transparent;
		white-space: nowrap;
	}
	.row:hover {
		background: var(--bg2);
	}
	.row.sel {
		background: var(--acc-bg);
		border-left-color: var(--acc);
	}
	.row.chain {
		border-left-color: var(--faint);
	}
	.row > span {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.t {
		color: var(--dim);
	}
	.d {
		color: var(--faint);
	}
	.d-out {
		color: var(--acc);
	}
	.who,
	.to {
		color: var(--dim);
	}
	.own {
		color: var(--acc);
	}
	.re {
		color: var(--faint);
		margin-right: 6px;
	}
	.n {
		color: var(--dim);
		text-align: right;
	}
	.st {
		color: var(--faint);
		text-align: right;
	}
	.st.bad {
		color: var(--bad);
	}
	.end {
		padding: 14px 16px 24px;
		color: var(--faint);
		font-size: 11.5px;
	}
	.thread {
		width: 500px;
		flex: none;
		border-left: 1px solid var(--line);
		background: var(--bg2);
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.grab {
		display: none;
	}
	.thread header {
		padding: 14px 20px 12px;
		border-bottom: 1px solid var(--line);
	}
	.kicker {
		color: var(--dim);
		font-size: 11.5px;
	}
	h2 {
		margin: 6px 0 10px;
		font-size: 15px;
		font-weight: 600;
		line-height: 1.4;
	}
	.acts {
		display: flex;
		justify-content: space-between;
		font-size: 12px;
	}
	.acts a {
		color: var(--acc);
	}
	.acts .x {
		color: var(--dim);
	}
	ol {
		list-style: none;
		margin: 0;
		padding: 8px 0 24px;
		overflow-y: auto;
		flex: 1;
	}
	li {
		position: relative;
		padding: 8px 20px 8px calc(20px + var(--d) * 18px);
	}
	li::before {
		content: '';
		position: absolute;
		left: calc(13px + var(--d) * 18px);
		top: 0;
		bottom: 0;
		width: 1px;
		background: var(--line);
	}
	li.focus {
		background: var(--acc-bg);
	}
	li.focus::before {
		background: var(--acc);
		width: 2px;
	}
	.mh {
		display: flex;
		gap: 10px;
	}
	.mh .who {
		color: var(--fg);
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	li .re {
		font-size: 12px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.mh .who.own {
		color: var(--acc);
	}
	.mh .st {
		margin-left: auto;
	}
	p {
		margin: 2px 0 0;
		white-space: pre-wrap;
		color: var(--fg);
	}
	.rx {
		margin-top: 4px;
		display: flex;
		gap: 10px;
		color: var(--dim);
	}
	.att {
		margin-top: 4px;
		color: var(--acc);
	}
	.att span {
		color: var(--faint);
	}
	@media (max-width: 720px) {
		.filters {
			gap: 14px;
			height: 40px;
		}
		.search {
			display: none;
		}
		.row {
			grid-template-columns: 40px 14px 1fr 62px;
			grid-template-areas: 't d who st' '. . txt txt';
			height: auto;
			row-gap: 0;
			padding: 7px 14px 7px 11px;
		}
		.t {
			grid-area: t;
		}
		.d {
			grid-area: d;
		}
		.who {
			grid-area: who;
		}
		.to,
		.n {
			display: none;
		}
		.txt {
			grid-area: txt;
			color: var(--dim);
		}
		.st {
			grid-area: st;
			font-size: 11.5px;
		}
		.thread {
			position: fixed;
			left: 0;
			right: 0;
			bottom: 0;
			top: 196px;
			width: auto;
			border-left: none;
			border-top: 2px solid var(--acc);
			z-index: 5;
			box-shadow: 0 -200px 0 200px #0008;
		}
		.grab {
			display: block;
			width: 36px;
			height: 3px;
			margin: 8px auto 0;
			background: var(--faint);
		}
		.thread header {
			padding: 8px 16px 10px;
		}
		li {
			padding: 8px 16px 8px calc(16px + var(--d) * 12px);
		}
		li::before {
			left: calc(9px + var(--d) * 12px);
		}
	}
</style>
