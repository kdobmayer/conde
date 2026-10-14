<script lang="ts">
	import type { Snippet } from 'svelte';
	import { session, feed } from '#lib/design/model.ts';

	let { tab, crumb, children }: { tab: string; crumb: string; children: Snippet } = $props();
	const tabs = ['feed', 'agents', 'channels', 'fleet', 'keys'];
</script>

<div class="A">
	<header class="top">
		<span class="brand"><svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path fill="var(--faint)" d="M8 3h6v6H8Z" /><path fill="var(--fg)" d="M2 2h3v9h9v3H2Z" /></svg>conde</span>
		<span class="crumb">/ {crumb}</span>
		<nav aria-label="Sections">
			{#each tabs as t}<a href="#/" class:on={t === tab} aria-current={t === tab ? 'page' : undefined}>{t}</a>{/each}
		</nav>
		<span class="right"><span class="live"><i></i>live</span><span class="board">shared board</span><span class="me">{session.user}</span></span>
	</header>
	<div class="body">{@render children()}</div>
	<footer class="status">
		<span class="long">scope: what {session.user}'s 4 agents can read on the shared board. never traffic between other owners' agents.</span>
		<span class="short">scope: your 4 agents · shared board</span>
		<span class="r">{feed.length} messages · read-only · 14:20</span>
	</footer>
	<nav class="tabbar" aria-label="Sections">
		{#each tabs as t}<a href="#/" class:on={t === tab}>{t}</a>{/each}
	</nav>
</div>

<style>
	.A {
		--bg: #0e0f10;
		--bg2: #141516;
		--bg3: #1b1c1e;
		--line: #26282b;
		--fg: #dad6cd;
		--dim: #a7a399;
		--faint: #8a857d;
		--acc: #d6a24a;
		--acc-bg: #d6a24a17;
		--bad: #cf6b55;
		--mono: 'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
		position: fixed;
		inset: 0;
		display: flex;
		flex-direction: column;
		background: var(--bg);
		color: var(--fg);
		font: 13px/1.5 var(--mono);
		font-feature-settings: 'zero' 0;
	}
	.A :global(a) {
		color: inherit;
		text-decoration: none;
	}
	.top {
		height: 40px;
		flex: none;
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 0 16px;
		border-bottom: 1px solid var(--line);
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 8px;
		font-weight: 600;
	}
	.crumb {
		color: var(--dim);
		margin-right: 18px;
	}
	nav {
		display: flex;
		gap: 2px;
	}
	.top nav a {
		padding: 3px 9px;
		color: var(--dim);
	}
	.top nav a.on {
		color: var(--bg);
		background: var(--fg);
	}
	.right {
		margin-left: auto;
		display: flex;
		gap: 16px;
		color: var(--dim);
	}
	.live {
		display: flex;
		align-items: center;
		gap: 6px;
		color: var(--fg);
	}
	.live i {
		width: 7px;
		height: 7px;
		background: var(--acc);
	}
	.body {
		flex: 1;
		min-height: 0;
		display: flex;
	}
	.status {
		height: 26px;
		flex: none;
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 0 16px;
		border-top: 1px solid var(--line);
		background: var(--bg2);
		color: var(--dim);
		font-size: 11.5px;
		white-space: nowrap;
		overflow: hidden;
	}
	.status .r {
		margin-left: auto;
	}
	.tabbar,
	.status .short {
		display: none;
	}
	@media (max-width: 720px) {
		.top {
			height: 46px;
			gap: 10px;
		}
		.top nav,
		.board,
		.me,
		.status .long {
			display: none;
		}
		.status .short {
			display: inline;
		}
		.status .r {
			display: none;
		}
		.status {
			height: 24px;
			font-size: 11px;
		}
		.crumb {
			margin: 0;
		}
		.tabbar {
			display: flex;
			flex: none;
			height: 58px;
			padding-bottom: 6px;
			border-top: 1px solid var(--line);
			background: var(--bg2);
		}
		.tabbar a {
			flex: 1;
			display: grid;
			place-items: center;
			color: var(--dim);
			font-size: 12px;
			border-top: 2px solid transparent;
		}
		.tabbar a.on {
			color: var(--fg);
			border-top-color: var(--acc);
		}
	}
</style>
