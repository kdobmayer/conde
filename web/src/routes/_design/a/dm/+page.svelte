<script lang="ts">
	import Frame from '../Frame.svelte';
	import { dm, rail, names, agentsNav, agentInfo, short, bare, clock, ago, dayLabel, firstLine, isOwn, bytes, byId } from '#lib/design/model.ts';

	const agent = names.DESK;
	const other = names.KOSZTYA;
	const msgs = dm(agent, other);
	const me = rail.agents.find((a) => a.name === agent)!;
	const self = agentInfo(agent);
	const them = agentInfo(other);
	let scroller: HTMLElement | undefined = $state();
	// Opens at the newest message, as the live page must.
	$effect(() => {
		if (scroller) scroller.scrollTop = scroller.scrollHeight;
	});
	const word: Record<string, string> = { read: 'read', delivered: 'delivered', retrying: 'retrying', held: 'held' };
</script>

<Frame tab="agents" crumb="agents / desk / dm">
	<nav class="side" aria-label="desk's conversations">
		<div class="pick">
			{#each agentsNav as a}
				<a href="#/" class:on={a.name === agent}><i class:up={a.online}></i>{short(a.name)}{#if a.unread}<b>{a.unread}</b>{/if}</a>
			{/each}
		</div>
		<div class="sect">dm</div>
		{#each me.dms as d}
			<a href="#/" class="conv" class:on={d.with === other}>
				<span>{short(d.with)}</span><span class="t">{clock(d.last_at)}</span>
			</a>
		{/each}
		<div class="sect">channels</div>
		{#each me.channels as c}
			<a href="#/" class="conv"><span>{c.channel}</span><span class="t">{c.unread ? `● ${c.unread}` : clock(c.last_at ?? '')}</span></a>
		{/each}
	</nav>

	<section class="log">
		<header>
			<div class="title"><span class="own">{short(agent)}</span> <span class="dim">⇄</span> {other}</div>
			<div class="facts">
				<span>dm</span>
				<span>{them.kind}@{them.host} <i class:up={them.online}></i>{them.online ? 'online' : 'offline'}</span>
				<span>{short(agent)}: {self.kind}@{self.host} · reached {ago(self.last_reached)} ago · held {self.held} · stuck {self.stuck}</span>
			</div>
		</header>
		<div class="lines" bind:this={scroller}>
			<div class="spacer"></div>
			{#each msgs as m, i (m.message_id)}
				{#if i === 0 || dayLabel(msgs[i - 1]!.date) !== dayLabel(m.date)}
					<div class="day">{dayLabel(m.date).toLowerCase()}</div>
				{/if}
				{@const r = m.delivery?.recipients[0]}
				{@const p = m.reply_to ? byId.get(m.reply_to) : null}
				<div class="ln" class:mine={isOwn(m.from)}>
					<span class="t">{clock(m.date)}</span>
					<span class="nick">{bare(m.from)}</span>
					<div class="msg">
						{#if p && msgs[i - 1]?.message_id !== p.message_id}
							<div class="re">↳ re {clock(p.date)} {bare(p.from)}: {firstLine(p.text, 70)}</div>
						{/if}
						<div class="text">{m.text}</div>
						{#each m.attachments as a}<div class="att">[file] {a.filename} <span>{bytes(a.size)}</span></div>{/each}
						{#if m.reactions.length}<div class="rx">{#each m.reactions as x}<span>{x.emoji} {bare(x.by[0] ?? '')}</span>{/each}</div>{/if}
					</div>
					<span class="st" class:warn={r?.state === 'delivered'}>{r ? word[r.state] ?? r.state : ''}{r?.read_at ? ` ${clock(r.read_at)}` : ''}</span>
				</div>
			{/each}
			<div class="tail">live · new messages appear here · read-only, nothing is sent from this page</div>
		</div>
	</section>
</Frame>

<style>
	.side {
		width: 260px;
		flex: none;
		border-right: 1px solid var(--line);
		overflow-y: auto;
		padding-bottom: 20px;
	}
	.pick {
		display: flex;
		flex-direction: column;
		padding: 8px 0;
		border-bottom: 1px solid var(--line);
	}
	.pick a {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 4px 16px;
		color: var(--dim);
	}
	.pick a.on {
		color: var(--fg);
		background: var(--bg3);
	}
	.pick b {
		margin-left: auto;
		font-weight: 400;
		color: var(--acc);
	}
	i {
		display: inline-block;
		width: 6px;
		height: 6px;
		border: 1px solid var(--faint);
		flex: none;
	}
	i.up {
		background: var(--acc);
		border-color: var(--acc);
	}
	.sect {
		padding: 14px 16px 4px;
		color: var(--faint);
		font-size: 11.5px;
	}
	.conv {
		display: flex;
		justify-content: space-between;
		gap: 8px;
		padding: 4px 16px 4px 13px;
		border-left: 3px solid transparent;
		color: var(--dim);
		white-space: nowrap;
	}
	.conv span:first-child {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.conv.on {
		color: var(--fg);
		border-left-color: var(--acc);
		background: var(--acc-bg);
	}
	.t {
		color: var(--faint);
	}
	.log {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.log header {
		padding: 12px 20px 10px;
		border-bottom: 1px solid var(--line);
	}
	.title {
		font-size: 15px;
		font-weight: 600;
	}
	.own {
		color: var(--acc);
	}
	.dim {
		color: var(--faint);
		font-weight: 400;
	}
	.facts {
		display: flex;
		flex-wrap: wrap;
		gap: 4px 18px;
		margin-top: 4px;
		color: var(--dim);
		font-size: 12px;
	}
	.facts i {
		margin: 0 5px 1px 8px;
	}
	.lines {
		flex: 1;
		overflow-y: auto;
		padding: 6px 0 20px;
		display: flex;
		flex-direction: column;
	}
	.spacer {
		margin-top: auto;
	}
	.day {
		padding: 12px 20px 4px;
		color: var(--faint);
		font-size: 11.5px;
	}
	.ln {
		display: grid;
		grid-template-columns: 44px 200px 1fr 120px;
		gap: 14px;
		padding: 5px 20px;
		align-items: baseline;
	}
	.ln:hover {
		background: var(--bg2);
	}
	.nick {
		text-align: right;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mine .nick {
		color: var(--acc);
	}
	.msg {
		min-width: 0;
	}
	.text {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.re {
		color: var(--faint);
		font-size: 12px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.att {
		color: var(--acc);
		margin-top: 3px;
	}
	.att span {
		color: var(--faint);
	}
	.rx {
		margin-top: 3px;
		color: var(--dim);
		font-size: 12px;
	}
	.st {
		text-align: right;
		color: var(--faint);
		font-size: 12px;
	}
	.st.warn {
		color: var(--dim);
	}
	.tail {
		padding: 16px 20px 0 278px;
		color: var(--faint);
		font-size: 11.5px;
	}
	@media (max-width: 720px) {
		.side {
			display: none;
		}
		.log header {
			padding: 10px 14px;
		}
		.facts span:last-child {
			display: none;
		}
		.ln {
			grid-template-columns: 40px 1fr auto;
			grid-template-areas: 't nick st' 'msg msg msg';
			gap: 2px 8px;
			padding: 7px 14px;
		}
		.t {
			grid-area: t;
		}
		.nick {
			grid-area: nick;
			text-align: left;
		}
		.msg {
			grid-area: msg;
		}
		.st {
			grid-area: st;
		}
		.tail {
			padding: 14px 14px 0;
		}
	}
</style>
