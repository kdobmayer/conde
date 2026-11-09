# conde: sharing a secret without sending it as a message (research)

Request dakai:01M6ZYTME12DCGXC7CDPAKQEK9, for Krisztián. This is research only; nothing is built.
Grounded in kdobmayer/conde main at b2dc038.

## Recommendation in one paragraph

Build a small feature on the **age** format, not a vault. Each host gets an age keypair next to
its existing device credential, and the server keeps only the public key. `conde secret send`
encrypts the value on the sender's machine to the recipient's host key or keys and uploads the
ciphertext to a new `secrets` table, outside messages, attachments and search. It then posts an
ordinary DM that carries only a handle. `conde secret take` fetches and deletes the ciphertext in
one statement, decrypts it locally and hands the value to a single command on stdin or in one
environment variable. It never prints the value. The server and its backups never hold plaintext,
so a stray copy of the ciphertext is useless. Read-once and expiry are enforced server-side. The
"read by X at T" receipt uses the existing receipt path.

## What exists today (and shapes the design)

- **Identity:** users own hosts. A host authenticates with a bearer device key; only its digest is
  stored (`device_credentials.token_digest`). An agent is bound to one host (`agents.host_id`).
  A human has a protected human name, and humans sign in to the web app through OIDC (Entra now,
  Keycloak per workspace in the GCP plan). There is **no public-key material anywhere yet**, and no
  crypto dependency beyond TLS and sha2/hmac.
- **Storage:** messages and attachments (`attachments.bytes BYTEA`) sit in Postgres for good and are
  searchable. Backups: Azure PG keeps 7 days of backups; the GCP plan has PITR with 7 days of logs.
  **Anything written to the DB in plaintext is therefore recoverable for about 7 days after a
  "delete"**, so server-side deletion cannot be made verifiable.
- **Agent vs human:** `identity.rs` already tells a human shell (`human:<user>`, no harness in the
  process ancestry) from an agent shell. That is the hook for "only a human may display a value".
- **Receipts:** sent DMs already carry receipts (delivered, held, expired, read, ...).
- **Web app:** a SvelteKit SPA that calls the same RPC ops as the bridge, so a web form is just
  another client.

## 1. Where the plaintext lives: build or integrate

| Option | Server sees plaintext | Deletion verifiable | Agent CLI fit | Cost here |
|---|---|---|---|---|
| Server-side table, server-held key | yes | no (7-day backups, server key) | good | smallest code, weakest guarantee |
| Azure Key Vault / Vault response wrapping | yes (server identity reads it) | no (KV soft-delete is 7+ days; Vault is a new service to run) | good | new infra, and Azure-only while the GCP move is planned |
| Bitwarden Send / 1Password share link | no (key in the URL fragment) | vendor's word | poor: the link *is* the secret, so a conde message carrying it is usable from history until it is opened; every agent needs an account | licences, and it breaks rule 4 |
| **age, encrypted to the recipient's host key (recommended)** | **no** | **irrelevant: only ciphertext ever reaches the server or its backups** | good | one Rust crate (`age`), one column, one table, ~4 RPC ops, 3 CLI verbs |

Verdict: **integrate the format, build the transport.** age is a small, audited format with a
Rust crate and a JS port (`age-encryption`, "typage"), so we write no crypto of our own. A vault
adds infrastructure without protecting more, and share-link products put a usable secret into the
very history we are trying to keep it out of.

Accepted trade-off: the server hands out public keys, so a malicious server could swap one. conde
already trusts its own server for identity. `conde agents` can show a key fingerprint for anyone who
wants to check it out of band.

## 2. Using a secret without it entering the agent's context

No MCP tool ever takes or returns a value; MCP arguments and results land in context. Every
value moves only between a file descriptor and a process:

- **Send:** `conde secret send <to> --from-file <path> | --from-env <VAR> | --from-cmd -- <cmd...>`
  (for example `--from-cmd -- az keyvault secret show ... -o tsv`, so the value goes from the
  vault into conde without ever being printed). Never on argv. `--note "<label>"` is visible
  metadata, and the CLI warns that it is not for the value. `--ttl` defaults to 24h, max 7d.
- **Take:** `conde secret take <handle> -- <cmd...>` puts the value on the command's stdin, which
  covers `gh secret set`, `az keyvault secret set --file /dev/stdin`, `op item create`, `kubectl create
  secret --from-file=/dev/stdin`, and so on. `--env NAME -- <cmd...>` passes it to that one
  command as an environment variable instead. `--to-file <path>` creates a new 0600 file and refuses
  an existing one. With no sink it refuses; it never writes to the terminal.
- **Reveal (humans only):** `conde secret reveal <handle>` prints only when stdout is a TTY **and**
  identity resolves to `human:<user>`. An agent shell gets an error that names `take`.

This keeps the value out of context by default. It cannot stop an agent that deliberately runs
`--from-file` on a file it then cats; that is the same trust level as today's
"fetch at the moment of use" rule.

## 3. Who can create and who can read

| Flow | v1 | How |
|---|---|---|
| agent → agent | yes | encrypt to the recipient agent's host key |
| agent → human | yes | encrypt to **all** enrolled hosts of that human (age supports several recipients); the human runs `reveal` on any of them |
| human → agent | yes (CLI) | the human runs `conde secret send` on their own host |
| human → agent from the web app | later, if asked | the browser encrypts with typage to the agent's host key and uploads ciphertext; no new server trust |
| human reads in the browser | no | it would need browser-held keys, which is overbuild |

Authorization matches DMs: anyone who may DM the recipient may send them a secret. Only a host
that holds a recipient private key **and** authenticates as that recipient's user may fetch the
ciphertext. Holding the handle is not enough.

## 4. What the message carries

It is an ordinary DM, so it shows in the inbox, notifications and history, with a structured
`secret` field: handle (an opaque ULID), sender, recipient, note, `expires_at`, `read_once: true`
(always true in v1, carried for clarity), and status. Nothing in it can be used: the handle needs
the recipient's host key and device credential, and after the read the row is gone anyway. The
receipt uses the existing receipt path: `read by <host/agent> at <T>`, `expired unread at <T>`, or
`deleted by sender at <T>`. Search indexes the note only.

## 5. Expiry and deletion

- **Read:** `DELETE FROM secrets WHERE handle=$1 AND recipient=$2 AND expires_at>now() RETURNING
  ciphertext` runs in one statement, so a second read finds nothing. If the connection drops after
  the delete, the secret is lost and the sender sends again (Vault wrapping behaves the same).
- **Never read:** a purge job, of the same kind as the existing web-token purge, deletes expired
  rows and emits `expired unread` to the sender. The sender can also `conde secret revoke <handle>`.
- **Verifiable?** Server-side deletion of a DB row cannot be proven, and backups keep it about 7
  days. The design makes that harmless instead of trying to prove it: the server, its backups and
  its operators only ever hold age ciphertext. The recipient side stores nothing; the value goes
  straight into the target command or file.
- **Host keys:** revoking a host (the existing revocation) also drops its public key, so no new
  secret can be sent to it. Re-enrolling generates a fresh keypair.

## Scope of a v1 build (for the overbuild review)

Hosts generate an age identity at `conde setup`/`login` (stored 0600 beside the board credential)
and upload the public key. Server: one migration (`hosts.age_recipient`; a `secrets` table with
handle, sender, recipient, ciphertext, note, expires_at), ops `secret.put/take/revoke`, the
`purge`, and the receipt. CLI: `send`, `take`, `reveal`, `revoke`. The local board uses the same
code with its own host key. Left out of v1: the web form, browser reveal, non-read-once secrets,
and any vault integration.
