/** Shapes of the board's answers, as the web ops return them. */

export interface Session {
	user: string;
	display: string;
	csrf: string;
	csrf_header: string;
	/** The human's canonical name on the board; null when it could not be resolved. */
	name: string | null;
	problem: string | null;
}

export interface RailDm {
	with: string;
	last_seq: number;
	last_at: string;
	unread: number;
}

export interface RailChannel {
	channel: string;
	topic: string | null;
	last_seq: number;
	last_at: string | null;
	unread: number;
}

export interface Rail {
	me: string;
	dms: RailDm[];
	channels: RailChannel[];
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
	me: string;
	channel?: string;
	topic?: string | null;
	read_seq?: number | null;
	with?: string;
	kind?: string | null;
	/** DMs from the partner not read yet. */
	unread?: number;
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
	address: string;
	pinned: string | null;
	discovered: string | null;
	held: number | null;
	stuck: number | null;
	last_reached: string | null;
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
