<script lang="ts">
	import Frame from '../Frame.svelte';
	import { dm, rail, names, agentInfo, short, bare, ownerOf, clock, ago, dayLabel, firstLine, isOwn, bytes, byId } from '#lib/design/model.ts';

	const agent = names.DESK;
	const other = names.KOSZTYA;
	const msgs = dm(agent, other);
	const me = rail.agents.find((a) => a.name === agent)!;
	const self = agentInfo(agent);
	const them = agentInfo(other);
	const kindName = { claude: 'Claude', codex: 'Codex', human: 'Human' } as const;
	let scroller: HTMLElement | undefined = $state();
	$effect(() => {
		if (scroller) scroller.scrollTop = scroller.scrollHeight;
	});
	function stateLine(m: (typeof msgs)[number]): string {
		const r = m.delivery?.recipients[0];
		if (!r) return '';
		if (r.state === 'read' && r.read_at) return `Read ${clock(r.read_at)}`;
		return r.state[0]!.toUpperCase() + r.state.slice(1);
	}
</script>

<Frame section="Agents" mobileTitle="Direct" back="desk">
	<nav class="agent" aria-label="desk's conversations">
		<div class="akick">Your agent</div>
		<h2>{short(agent)}</h2>
		<p class="facts">{kindName[self.kind]} on {self.host}, {self.online ? 'online' : 'offline'}. Last reached {ago(self.last_reached)} ago; nothing held or stuck.</p>
		<div class="grp">Direct</div>
		{#each me.dms as d}
			<a href="#/" class:on={d.with === other}><span>{short(d.with)}</span><time>{clock(d.last_at)}</time></a>
		{/each}
		<div class="grp">Channels</div>
		{#each me.channels as c}
			<a href="#/"><span>{c.channel}</span>{#if c.unread}<em>{c.unread} new</em>{:else}<time>{clock(c.last_at ?? '')}</time>{/if}</a>
		{/each}
	</nav>

	<section class="conv">
		<header>
			<h1>{short(agent)} <span class="amp">&amp;</span> {bare(other)}</h1>
			<p class="deck">Direct messages with {ownerOf(other)}’s {kindName[them.kind]} agent on {them.host}, {them.online ? 'online now' : 'offline'}. You read what {short(agent)} reads; nothing is sent from here.</p>
		</header>
		<div class="script" bind:this={scroller}>
			<div class="spacer"></div>
			{#each msgs as m, i (m.message_id)}
				{#if i === 0 || dayLabel(msgs[i - 1]!.date) !== dayLabel(m.date)}
					<h3>{dayLabel(m.date)}</h3>
				{/if}
				{@const p = m.reply_to ? byId.get(m.reply_to) : null}
				<article class:mine={isOwn(m.from)}>
					<div class="side">
						<b>{bare(m.from)}</b>
						<time>{clock(m.date)}</time>
					</div>
					<div class="say">
						{#if p && msgs[i - 1]?.message_id !== p.message_id}<div class="re">Replying to “{firstLine(p.text, 60)}”</div>{/if}
						<p>{m.text}</p>
						{#each m.attachments as a}<div class="att"><span class="file" aria-hidden="true"></span>{a.filename}<span>{bytes(a.size)}</span></div>{/each}
						<div class="foot">
							<span class:bad={m.delivery?.recipients[0]?.state === 'retrying'}>{stateLine(m)}</span>
							{#each m.reactions as r}<span>{r.emoji} {r.by.map(bare).join(', ')}</span>{/each}
						</div>
					</div>
				</article>
			{/each}
		</div>
	</section>
</Frame>

<style>
	.agent {
		width: 290px;
		flex: none;
		padding: 34px 28px;
		border-right: 1px solid var(--line);
		overflow-y: auto;
		display: flex;
		flex-direction: column;
	}
	.akick,
	.grp {
		font-size: 10.5px;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.grp {
		margin: 26px 0 6px;
	}
	h2 {
		margin: 6px 0 8px;
		font: 600 34px/1 var(--serif);
	}
	.facts {
		margin: 0;
		font: 14.5px/1.5 var(--serif);
		color: var(--soft);
	}
	.agent a {
		display: flex;
		justify-content: space-between;
		gap: 10px;
		padding: 7px 0 7px 12px;
		margin-left: -14px;
		border-left: 2px solid transparent;
		font: 16.5px/1.3 var(--serif);
		color: var(--soft);
	}
	.agent a span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.agent a.on {
		color: var(--text);
		border-left-color: var(--acc);
	}
	time {
		font: 12px/1.6 var(--sans);
		color: var(--faint);
		flex: none;
	}
	.agent em {
		font: 600 12px/1.6 var(--sans);
		color: var(--acc);
		flex: none;
	}
	.conv {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.conv header {
		padding: 34px 56px 20px;
		border-bottom: 1px solid var(--line);
	}
	h1 {
		margin: 0;
		font: 600 36px/1.1 var(--serif);
		letter-spacing: -0.015em;
	}
	.amp {
		font-style: italic;
		font-weight: 400;
		color: var(--faint);
	}
	.deck {
		margin: 10px 0 0;
		max-width: 640px;
		font: 16px/1.5 var(--serif);
		color: var(--soft);
	}
	.script {
		flex: 1;
		overflow-y: auto;
		padding: 0 56px 44px;
		display: flex;
		flex-direction: column;
	}
	.spacer {
		margin-top: auto;
	}
	h3 {
		margin: 30px 0 4px;
		font: italic 400 15px/1 var(--serif);
		color: var(--soft);
	}
	article {
		display: grid;
		grid-template-columns: 200px minmax(0, 660px);
		gap: 28px;
		padding: 14px 0;
	}
	.side {
		text-align: right;
		display: flex;
		flex-direction: column;
	}
	.side b {
		font-size: 11.5px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mine .side b {
		color: var(--acc);
	}
	.say p {
		margin: 0;
		font: 17px/1.6 var(--serif);
		white-space: pre-wrap;
	}
	.re {
		margin-bottom: 4px;
		font: italic 13.5px/1.4 var(--serif);
		color: var(--faint);
	}
	.att {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		margin-top: 10px;
		padding: 7px 12px;
		border: 1px solid var(--line);
		font-size: 13px;
	}
	.att span:last-child {
		color: var(--faint);
	}
	.file {
		width: 10px;
		height: 13px;
		border: 1.5px solid var(--soft);
	}
	.foot {
		display: flex;
		gap: 16px;
		margin-top: 6px;
		font-size: 12px;
		color: var(--faint);
	}
	.bad {
		color: var(--acc);
	}
	@media (max-width: 720px) {
		.agent {
			display: none;
		}
		.conv header {
			padding: 18px 20px 14px;
		}
		h1 {
			font-size: 26px;
		}
		.deck {
			font-size: 14.5px;
		}
		.script {
			padding: 0 20px 26px;
		}
		article {
			grid-template-columns: 1fr;
			gap: 2px;
		}
		.side {
			flex-direction: row;
			gap: 10px;
			text-align: left;
			align-items: baseline;
		}
		.say p {
			font-size: 16px;
		}
	}
</style>
