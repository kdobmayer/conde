<script lang="ts">
	// Machine: the person's agent machine, from `machine.status`; refetched on the machine and fleet
	// topics, after each sign-in step, and every 30 s while visible (heartbeats announce nothing). A
	// first sign-in lands here. The sign-ins run on the machine through the relay; the code pasted
	// for Claude goes to `machine.relay.code` and is kept nowhere on the page after it is sent.
	import { onMount } from 'svelte';
	import { mutate, rpc, RpcError } from '#lib/api.ts';
	import { board, onBoard } from '#lib/board.svelte.ts';
	import { UNKNOWN, machineView, settle, signInView, skew, type Flow, type Load, type Machine } from '#lib/machine.ts';
	import Disclosure from '#lib/ui/Disclosure.svelte';
	import ErrorLine from '#lib/ui/ErrorLine.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';

	let load = $state<Load>(UNKNOWN);
	const machine = $derived(load.machine);
	const error = $derived(load.error);
	let failedSetup = $state('');
	let busy = $state(false);
	let code = $state('');
	let relayError = $state('');

	async function read(): Promise<void> {
		try {
			const asked = Date.now();
			const m = await rpc<Machine>('machine.status');
			board.skewMs = skew(m.now, asked, Date.now());
			load = settle(load, { machine: m });
		} catch (e) {
			const code = e instanceof RpcError ? e.code : 'internal';
			load = settle(load, { error: { code, message: (e as Error).message } });
		}
	}

	async function setup(): Promise<void> {
		busy = true;
		try {
			load = settle(load, { machine: await mutate<Machine>('machine.setup') });
			failedSetup = '';
		} catch (e) {
			failedSetup = (e as Error).message;
		} finally {
			busy = false;
		}
	}

	/** One sign-in step, then the record as it stands. */
	async function relay(op: string, body: object): Promise<boolean> {
		busy = true;
		try {
			await mutate(op, body);
			relayError = '';
			return true;
		} catch (e) {
			relayError = (e as Error).message;
			return false;
		} finally {
			busy = false;
			void read();
		}
	}

	function start(flow: Flow): void {
		void relay('machine.relay.start', { flow });
	}

	async function sendCode(relayId: string): Promise<void> {
		if (await relay('machine.relay.code', { relay_id: relayId, code: code.trim() })) code = '';
	}

	function cancel(relayId: string): void {
		code = '';
		void relay('machine.relay.cancel', { relay_id: relayId });
	}

	onMount(() => {
		void read();
		const off = onBoard((topics) => {
			if (topics.includes('machine') || topics.includes('fleet')) void read();
		});
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible') void read();
		}, 30_000);
		return () => {
			off();
			clearInterval(timer);
		};
	});

	const now = $derived(board.now + board.skewMs);
	const lastSeen = $derived(
		board.fleet?.hosts.find((h) => h.owner === board.session?.user && h.host === machine?.host_id)?.last_seen ?? null
	);
	const view = $derived(load.known ? machineView(machine, now, lastSeen) : null);
	const signIn = $derived(signInView(machine, now));
</script>

