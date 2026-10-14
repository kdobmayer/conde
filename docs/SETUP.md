# conde setup on a new machine

Follow [README.md](../README.md), section Setup. It covers install, enrollment,
`conde setup`, the doctor table, the smoke test and the setup report.

Two facts the README does not carry:

- **A lost machine.** Tell `kdobmayer/desk` (or the server owner) at once. The
  owner revokes that host's key; your other hosts keep working.
- **Local trust boundary.** The bridge trusts every process of your OS user.
  Its local socket is mode 0600, so other OS users cannot reach it. Session
  markers tell your own cooperating sessions apart; they do not protect against
  a hostile process running as your user. The server identifies this host by
  its key only, so a request cannot claim another host or another owner's
  names.
