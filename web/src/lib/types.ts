/** Shapes of the board's answers, as the web ops return them. */

export interface Session {
	user: string;
	display: string;
	csrf: string;
	csrf_header: string;
}

/** A DM conversation of one of the owner's agents. */
export interface RailDm {
	with: string;
	last_seq: number;
	last_at: string;
	/** DMs from the counterpart the agent has not read. */
	unread: number;
}

/** A channel one of the owner's agents belongs to. */
export interface RailChannel {
	channel: string;
	topic: string | null;
	last_seq: number;
	last_at: string | null;
	/** Posts by others past the agent's read position. */
	unread: number;
}

export interface OwnerAgent {
	name: string;
	kind: 'claude' | 'codex' | 'human' | null;
	address: string | null;
	dms: RailDm[];
	channels: RailChannel[];
}

/** Everything the owner's agents can read, per agent. */
export interface Rail {
	owner: string;
	agents: OwnerAgent[];
}

export interface Attachment {
	attachment_id: string;
	filename: string;
	size: number;
}

export interface Reaction {
	emoji: string;
	by: string[];
}

export interface RecipientState {
	recipient: string;
	kind?: string;
	/** queued | sending | retrying | notified | held | delivered | read | unread | denied | refused | gone | sent */
	state: string;
	status?: string;
	attempts?: number;
	retry_at?: string | null;
	last_error?: { status: string; reason: string | null } | null;
	read_at?: string | null;
}

export interface Message {
	message_id: string;
	seq: number;
	place: string;
	from: string;
	to: string;
	date: string;
	reply_to: string | null;
	text: string;
	attachments: Attachment[];
	reactions: Reaction[];
	delivery?: { recipients: RecipientState[]; read_by: { read: number; total: number } | null };
}

export interface History {
	/** The owner's agent whose conversation this is. */
	agent: string;
	channel?: string;
	topic?: string | null;
	with?: string;
	kind?: string | null;
	messages: Message[];
	more: boolean;
}

export interface FleetHost {
	host: string;
	owner: string;
	/** Whether a bridge was ever seen for this host (false for the browser's `web` host). */
	bridge: boolean;
	online: boolean;
	heartbeat: string | null;
	last_seen: string | null;
}

export interface FleetAgent {
	name: string;
	owner: string;
	display: string | null;
	kind: 'claude' | 'codex' | 'human';
	host: string;
	address: string | null;
	pinned: string | null;
	discovered: string | null;
	held: number | null;
	stuck: number | null;
	last_reached: string | null;
	/** Reached at some point; with no `last_reached`, at a time not recorded. */
	ever_reached: boolean | null;
	own: boolean;
}

export interface Fleet {
	now: string;
	online_ttl_s: number;
	hosts: FleetHost[];
	agents: FleetAgent[];
}

export interface ChannelSummary {
	channel: string;
	topic: string | null;
	members: number;
	messages: number;
}

/** A Feed row: the history view with the row's direction and the own agents it is read through. */
export interface FeedMessage extends Message {
	direction: 'in' | 'out' | 'own';
	via: string[];
}

/** `owner.feed`: newest first; `more` when matching rows remain below the last one, inside the bounds. */
export interface FeedAnswer {
	messages: FeedMessage[];
	more: boolean;
}

/** `owner.thread`: the subtree under `top`, sorted by seq; `next` continues it. */
export interface ThreadAnswer {
	top: string;
	start: string | null;
	hidden_parent: boolean;
	messages: Message[];
	next: number | null;
}

export interface SearchHit {
	place: string;
	message_id: string;
	seq: number;
	from: string;
	date: string;
	line: string;
}

/** `owner.search`: hits oldest first; `partial` with `scanned` when the scan budget stopped it. */
export interface SearchAnswer {
	hits: SearchHit[];
	next_cursor: number | string | null;
	partial?: boolean;
	scanned?: number;
}