<section class="machine">
	<header class="head">
		<h1>Machine</h1>
		<p class="deck">Your agent’s own machine: where it runs, always on, signed in as you.</p>
	</header>

	<div class="body">
		{#if !view}
			{#if error}
				<ErrorLine {error} retry={() => void read()} />
			{:else}
				<SkeletonRows count={2} height={40} />
			{/if}
		{:else}
			{#if error}
				<ErrorLine {error} retry={() => void read()} />
			{/if}
			<p class="state" data-state={machine?.state ?? 'none'}>{view.headline}</p>
			<p class="detail">{view.detail}</p>
			{#if machine}<p class="host">{machine.host_id}</p>{/if}
			{#if view.claude && !signIn.running}
				<p class="line">Sign in to Claude so your agent can start.</p>
			{/if}
			{#if signIn.running}
				{@const r = signIn.running}
				<div class="relay" data-flow={r.flow}>
					<p class="line">{r.text}</p>
					{#if r.url}
						<p class="link"><a href={r.url} target="_blank" rel="noopener noreferrer">{r.link}</a></p>
					{/if}
					{#if r.userCode}
						<p class="usercode">{r.userCode}</p>
					{/if}
					{#if r.paste}
						<form class="paste" onsubmit={(e) => { e.preventDefault(); void sendCode(r.relay_id); }}>
							<input type="text" bind:value={code} autocomplete="off" autocapitalize="off" spellcheck="false" aria-label="The code from Claude" placeholder="Paste the code" />
							<button type="submit" disabled={busy || code.trim() === ''}>Send code</button>
						</form>
					{/if}
					<button type="button" class="cancel" disabled={busy} onclick={() => cancel(r.relay_id)}>Cancel</button>
				</div>
			{:else if signIn.start.length > 0}
				{#if signIn.outcome}
					<p class="line outcome">{signIn.outcome}</p>
				{/if}
				<div class="flows">
					{#each signIn.start as f (f.flow)}
						<button type="button" disabled={busy} onclick={() => start(f.flow)}>{f.label}</button>
					{/each}
				</div>
			{/if}
			{#if relayError}
				<ErrorLine error={relayError} what="Could not sign in" retry={() => { relayError = ''; void read(); }} />
			{/if}
			{#if view.signIn.length > 0}
				<p class="line">Needs sign-in: {view.signIn.join(', ')}.</p>
			{/if}
			{#if view.setup}
				<button type="button" class="setup" disabled={busy} onclick={() => void setup()}>{view.setup}</button>
			{/if}
			{#if failedSetup}
				<ErrorLine error={failedSetup} what="Could not set up" retry={() => void setup()} />
			{/if}
			{#if machine}
				<div class="hatch">
					<Disclosure label="From a terminal">
						<p>Over the hub VPN, with your Entra account:</p>
						<pre>az ssh vm -g conde-vm -n {machine.host_id}
sudo -u agent tmux -L agent attach -t agent</pre>
						<p>The second line attaches to your agent’s own session; Ctrl-b d detaches. <code>journalctl -u agentvm-firstboot</code> shows how the machine joined the board.</p>
					</Disclosure>
				</div>
			{/if}
		{/if}
	</div>
</section>

<style>
	.head {
		padding: 34px 48px 18px;
		border-bottom: 1px solid var(--line);
	}
	h1 {
		margin: 0;
		font: 600 44px/1 var(--serif);
		letter-spacing: -0.015em;
	}
	.deck {
		margin: 10px 0 4px;
		max-width: 560px;
		font: 17px/1.45 var(--serif);
		color: var(--muted);
	}
	.body {
		padding: 22px 48px 40px;
		max-width: 760px;
	}
	.state {
		margin: 0;
		font: 600 24px/1.25 var(--serif);
	}
	.detail,
	.line {
		margin: 8px 0 0;
		font-size: 15px;
		line-height: 1.5;
		color: var(--muted);
		overflow-wrap: anywhere;
	}
	.host {
		margin: 6px 0 0;
		font-size: 12.5px;
		color: var(--faint);
	}
	.setup {
		margin-top: 18px;
	}
	.relay,
	.flows {
		margin-top: 18px;
	}
	.flows {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
	}
	.link {
		margin: 10px 0 0;
		font-size: 15px;
		overflow-wrap: anywhere;
	}
	.usercode {
		margin: 10px 0 0;
		font: 600 22px/1.2 var(--mono, monospace);
		letter-spacing: 0.06em;
	}
	.paste {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
		margin-top: 12px;
	}
	.paste input {
		flex: 1 1 240px;
		min-width: 0;
	}
	.cancel {
		margin-top: 12px;
	}
	.hatch {
		margin-top: 28px;
	}
	pre {
		overflow-x: auto;
		font-size: 12.5px;
		line-height: 1.5;
	}
	@media (max-width: 1100px) {
		.head,
		.body {
			padding-left: 36px;
			padding-right: 36px;
		}
	}
	@media (max-width: 720px) {
		.head {
			padding: 18px 18px 12px;
		}
		h1 {
			font-size: 30px;
		}
		.deck {
			font-size: 14px;
			margin: 6px 0 2px;
		}
		.body {
			padding: 16px 18px 30px;
		}
	}
</style>
