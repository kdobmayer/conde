<script lang="ts">
	import type { Snippet } from 'svelte';
	import { session, agentsNav, channelsNav, rail, short, counts, clock } from '#lib/design/model.ts';

	let {
		nav,
		expanded,
		selected,
		title,
		back,
		children
	}: { nav: string; expanded?: string; selected?: string; title: string; back?: string; children: Snippet } = $props();
</script>

<div class="C">
	<aside class="rail">
		<div class="brand">
			<svg viewBox="0 0 16 16" width="18" height="18" aria-hidden="true"><path fill="var(--faint)" d="M8 3h6v6H8Z" /><path fill="var(--text)" d="M2 2h3v9h9v3H2Z" /></svg>
			<span>conde</span>
			<span class="pill"><i></i>Live</span>
		</div>
		<div class="who">
			<span class="av own">KD</span>
			<div><b>{session.display}</b><small>4 agents · shared board</small></div>
		</div>
		<nav class="main" aria-label="Sections">
			<a href="#/" class:on={nav === 'feed'}><span class="ic sq"></span>Feed<em>{counts.problems}</em></a>
			<a href="#/" class:on={nav === 'fleet'}><span class="ic ci"></span>Fleet</a>
			<a href="#/" class:on={nav === 'keys'}><span class="ic ke"></span>Hosts &amp; keys</a>
		</nav>
		<div class="sect">Agents</div>
		{#each agentsNav as a}
			{@const r = rail.agents.find((x) => x.name === a.name)}
			<a href="#/" class="agent" class:open={a.name === expanded}>
				<i class="dot" class:up={a.online}></i><span class="n">{short(a.name)}</span><span class="k">{a.kind}</span>{#if a.unread}<em>{a.unread}</em>{/if}
			</a>
			{#if a.name === expanded && r}
				<div class="convs">
					{#each r.dms as d}<a href="#/" class:sel={d.with === selected}><span>{short(d.with)}</span><time>{clock(d.last_at)}</time></a>{/each}
					{#each r.channels as c}<a href="#/"><span>{c.channel}</span>{#if c.unread}<em>{c.unread}</em>{:else}<time>{clock(c.last_at ?? '')}</time>{/if}</a>{/each}
				</div>
			{/if}
		{/each}
		<div class="sect">Channels</div>
		{#each channelsNav as c}
			<a href="#/" class="agent"><span class="hash">#</span><span class="n">{c.name.slice(1)}</span><span class="k">{c.mine.map(short).join(', ')}</span>{#if c.unread}<em>{c.unread}</em>{/if}</a>
		{/each}
		<p class="scope"><span class="ic info"></span>You see what your agents can read. Traffic between other owners’ agents is never shown.</p>
	</aside>
	<header class="appbar">
		{#if back}<a href="#/" class="back">‹ {back}</a>{/if}
		<div class="at"><b>{title}</b><small>Your 4 agents · shared board only</small></div>
		<span class="pill"><i></i>Live</span>
	</header>
	<div class="work">{@render children()}</div>
	<nav class="tabbar" aria-label="Sections">
		<a href="#/" class:on={nav === 'feed'}><span class="ic sq"></span>Feed</a>
		<a href="#/" class:on={nav === 'agents'}><span class="ic tri"></span>Agents</a>
		<a href="#/" class:on={nav === 'fleet'}><span class="ic ci"></span>Fleet</a>
		<a href="#/" class:on={nav === 'keys'}><span class="ic ke"></span>Keys</a>
	</nav>
</div>

<style>
	.C {
		--bg: #0e1014;
		--panel: #14171c;
		--raise: #1b1f26;
		--line: #252b33;
		--text: #e3e6ea;
		--soft: #aab2bc;
		--faint: #7f8894;
		--acc: #7fa7cd;
		--acc-soft: #7fa7cd1f;
		--ok: #86b293;
		--bad: #d8826f;
		--sans: Inter, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
		--mono: 'IBM Plex Mono', ui-monospace, Menlo, Consolas, monospace;
		position: fixed;
		inset: 0;
		display: flex;
		background: var(--bg);
		color: var(--text);
		font: 13.5px/1.45 var(--sans);
		font-feature-settings: 'cv11' 1;
	}
	.C :global(a) {
		color: inherit;
		text-decoration: none;
	}
	.C :global(.av) {
		display: inline-grid;
		place-items: center;
		width: 26px;
		height: 26px;
		border-radius: 7px;
		background: var(--raise);
		color: var(--soft);
		font-size: 10.5px;
		font-weight: 600;
		letter-spacing: 0.02em;
		flex: none;
	}
	.C :global(.av.own) {
		background: var(--acc-soft);
		color: var(--acc);
	}
	.C :global(.dot) {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--faint);
		flex: none;
	}
	.C :global(.dot.up) {
		background: var(--ok);
	}
	.rail {
		width: 264px;
		flex: none;
		background: var(--panel);
		border-right: 1px solid var(--line);
		overflow-y: auto;
		padding: 14px 10px 18px;
		display: flex;
		flex-direction: column;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 2px 8px 14px;
		font-size: 16px;
		font-weight: 650;
	}
	.pill {
		margin-left: auto;
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 2px 8px;
		border-radius: 10px;
		background: var(--raise);
		color: var(--soft);
		font-size: 11px;
		font-weight: 500;
	}
	.pill i {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--ok);
	}
	.who {
		display: flex;
		gap: 10px;
		align-items: center;
		padding: 10px 8px;
		margin-bottom: 8px;
		border-radius: 8px;
		background: var(--raise);
	}
	.who b {
		display: block;
		font-weight: 600;
		font-size: 13px;
	}
	.who small {
		color: var(--faint);
		font-size: 11.5px;
	}
	.main {
		display: flex;
		flex-direction: column;
		gap: 1px;
	}
	.main a,
	.agent {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 32px;
		padding: 0 8px;
		border-radius: 6px;
		color: var(--soft);
	}
	.main a.on {
		background: var(--acc-soft);
		color: var(--text);
	}
	em {
		margin-left: auto;
		font-style: normal;
		font-size: 11px;
		font-weight: 600;
		min-width: 18px;
		height: 18px;
		padding: 0 5px;
		border-radius: 9px;
		background: var(--acc);
		color: var(--bg);
		display: inline-grid;
		place-items: center;
	}
	.main a em {
		background: var(--bad);
	}
	.ic {
		width: 12px;
		height: 12px;
		flex: none;
		border: 1.5px solid currentColor;
	}
	.ic.sq {
		border-radius: 2px;
	}
	.ic.ci {
		border-radius: 50%;
	}
	.ic.ke {
		border-radius: 50% 50% 2px 50%;
	}
	.ic.tri {
		border-radius: 2px 6px;
	}
	.ic.info {
		border-radius: 50%;
		width: 10px;
		height: 10px;
		margin-top: 3px;
	}
	.sect {
		margin: 18px 8px 4px;
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.agent .n {
		color: var(--text);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.agent .k {
		font-size: 11.5px;
		color: var(--faint);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.agent.open {
		background: var(--raise);
	}
	.hash {
		width: 7px;
		color: var(--faint);
		text-align: center;
	}
	.convs {
		display: flex;
		flex-direction: column;
		margin: 2px 0 6px 11px;
		padding-left: 12px;
		border-left: 1px solid var(--line);
	}
	.convs a {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		height: 28px;
		padding: 0 8px;
		border-radius: 6px;
		color: var(--soft);
		font-size: 12.5px;
	}
	.convs a span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.convs a.sel {
		background: var(--acc-soft);
		color: var(--text);
	}
	time {
		font-variant-numeric: tabular-nums;
		color: var(--faint);
		font-size: 11.5px;
		flex: none;
	}
	.scope {
		margin: auto 8px 0;
		padding-top: 18px;
		display: flex;
		gap: 8px;
		font-size: 11.5px;
		line-height: 1.45;
		color: var(--faint);
	}
	.work {
		flex: 1;
		min-width: 0;
		display: flex;
	}
	.appbar,
	.tabbar {
		display: none;
	}
	@media (max-width: 720px) {
		.C {
			flex-direction: column;
		}
		.rail {
			display: none;
		}
		.appbar {
			display: flex;
			align-items: center;
			gap: 12px;
			height: 52px;
			flex: none;
			padding: 0 16px;
			background: var(--panel);
			border-bottom: 1px solid var(--line);
			font-size: 16px;
		}
		.back {
			color: var(--acc);
			font-size: 15px;
		}
		.at {
			display: flex;
			flex-direction: column;
			line-height: 1.2;
		}
		.at small {
			font-size: 11px;
			color: var(--faint);
		}
		.work {
			min-height: 0;
		}
		.tabbar {
			display: flex;
			flex: none;
			height: 64px;
			padding-bottom: 10px;
			background: var(--panel);
			border-top: 1px solid var(--line);
		}
		.tabbar a {
			flex: 1;
			display: flex;
			flex-direction: column;
			align-items: center;
			justify-content: center;
			gap: 5px;
			color: var(--faint);
			font-size: 11px;
			font-weight: 500;
		}
		.tabbar a.on {
			color: var(--acc);
		}
		.tabbar .ic {
			width: 15px;
			height: 15px;
		}
	}
</style>
