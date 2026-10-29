<script lang="ts">
	import { board } from './board.svelte';
	import { CHOICES, LABELS } from './theme';
	import { choose, theme } from './theme.svelte';

	/** The owner's menu. It sits at the bottom of the screen in both places (the masthead's foot,
	 * the phone's "More" tab), so it opens upward; `more` adds the Keys link the phone tab bar has
	 * no room for. Outside `more` the panel spans the nearest positioned ancestor (the foot), so
	 * it stays inside the masthead column. */
	let { more = false }: { more?: boolean } = $props();

	let open = $state(false);
	let root: HTMLElement | undefined = $state();

	function outside(e: MouseEvent) {
		if (open && root && !root.contains(e.target as Node)) open = false;
	}
</script>

<svelte:window onclick={outside} onkeydown={(e) => e.key === 'Escape' && (open = false)} />

<div class="menu" class:more bind:this={root}>
	<button class="who" aria-haspopup="menu" aria-expanded={open} onclick={() => (open = !open)}>
		{more ? 'More' : (board.session?.display ?? '…')}
	</button>
	{#if open && board.session}
		<div class="panel" role="menu">
			<p>Signed in as <strong>{board.session.user}</strong>{board.fleet?.admins?.includes(board.session.user) ? ', a workspace admin' : ''}.</p>
			<div class="theme" role="group" aria-label="Theme">
				<span class="kick" aria-hidden="true">Theme</span>
				<div class="seg">
					{#each CHOICES as c (c)}
						<button class="quiet" role="menuitemradio" aria-checked={theme.choice === c} data-theme-choice={c} onclick={() => choose(c)}>{LABELS[c]}</button>
					{/each}
				</div>
			</div>
			{#if more}<a role="menuitem" href="/keys">Keys</a>{/if}
			<form method="post" action="/auth/logout">
				<input type="hidden" name="csrf" value={board.session.csrf} />
				<button class="quiet" role="menuitem">Sign out</button>
			</form>
		</div>
	{/if}
</div>

<style>
	.menu {
		min-width: 0;
	}
	.who {
		display: block;
		max-width: 100%;
		padding: 0;
		border: none;
		background: none;
		color: inherit;
		font: inherit;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.more {
		position: relative;
	}
	.more,
	.more .who {
		width: 100%;
		height: 100%;
	}
	.panel {
		position: absolute;
		left: 0;
		right: 0;
		bottom: calc(100% + 8px);
		z-index: 10;
		padding: 14px;
		background: var(--panel);
		border: 1px solid var(--line);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 12px;
		font: 14px/1.4 var(--sans);
		color: var(--fg);
		text-align: left;
	}
	.more .panel {
		left: auto;
		right: 8px;
		width: 240px;
	}
	.panel p {
		margin: 0;
		color: var(--muted);
	}
	.panel a {
		color: var(--fg);
	}
	.theme {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.kick {
		font: 400 10.5px/1.4 var(--sans);
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.seg {
		display: flex;
		border: 1px solid var(--line);
		border-radius: 6px;
		overflow: hidden;
	}
	.seg button {
		flex: 1;
		min-width: 0;
		padding: 6px 0 7px;
		border: none;
		border-radius: 0;
		font-size: 12.5px;
		color: var(--muted);
	}
	.seg button + button {
		border-left: 1px solid var(--line);
	}
	/* The selection reads like the Feed's filter tabs: the text tone and an accent underline. */
	.seg button[aria-checked='true'] {
		background: var(--soft);
		color: var(--fg);
		text-decoration: underline;
		text-decoration-color: var(--accent);
		text-decoration-thickness: 2px;
		text-underline-offset: 5px;
	}
	.seg button:focus-visible {
		outline-offset: -2px;
	}
	form {
		margin: 0;
	}
	form button {
		width: 100%;
	}
</style>
