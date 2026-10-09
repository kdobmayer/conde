<script lang="ts">
	import { fileToBase64, rpc } from './api';
	import { candidates, mentionAt } from './mentions';
	import { changed, short } from './board.svelte';
	import { bytes } from './time';
	import type { Message } from './types';

	/** Where the message goes: an agent's canonical name or a `#channel`. */
	let {
		to,
		replyTo = $bindable(null),
		onsent
	}: { to: string; replyTo?: Message | null; onsent?: (id: string, to: string) => void } = $props();

	const MAX_FILE = 8 * 1024 * 1024;
	let text = $state('');
	let files = $state<File[]>([]);
	let sending = $state(false);
	let error = $state('');
	let area: HTMLTextAreaElement | undefined = $state();
	let picker: HTMLInputElement | undefined = $state();
	let mention = $state<{ start: number; query: string } | null>(null);
	let options = $state<string[]>([]);
	let selected = $state(0);
	const channel = $derived(to.startsWith('#'));
	const coarse = typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;

	function update() {
		if (!area) return;
		area.style.height = 'auto';
		area.style.height = Math.min(area.scrollHeight, 220) + 'px';
		mention = mentionAt(text, area.selectionStart);
		options = mention ? candidates(mention.query, channel) : [];
		selected = 0;
	}

	function pick(name: string) {
		if (!mention || !area) return;
		const before = text.slice(0, mention.start);
		const after = text.slice(area.selectionStart);
		text = `${before}@${name} ${after}`;
		const caret = before.length + name.length + 2;
		mention = null;
		options = [];
		queueMicrotask(() => {
			area?.focus();
			area?.setSelectionRange(caret, caret);
		});
	}

	function keydown(e: KeyboardEvent) {
		if (options.length > 0) {
			if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
				e.preventDefault();
				selected = (selected + (e.key === 'ArrowDown' ? 1 : options.length - 1)) % options.length;
				return;
			}
			if (e.key === 'Enter' || e.key === 'Tab') {
				e.preventDefault();
				pick(options[selected]!);
				return;
			}
			if (e.key === 'Escape') {
				options = [];
				return;
			}
		}
		if (e.key === 'Enter' && !e.shiftKey && !coarse) {
			e.preventDefault();
			void send();
		}
	}

	function addFiles(list: FileList | null) {
		for (const f of list ?? []) {
			if (f.size > MAX_FILE) {
				error = `${f.name} is ${bytes(f.size)}; the limit is 8 MiB per file.`;
				continue;
			}
			files = [...files, f];
		}
		if (picker) picker.value = '';
	}

	async function send() {
		if (sending || (!text.trim() && files.length === 0)) return;
		sending = true;
		error = '';
		try {
			const attachments = await Promise.all(
				files.map(async (f) => ({ filename: f.name, data: await fileToBase64(f) }))
			);
			const body: Record<string, unknown> = {
				to: channel ? { channel: to } : { agent: to },
				text
			};
			if (replyTo) body.reply_to = replyTo.message_id;
			if (attachments.length > 0) body.attachments = attachments;
			const r = await rpc<{ message_id: string; to: string }>('message.send', body);
			text = '';
			files = [];
			replyTo = null;
			queueMicrotask(update);
			changed(['messages', 'delivery']);
			onsent?.(r.message_id, r.to);
		} catch (e) {
			error = (e as Error).message;
		} finally {
			sending = false;
		}
	}
</script>

<form class="send" onsubmit={(e) => (e.preventDefault(), send())}>
	{#if replyTo}
		<div class="reply">
			<span class="muted">Replying to {short(replyTo.from)}:</span>
			<span class="quote">{replyTo.text.split('\n')[0]}</span>
			<button type="button" class="link" onclick={() => (replyTo = null)}>cancel</button>
		</div>
	{/if}
	{#if files.length > 0}
		<ul class="files">
			{#each files as f, i (i)}
				<li>
					{f.name} <span class="muted">{bytes(f.size)}</span>
					<button type="button" class="link" onclick={() => (files = files.filter((_, j) => j !== i))}>remove</button>
				</li>
			{/each}
		</ul>
	{/if}
	{#if options.length > 0}
		<ul class="mentions" role="listbox">
			{#each options as name, i (name)}
				<li role="option" aria-selected={i === selected}>
					<button type="button" class="quiet" class:sel={i === selected} onmousedown={(e) => (e.preventDefault(), pick(name))}>
						@{name}
					</button>
				</li>
			{/each}
		</ul>
	{/if}
	<div class="row">
		<button type="button" class="quiet attach" title="Attach files" onclick={() => picker?.click()}>Attach</button>
		<input bind:this={picker} type="file" multiple hidden onchange={(e) => addFiles(e.currentTarget.files)} />
		<label class="sr-only" for="send-text">Message to {short(to)}</label>
		<textarea
			id="send-text"
			bind:this={area}
			bind:value={text}
			rows="1"
			placeholder={`Message ${short(to)}`}
			oninput={update}
			onclick={update}
			onkeydown={keydown}
		></textarea>
		<button type="submit" disabled={sending || (!text.trim() && files.length === 0)}>{sending ? 'Sending' : 'Send'}</button>
	</div>
	{#if error}<p class="error">{error}</p>{/if}
</form>

<style>
	.send {
		position: relative;
		border-top: 1px solid var(--line);
		padding: 8px 12px calc(8px + env(safe-area-inset-bottom));
		background: var(--panel);
	}
	.row {
		display: flex;
		align-items: flex-end;
		gap: 8px;
	}
	textarea {
		flex: 1;
		resize: none;
		max-height: 220px;
		line-height: 1.4;
	}
	.attach {
		padding: 6px 9px;
	}
	.reply {
		display: flex;
		gap: 6px;
		align-items: baseline;
		font-size: 13px;
		padding: 0 0 6px;
		min-width: 0;
	}
	.quote {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.files {
		list-style: none;
		margin: 0 0 6px;
		padding: 0;
		font-size: 13px;
	}
	.mentions {
		position: absolute;
		bottom: 100%;
		left: 12px;
		z-index: 5;
		list-style: none;
		margin: 0 0 4px;
		padding: 4px;
		background: var(--panel);
		border: 1px solid var(--line);
		border-radius: 8px;
		max-width: calc(100% - 24px);
	}
	.mentions button {
		width: 100%;
		text-align: left;
		border: none;
		padding: 4px 8px;
	}
	.mentions button.sel {
		background: var(--soft);
	}
	.error {
		margin: 6px 0 0;
		font-size: 13px;
	}
</style>
