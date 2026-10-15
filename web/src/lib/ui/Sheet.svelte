<script lang="ts">
	// A modal dialog: a bottom sheet under 720 px (its handle button toggles full height), a
	// right-hand overlay panel from 720 px. Opening moves focus in and makes the page behind inert
	// (showModal); Tab stays inside; Escape, Close and browser back close it, and focus returns
	// to what opened it. Back works through a shallow history entry pushed on open.
	import type { Snippet } from 'svelte';
	import { pushState } from '$app/navigation';
	import { page } from '$app/state';

	let { open = $bindable(false), label, children }: { open?: boolean; label: string; children: Snippet } = $props();

	const id = $props.id();
	let dialog = $state<HTMLDialogElement>();
	let close = $state<HTMLButtonElement>();
	let full = $state(false);
	let opener: HTMLElement | null = null;
	/** Our history entry is the current one. */
	let pushed = false;

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) show(dialog);
		else if (!open && dialog.open) hide(dialog);
	});

	// back (or forward) left our entry: close
	$effect(() => {
		const ours = (page.state as { sheet?: string }).sheet === id;
		if (pushed && !ours) {
			pushed = false;
			open = false;
		}
	});

	function show(d: HTMLDialogElement) {
		opener = document.activeElement as HTMLElement | null;
		full = false;
		d.showModal();
		pushState('', { ...page.state, sheet: id });
		pushed = true;
		close?.focus();
	}

	function hide(d: HTMLDialogElement) {
		d.close();
		opener?.focus();
		opener = null;
	}

	/** Escape and Close: leave our history entry, which closes it; without one, close at once. */
	function dismiss() {
		if (pushed) history.back();
		else open = false;
	}

	function trapTab(e: KeyboardEvent) {
		if (e.key !== 'Tab' || !dialog) return;
		const all = [...dialog.querySelectorAll<HTMLElement>('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])')].filter(
			(el) => !el.hasAttribute('disabled') && el.getClientRects().length > 0
		);
		const first = all[0];
		const last = all[all.length - 1];
		if (!first || !last) return;
		if (e.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) {
			e.preventDefault();
			last.focus();
		} else if (!e.shiftKey && document.activeElement === last) {
			e.preventDefault();
			first.focus();
		}
	}
</script>

<dialog
	bind:this={dialog}
	class="sheet"
	class:full
	aria-labelledby="{id}-title"
	oncancel={(e) => {
		e.preventDefault();
		dismiss();
	}}
	onkeydown={trapTab}
>
	<button type="button" class="handle" aria-label="Full height" aria-expanded={full} onclick={() => (full = !full)}>
		<span aria-hidden="true"></span>
	</button>
	<header>
		<h2 id="{id}-title">{label}</h2>
		<button bind:this={close} type="button" class="quiet" onclick={dismiss}>Close</button>
	</header>
	<div class="body">
		{#if open}{@render children()}{/if}
	</div>
</dialog>

<style>
	.sheet {
		position: fixed;
		inset: auto 0 0 0;
		width: 100%;
		max-width: 100%;
		height: 70dvh;
		max-height: 100dvh;
		margin: 0;
		padding: 0;
		border: 1px solid var(--line);
		border-radius: 12px 12px 0 0;
		background: var(--panel);
		color: var(--fg);
		flex-direction: column;
	}
	.sheet[open] {
		display: flex;
	}
	.sheet.full {
		height: 100dvh;
		border-radius: 0;
	}
	.sheet::backdrop {
		background: #0009;
	}
	.handle {
		align-self: center;
		width: 64px;
		height: 24px;
		padding: 0;
		border: none;
		background: none;
	}
	.handle span {
		display: block;
		width: 40px;
		height: 4px;
		margin: auto;
		border-radius: 2px;
		background: var(--faint);
	}
	header {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 4px 16px 8px;
		border-bottom: 1px solid var(--line);
	}
	h2 {
		flex: 1;
		margin: 0;
		font-size: 1em;
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.body {
		flex: 1;
		overflow-y: auto;
		padding: 12px 16px;
	}
	@media (min-width: 720px) {
		.sheet,
		.sheet.full {
			inset: 0 0 0 auto;
			width: min(500px, 100vw);
			height: 100dvh;
			border-radius: 0;
			border-width: 0 0 0 1px;
		}
		.handle {
			display: none;
		}
		header {
			padding-top: 12px;
		}
	}
</style>
