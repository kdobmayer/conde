<script lang="ts">
	import Frame from '../Frame.svelte';
	import { dm, names, agentInfo, short, bare, ownerOf, clock, ago, dayLabel, firstLine, isOwn, bytes, byId, channels } from '#lib/design/model.ts';

	const agent = names.DESK;
	const other = names.KOSZTYA;
	const msgs = dm(agent, other);
	const self = agentInfo(agent);
	const them = agentInfo(other);
	const files = msgs.flatMap((m) => m.attachments.map((a) => ({ ...a, from: m.from, date: m.date })));
	const common = Object.entries(channels).filter(([, c]) => c.members.includes(agent) && c.members.includes(other)).map(([n]) => n);
	const unreadByThem = msgs.filter((m) => m.from === agent && m.delivery?.recipients[0]?.state !== 'read').length;
	let scroller: HTMLElement | undefined = $state();
	$effect(() => {
		if (scroller) scroller.scrollTop = scroller.scrollHeight;
	});
	function ini(name: string): string {
		const parts = bare(name).split('-');
		return (parts.length > 1 ? parts[0]![0]! + parts[1]![0]! : bare(name).slice(0, 2)).toUpperCase();
	}
	function tick(m: (typeof msgs)[number]): string {
		const r = m.delivery?.recipients[0];
		if (!r) return '';
		return r.state === 'read' && r.read_at ? `Read ${clock(r.read_at)}` : r.state[0]!.toUpperCase() + r.state.slice(1);
	}
</script>

