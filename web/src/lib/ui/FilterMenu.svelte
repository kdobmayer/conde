<script lang="ts" module>
	export interface Option<T> {
		value: T;
		label: string;
	}
</script>

<script lang="ts" generics="V extends string">
	// A button that opens a listbox of options. On the button Enter, Space and ArrowDown open it
	// at the selected option; in the list the arrows, Home and End move, Enter or Space selects,
	// and Escape closes; both return focus to the button. Tab, or a press outside, closes it.
	import { tick } from 'svelte';

	let {
		label,
		options,
		value = $bindable(),
		onchange
	}: { label: string; options: Option<V>[]; value: V; onchange?: (value: V) => void } = $props();

	const id = $props.id();
	let open = $state(false);
	let active = $state(0);
	let button = $state<HTMLButtonElement>();
	let list = $state<HTMLUListElement>();
	const current = $derived(options.find((o) => o.value === value));

	async function show() {
		active = Math.max(0, options.findIndex((o) => o.value === value));
		open = true;
		await tick();
		list?.focus();
	}

	function hide(refocus: boolean) {
		open = false;
		if (refocus) button?.focus();
	}

	function pick(i: number) {
		const o = options[i];
		if (o && o.value !== value) {
			value = o.value;
			onchange?.(o.value);
		}
		hide(true);
	}

	function onButtonKey(e: KeyboardEvent) {
		if (e.key === 'ArrowDown' || e.key === 'Enter' || e.key === ' ') {
			e.preventDefault();
			void show();
		}
	}

	function onListKey(e: KeyboardEvent) {
		const last = options.length - 1;
		const to: Record<string, () => number> = {
			ArrowDown: () => Math.min(active + 1, last),
			ArrowUp: () => Math.max(active - 1, 0),
			Home: () => 0,
			End: () => last
		};
		if (to[e.key]) {
			e.preventDefault();
			active = to[e.key]!();
			document.getElementById(`${id}-${active}`)?.scrollIntoView({ block: 'nearest' });
		} else if (e.key === 'Enter' || e.key === ' ') {
			e.preventDefault();
			pick(active);
		} else if (e.key === 'Escape') {
			e.preventDefault();
			hide(true);
		} else if (e.key === 'Tab') {
			hide(false);
		}
	}
</script>

<svelte:window
	onpointerdown={(e) => {
		if (open && !(e.target as Element).closest?.(`[data-menu="${id}"]`)) hide(false);
	}}
/>

<div class="menu" data-menu={id}>
	<button
		bind:this={button}
		type="button"
		class="trigger"
		aria-haspopup="listbox"
		aria-expanded={open}
		aria-controls="{id}-list"
		onclick={() => (open ? hide(true) : show())}
		onkeydown={onButtonKey}
	>
		<span class="label">{label}</span>
		<span class="value">{current?.label ?? ''}</span>
		<span class="caret" aria-hidden="true">▾</span>
	</button>
	{#if open}
		<ul
			bind:this={list}
			id="{id}-list"
			class="list"
			role="listbox"
			tabindex="-1"
			aria-label={label}
			aria-activedescendant="{id}-{active}"
			onkeydown={onListKey}
		>
			{#each options as o, i (o.value)}
				<!-- keyboard is handled on the listbox (aria-activedescendant) -->
				<!-- svelte-ignore a11y_click_events_have_key_events -->
				<li
					id="{id}-{i}"
					role="option"
					aria-selected={o.value === value}
					class:active={i === active}
					onclick={() => pick(i)}
					onpointermove={() => (active = i)}
				>
					{o.label}
				</li>
			{/each}
		</ul>
	{/if}
</div>

<style>
	.menu {
		position: relative;
		display: inline-block;
	}
	.trigger {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		min-height: 32px;
		padding: 4px 10px;
		border: 1px solid var(--line);
		background: var(--panel);
		color: var(--fg);
	}
	.label {
		color: var(--muted);
	}
	.caret {
		color: var(--faint);
	}
	.list {
		position: absolute;
		z-index: 20;
		top: calc(100% + 4px);
		left: 0;
		min-width: max(100%, 12em);
		max-height: min(60vh, 320px);
		overflow-y: auto;
		margin: 0;
		padding: 4px;
		list-style: none;
		border: 1px solid var(--line);
		border-radius: 6px;
		background: var(--panel);
	}
	.list:focus-visible {
		outline: 2px solid var(--accent);
	}
	li {
		padding: 6px 10px;
		min-height: 32px;
		border-radius: 4px;
		cursor: pointer;
		white-space: nowrap;
	}
	li.active {
		background: var(--soft);
	}
	li[aria-selected='true'] {
		color: var(--accent);
	}
	li[aria-selected='true']::before {
		content: '✓ ';
	}
</style>
