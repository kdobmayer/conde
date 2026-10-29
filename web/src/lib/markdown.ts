/**
 * Message bodies as a strict markdown subset (marked, then DOMPurify on the result). Messages
 * stay plain text everywhere else; this is only how the web app shows them. Rendered:
 * paragraphs, line breaks, bold, italic, inline code, fenced and indented code, lists,
 * blockquotes, https links (bare ones too) and headings as bold paragraphs. Raw HTML, images,
 * tables, rules, setext headings, reference definitions and strikethrough stay text.
 * Qualified agent names, #channels and @mentions are never read as emphasis.
 */

import { Marked, type Token, type Tokens } from 'marked';
import DOMPurify from 'dompurify';

const PART = '[A-Za-z0-9_-]+(?:\\.[A-Za-z0-9_-]+)*';
const NAME = `(?:${PART}\\/${PART}|[#@]${PART})`;
/** A name at the start of the source. */
const NAME_AT = new RegExp(`^${NAME}`);
/** A name not glued to the word or path before it. */
const NAME_IN = new RegExp(`(?<![\\w./:#@-])${NAME}`);
const NAME_ALL = new RegExp(NAME_IN.source, 'g');
const GLUE = /[\w./:#@-]/;

const ESC: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
/** Escapes every `&`, so an entity in the text shows as typed. */
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ESC[c]!);

/** The href when it is an absolute https URL, else null. */
function https(href: string): string | null {
	try {
		const u = new URL(href);
		return u.protocol === 'https:' ? u.href : null;
	} catch {
		return null;
	}
}

const off = () => undefined;

const marked = new Marked({
	gfm: true,
	breaks: true,
	async: false,
	tokenizer: { html: off, tag: off, table: off, hr: off, lheading: off, def: off, del: off },
	hooks: {
		// a `*` or `_` inside a name never closes emphasis opened before it
		emStrongMask: (src: string) => src.replace(NAME_ALL, (m) => m.replace(/[*_~]/g, 'a'))
	},
	extensions: [
		{
			name: 'name',
			level: 'inline',
			start: (src: string) => {
				const i = src.search(NAME_IN);
				return i < 0 ? undefined : i;
			},
			tokenizer(src: string, tokens: Token[]) {
				const before = tokens.at(-1)?.raw.slice(-1) ?? '';
				if (GLUE.test(before)) return undefined;
				const m = NAME_AT.exec(src);
				return m ? { type: 'text', raw: m[0], text: m[0] } : undefined;
			}
		}
	],
	renderer: {
		heading({ tokens }: Tokens.Heading) {
			return `<p><strong>${this.parser.parseInline(tokens)}</strong></p>\n`;
		},
		link({ href, tokens }: Tokens.Link) {
			const text = this.parser.parseInline(tokens);
			const url = https(href);
			return url ? `<a href="${esc(url)}" target="_blank" rel="noopener noreferrer">${text}</a>` : text;
		},
		image({ text }: Tokens.Image) {
			return esc(text);
		},
		checkbox({ checked }: Tokens.Checkbox) {
			return checked ? '[x] ' : '[ ] ';
		},
		html({ text }: Tokens.HTML | Tokens.Tag) {
			return esc(text);
		},
		text(token: Tokens.Text | Tokens.Escape) {
			if ('tokens' in token && token.tokens) return this.parser.parseInline(token.tokens);
			return esc(token.type === 'escape' ? token.text : token.raw);
		}
	}
});

const TAGS = ['p', 'br', 'strong', 'em', 'code', 'pre', 'ul', 'ol', 'li', 'blockquote', 'a'];

type Purifier = Pick<typeof DOMPurify, 'sanitize' | 'isSupported'>;

/** The message body as sanitised HTML. `purify` is DOMPurify bound to a window (tests pass
 * one over jsdom); without a DOM the text comes back escaped, as one paragraph. */
export function renderMarkdown(text: string, purify: Purifier = DOMPurify): string {
	if (!purify.isSupported) return `<p>${esc(text)}</p>`;
	let html: string;
	try {
		html = marked.parse(text) as string;
	} catch {
		return `<p>${esc(text)}</p>`;
	}
	// no whitespace text between blocks: the body is set in pre-wrap, as plain text was
	html = html.replace(/>\n+</g, '><').trim();
	return purify.sanitize(html, {
		ALLOWED_TAGS: TAGS,
		ALLOWED_ATTR: ['href', 'target', 'rel', 'start'],
		ADD_URI_SAFE_ATTR: ['target', 'rel', 'start'],
		ALLOWED_URI_REGEXP: /^https:/i
	});
}

/** The text a block shows, markup removed, one line per block or line break. */
function flat(tokens: Token[] | undefined): string {
	if (!tokens) return '';
	return tokens
		.map((t): string => {
			switch (t.type) {
				case 'space':
					return '';
				case 'br':
					return '\n';
				case 'code':
				case 'codespan':
				case 'escape':
				case 'image':
					return t.text;
				case 'list': {
					// an ordered item keeps its number, so a run-on preview still reads as steps
					const l = t as Tokens.List;
					const first = Number(l.start) || 1;
					return l.items.map((i, k) => (l.ordered ? `${first + k}. ` : '') + flat(i.tokens)).join('\n');
				}
				case 'checkbox':
					return '';
				default:
					return 'tokens' in t && t.tokens ? flat(t.tokens) : t.raw;
			}
		})
		.reduce((out, s, i) => {
			// blocks go on lines of their own
			const block = (t: Token | undefined) => !!t && /^(paragraph|heading|code|list|blockquote|space)$/.test(t.type);
			if (i === 0) return s;
			return out + (block(tokens[i]) || block(tokens[i - 1]) ? '\n' : '') + s;
		}, '')
		.replace(/\n{2,}/g, '\n');
}

/** The message as plain text with the markup removed, for previews (a Feed row, a thread's
 * headline, a reply's first line, a search hit). */
export function plainText(text: string): string {
	try {
		return flat(marked.lexer(text)).trim();
	} catch {
		return text;
	}
}
