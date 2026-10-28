<script lang="ts">
	import { board } from './board.svelte';

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
			<p>Signed in as <strong>{board.session.user}</strong>.</p>
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
	form {
		margin: 0;
	}
	form button {
		width: 100%;
	}
</style>