<Frame nav="agents" expanded={agent} selected={other} title={bare(other)} back="desk">
	<section class="center">
		<header class="bar">
			<span class="av">{ini(other)}</span>
			<div class="t">
				<b>{other}</b>
				<small><i class="dot" class:up={them.online}></i>{them.kind} on {them.host} · {them.online ? 'online' : 'offline'} · DM with your agent <strong>{short(agent)}</strong></small>
			</div>
			<span class="seg"><span class="on">Messages</span><span>Files {files.length}</span></span>
		</header>
		<div class="msgs" bind:this={scroller}>
			<div class="spacer"></div>
			{#each msgs as m, i (m.message_id)}
				{#if i === 0 || dayLabel(msgs[i - 1]!.date) !== dayLabel(m.date)}
					<div class="day"><span>{dayLabel(m.date)}</span></div>
				{/if}
				{@const p = m.reply_to ? byId.get(m.reply_to) : null}
				{@const mine = isOwn(m.from)}
				<div class="row" class:mine>
					{#if !mine}<span class="av">{ini(m.from)}</span>{/if}
					<div class="col">
						<div class="bub">
							{#if p && msgs[i - 1]?.message_id !== p.message_id}<div class="q"><b>{bare(p.from)}</b> {firstLine(p.text, 60)}</div>{/if}
							<p>{m.text}</p>
							{#each m.attachments as a}<div class="att"><span class="fi"></span><div><b>{a.filename}</b><small>{bytes(a.size)} · download</small></div></div>{/each}
						</div>
						<div class="meta">
							<time>{clock(m.date)}</time>
							{#if tick(m)}<span class:pending={tick(m) === 'Delivered'}>{tick(m)}</span>{/if}
							{#each m.reactions as r}<span class="rx">{r.emoji} {bare(r.by[0] ?? '')}</span>{/each}
						</div>
					</div>
					{#if mine}<span class="av own">{ini(m.from)}</span>{/if}
				</div>
			{/each}
			<div class="ro">Read-only view of what {short(agent)} reads · new messages arrive live</div>
		</div>
	</section>

	<aside class="insp" aria-label="Details">
		<div class="ih"><b>Details</b></div>
		<div class="sec">
			<h3>Participants</h3>
			<div class="pc">
				<span class="av own">{ini(agent)}</span>
				<div><b>{short(agent)}</b><small>your {self.kind} · {self.host}</small></div>
				<span class="st"><i class="dot" class:up={self.online}></i>{self.online ? 'online' : 'offline'}</span>
			</div>
			<dl>
				<dt>Last reached</dt><dd>{ago(self.last_reached)} ago</dd>
				<dt>Held</dt><dd>{self.held}</dd>
				<dt>Stuck</dt><dd>{self.stuck}</dd>
			</dl>
			<div class="pc">
				<span class="av">{ini(other)}</span>
				<div><b>{bare(other)}</b><small>{ownerOf(other)}’s {them.kind} · {them.host}</small></div>
				<span class="st"><i class="dot" class:up={them.online}></i>{them.online ? 'online' : 'offline'}</span>
			</div>
		</div>
		<div class="sec">
			<h3>Delivery</h3>
			<dl>
				<dt>Messages</dt><dd>{msgs.length}</dd>
				<dt>Sent by {short(agent)}, not yet read</dt><dd>{unreadByThem}</dd>
				<dt>Failed</dt><dd>0</dd>
			</dl>
		</div>
		<div class="sec">
			<h3>Files</h3>
			{#each files as f}<div class="fl"><span class="fi"></span><div><b>{f.filename}</b><small>{bytes(f.size)} · {bare(f.from)} · {clock(f.date)}</small></div></div>{/each}
		</div>
		<div class="sec">
			<h3>Both are in</h3>
			<div class="chs">{#each common as c}<span>{c}</span>{/each}</div>
		</div>
	</aside>
</Frame>

<style>
	time,
	dd {
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
		gap: 12px;
		height: 60px;
		flex: none;
		padding: 0 24px;
		border-bottom: 1px solid var(--line);
	}
	.bar .av {
		width: 32px;
		height: 32px;
	}
	.t b {
		display: block;
		font-weight: 600;
		font-size: 14.5px;
	}
	.t small {
		display: flex;
		align-items: center;
		gap: 6px;
		color: var(--faint);
		font-size: 12px;
	}
	.t strong {
		color: var(--acc);
		font-weight: 500;
	}
	.seg {
		margin-left: auto;
		display: flex;
		padding: 3px;
		border-radius: 8px;
		background: var(--panel);
		border: 1px solid var(--line);
	}
	.seg span {
		padding: 4px 12px;
		border-radius: 6px;
		color: var(--faint);
		font-size: 12.5px;
	}
	.seg .on {
		background: var(--raise);
		color: var(--text);
	}
	.msgs {
		flex: 1;
		overflow-y: auto;
		padding: 0 28px 20px;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.spacer {
		margin-top: auto;
	}
	.day {
		display: flex;
		justify-content: center;
		margin: 14px 0 4px;
	}
	.day span {
		padding: 2px 10px;
		border-radius: 10px;
		background: var(--panel);
		color: var(--faint);
		font-size: 11.5px;
	}
	.row {
		display: flex;
		align-items: flex-end;
		gap: 10px;
		max-width: 76%;
	}
	.row.mine {
		align-self: flex-end;
	}
	.col {
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.mine .col {
		align-items: flex-end;
	}
	.bub {
		padding: 9px 13px;
		border-radius: 12px 12px 12px 4px;
		background: var(--panel);
		border: 1px solid var(--line);
	}
	.mine .bub {
		border-radius: 12px 12px 4px 12px;
		background: var(--acc-soft);
		border-color: #7fa7cd33;
	}
	.bub p {
		margin: 0;
		white-space: pre-wrap;
		line-height: 1.5;
	}
	.q {
		margin-bottom: 6px;
		padding: 3px 8px;
		border-left: 2px solid var(--acc);
		border-radius: 3px;
		background: #ffffff08;
		color: var(--soft);
		font-size: 12px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.q b {
		color: var(--text);
		font-weight: 600;
	}
	.att,
	.fl {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-top: 8px;
		padding: 8px 10px;
		border-radius: 8px;
		background: var(--raise);
	}
	.att b,
	.fl b {
		display: block;
		font-weight: 500;
		font-size: 12.5px;
	}
	.att small,
	.fl small {
		color: var(--faint);
		font-size: 11.5px;
	}
	.fi {
		width: 22px;
		height: 26px;
		border-radius: 3px 8px 3px 3px;
		background: var(--acc-soft);
		border: 1.5px solid var(--acc);
		flex: none;
	}
	.meta {
		display: flex;
		gap: 10px;
		margin-top: 4px;
		padding: 0 4px;
		font-size: 11.5px;
		color: var(--faint);
	}
	.pending {
		color: var(--soft);
	}
	.rx {
		color: var(--soft);
	}
	.ro {
		align-self: center;
		margin-top: 12px;
		color: var(--faint);
		font-size: 11.5px;
	}
	.insp {
		width: 340px;
		flex: none;
		background: var(--panel);
		border-left: 1px solid var(--line);
		overflow-y: auto;
	}
	.ih {
		height: 60px;
		display: flex;
		align-items: center;
		padding: 0 18px;
		border-bottom: 1px solid var(--line);
	}
	.sec {
		padding: 14px 18px;
		border-bottom: 1px solid var(--line);
	}
	h3 {
		margin: 0 0 10px;
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.pc {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 6px 0;
	}
	.pc b {
		display: block;
		font-weight: 600;
	}
	.pc small {
		color: var(--faint);
		font-size: 12px;
	}
	.st {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 12px;
		color: var(--soft);
	}
	dl {
		display: grid;
		grid-template-columns: 1fr auto;
		gap: 4px 12px;
		margin: 4px 0 8px 36px;
		font-size: 12.5px;
	}
	.sec:not(:first-of-type) dl {
		margin-left: 0;
	}
	dt {
		color: var(--soft);
	}
	dd {
		margin: 0;
		text-align: right;
	}
	.chs {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}
	.chs span {
		padding: 2px 9px;
		border-radius: 5px;
		background: var(--raise);
		color: var(--soft);
		font-size: 12px;
	}
	@media (max-width: 720px) {
		.bar,
		.insp {
			display: none;
		}
		.msgs {
			padding: 0 12px 16px;
		}
		.row {
			max-width: 88%;
		}
		.row .av {
			display: none;
		}
	}
</style>
