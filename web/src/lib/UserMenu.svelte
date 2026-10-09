<script lang="ts">
	import { board } from './board.svelte';

	let open = $state(false);
	let root: HTMLElement | undefined = $state();

	function outside(e: MouseEvent) {
		if (open && root && !root.contains(e.target as Node)) open = false;
	}
</script>

<svelte:window onclick={outside} onkeydown={(e) => e.key === 'Escape' && (open = false)} />

<div class="menu" bind:this={root}>
	<button class="quiet who" aria-haspopup="menu" aria-expanded={open} onclick={() => (open = !open)}>
		{board.session?.display ?? '…'}
	</button>
	{#if open && board.session}
		<div class="panel" role="menu">
			<p class="muted small">
				Signed in as <strong>{board.session.user}</strong>{#if board.session.name}; you write as
					<strong>{board.session.name}</strong>{/if}.
			</p>
			<a role="menuitem" href="/keys">Host keys</a>
			<form method="post" action="/auth/logout">
				<input type="hidden" name="csrf" value={board.session.csrf} />
				<button class="quiet" role="menuitem">Sign out</button>
			</form>
		</div>
	{/if}
</div>

<style>
	.menu {
		position: relative;
	}
	.who {
		max-width: 180px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.panel {
		position: absolute;
		right: 0;
		top: calc(100% + 6px);
		z-index: 10;
		width: 240px;
		padding: 12px;
		background: var(--panel);
		border: 1px solid var(--line);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.panel p {
		margin: 0;
	}
	.small {
		font-size: 13px;
	}
	form {
		margin: 0;
	}
	form button {
		width: 100%;
	}
</style>
