#!/usr/bin/env bash
# Tests praxia-connect and its units under a real user systemd manager, against local fakes: the metadata service
# and Praxia (one Python server), `claude`, and the agent session (a stub claude-agent.service). Run as an ordinary
# user on a Linux box with systemd, jq, curl and python3 (a lane VM), never on an agent VM: it installs its own
# units into ~/.config/systemd/user and removes them at the end. Prints one line per check; exits 1 on a failure.
# The tokens are fixtures, compared silently, never printed.
set -uo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d)
U="$HOME/.config/systemd/user"
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
fails=0

cleanup() {
  systemctl --user stop praxia-connect.timer praxia-connect.service claude-agent.service 2>/dev/null
  systemctl --user disable praxia-connect.timer 2>/dev/null
  rm -rf "$U/praxia-connect.service" "$U/praxia-connect.service.d" "$U/praxia-connect.timer" \
    "$U/praxia-connect.timer.d" "$U/claude-agent.service"
  systemctl --user daemon-reload
  [ -n "${fake:-}" ] && kill "$fake" 2>/dev/null
  rm -rf "$T"
}
trap cleanup EXIT

ok() { echo "ok   $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }
check() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }

mkdir -p "$T/bin" "$T/home" "$U"
echo ok > "$T/mode"

# --- the fake metadata service and Praxia: Praxia mints pxm_test_<n>, and each mint revokes the one before
cat > "$T/fake.py" <<'PY'
import http.server, json, os, sys, time
T, port = sys.argv[1], int(sys.argv[2])
state = {"imds": 0, "mints": 0, "live": None}
def save():
    with open(f"{T}/state.json", "w") as f: json.dump(state, f)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body):
        b = json.dumps(body).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path.startswith("/metadata/identity/oauth2/token") and self.headers.get("Metadata") == "true" \
                and "resource=connect-app" in self.path:
            state["imds"] += 1; save(); return self.send(200, {"access_token": f"imds-{state['imds']}"})
        self.send(404, {})
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0); self.rfile.read(n)
        if self.path != "/api/agent/connect" or not (self.headers.get("Authorization") or "").startswith("Bearer imds-"):
            return self.send(401, {"error": "not_a_machine"})
        mode = open(f"{T}/mode").read().strip()
        if mode == "hang": time.sleep(120)
        if mode != "ok": return self.send(int(mode), {"error": "fake"})
        state["mints"] += 1; state["live"] = f"pxm_test_{state['mints']}"; save()
        self.send(201, {"token": state["live"], "name": "vm-agent-test", "id": "00000000-0000-4000-8000-000000000001"})
save()
http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
PY
python3 "$T/fake.py" "$T" "$PORT" & fake=$!

# --- `claude`: the MCP entry in ~/.claude.json, the sign-in from a flag; records subcommands only
cat > "$T/bin/claude" <<EOF
#!/usr/bin/env bash
T=$T
EOF
cat >> "$T/bin/claude" <<'EOF'
f="$HOME/.claude.json"; [ -f "$f" ] || echo '{}' > "$f"
echo "$1 $2" >> "$T/claude-calls"
case "$1 $2" in
  "mcp remove") [ -e "$T/slow-remove" ] && sleep 30
                jq 'del(.mcpServers.praxia)' "$f" > "$f.n" && mv "$f.n" "$f" ;;
  "mcp add") [ -e "$T/fail-add" ] && exit 3
             url=""; hdr=""
             while [ $# -gt 0 ]; do case "$1" in --header) hdr=$2; shift 2 ;; http*) url=$1; shift ;; *) shift ;; esac; done
             jq --arg u "$url" --arg h "${hdr#Authorization: }" '.mcpServers.praxia = {type: "http", url: $u, headers: {Authorization: $h}}' \
               "$f" > "$f.n" && mv "$f.n" "$f" ;;
  "mcp get") jq -e '.mcpServers.praxia' "$f" > /dev/null ;;
  "auth status") if [ -e "$T/signed-in" ]; then echo '{"loggedIn": true}'; else echo '{"loggedIn": false}'; fi ;;
esac
EOF
# --- the agent session: asks for a reconnect from inside its own unit when told to, as the agent would
cat > "$T/bin/agent-loop" <<EOF
#!/usr/bin/env bash
while sleep 0.2; do
  if [ -e "$T/do-again" ]; then
    rm -f "$T/do-again"; echo "\$INVOCATION_ID" > "$T/agent-asked-with"
    "$here/praxia-connect" --again
  fi
done
EOF
chmod +x "$T/bin/claude" "$T/bin/agent-loop"
cat > "$U/claude-agent.service" <<EOF
[Service]
Environment=HOME=$T/home
ExecStartPre=/bin/sh -c '! test -e $T/fail-agent'
ExecStart=$T/bin/agent-loop
EOF
cp "$here/praxia-connect.service" "$here/praxia-connect.timer" "$U/"
mkdir -p "$U/praxia-connect.service.d" "$U/praxia-connect.timer.d"
cat > "$U/praxia-connect.service.d/test.conf" <<EOF
[Service]
ExecStart=
ExecStart=$here/praxia-connect --run
Environment=HOME=$T/home PATH=$T/bin:/usr/local/bin:/usr/bin:/bin AGENTVM_CONF=$T/agentvm.conf AGENTVM_IMDS=http://127.0.0.1:$PORT/metadata
EOF
# the timer as shipped, but retrying every 3 s instead of 10 min, to the second (systemd's default accuracy is a
# minute, which the shipped 10-minute retry does not mind)
cat > "$U/praxia-connect.timer.d/test.conf" <<'EOF'
[Timer]
OnUnitInactiveSec=
OnUnitInactiveSec=3s
AccuracySec=1s
EOF
conf() { printf 'PRAXIA_URL=http://127.0.0.1:%s\nCONNECT_RESOURCE=connect-app\n' "$PORT" > "$T/agentvm.conf"; }
conf
systemctl --user daemon-reload
for _ in $(seq 50); do curl -s "http://127.0.0.1:$PORT/" >/dev/null && break; sleep 0.1; done

H="$T/home"; M="$H/.config/agentvm/praxia-connected"; R="$H/.config/agentvm/praxia-reconnect"
mints() { jq -r .mints "$T/state.json"; }
installed_is_live() {
  [ "$(jq -r '.mcpServers.praxia.headers.Authorization' "$H/.claude.json" 2>/dev/null)" = "Bearer $(jq -r .live "$T/state.json")" ]
}
run() { systemctl --user start praxia-connect.service 2>/dev/null; }
agent_inv() { systemctl --user show -p InvocationID --value claude-agent.service; }
# waits up to $1 seconds for the command after it, evaluated anew each second
wait_for() { local n=$1; shift; while [ "$n" -gt 0 ]; do "$@" && return 0; sleep 1; n=$((n - 1)); done; return 1; }
restarted() { [ "$(agent_inv)" != "$inv" ]; }
marker_changed() { [ -e "$M" ] && [ "$(cat "$M")" != "$old_marker" ]; }
journal_clean() { ! journalctl --user -u praxia-connect.service --no-pager 2>/dev/null | grep -qE 'pxm_test_|imds-[0-9]'; }
mode0600() { [ "$(stat -c %a "$H/.claude.json")" = 600 ]; }
no_marker() { [ ! -e "$M" ]; }
timer_until_marker() {
  systemctl --user start praxia-connect.timer
  wait_for 30 test -e "$M"; local r=$?
  if [ $r -ne 0 ]; then
    systemctl --user list-timers praxia-connect.timer --all --no-pager | sed 's/^/     /'
    journalctl --user -u praxia-connect.service -u praxia-connect.timer -n 8 --no-pager -o short-precise | sed 's/^/     /'
    systemctl --user status praxia-connect.service --no-pager 2>&1 | grep -E 'Active|└|├' | sed 's/^/     /'
  fi
  systemctl --user stop praxia-connect.timer
  return $r
}

echo "== 1. a 201 installs the entry"
check "1 the run succeeds" run
check "1 the installed token is Praxia's live one" installed_is_live
check "1 .claude.json is 0600" mode0600
check "1 the marker is written" test -e "$M"
check "1 the journal holds no token" journal_clean

echo "== 5. connected, no request: nothing is minted"
before=$(mints)
check "5 the run succeeds" run
check "5 no new mint" test "$(mints)" = "$before"

echo "== 8. not signed in: the restart is skipped, and the next run mints nothing"
systemctl --user start claude-agent.service; inv=$(agent_inv)
"$here/praxia-connect" --again; wait_for 15 test ! -e "$R"; wait_for 15 test -e "$M"
check "8 connected again" installed_is_live
check "8 the session was not restarted" test "$(agent_inv)" = "$inv"
before=$(mints); run
check "8 the next run mints nothing" test "$(mints)" = "$before"

echo "== 3. a reconnect asked from inside the agent's own unit"
touch "$T/signed-in"; inv=$(agent_inv); before=$(mints); old_marker=$(cat "$M")
sleep 1.1; asked_at=$(date +%s); touch "$T/do-again"
check "3 the service ran and the session restarted" wait_for 20 restarted
check "3 the marker was written after it" wait_for 10 marker_changed
check "3 the agent asked with its own unit's INVOCATION_ID" test "$(cat "$T/agent-asked-with")" = "$inv"
worked() { journalctl --user -u praxia-connect.service --since "@$asked_at" --no-pager -o cat | grep -q 'restarted to load'; }
check "3 the work ran in praxia-connect.service, not in the agent's unit" worked
check "3 one new mint, installed" test "$(mints)" = $((before + 1))
check "3 the installed token is the live one" installed_is_live

echo "== 2. faults, each recovered by the timer"
touch "$R"; echo 500 > "$T/mode"; run
check "2a connect 500: no marker" no_marker
echo ok > "$T/mode"
check "2a the timer recovers" timer_until_marker
check "2a installed is live" installed_is_live

touch "$R" "$T/slow-remove"; : > "$T/claude-calls"; before=$(mints)
systemctl --user start --no-block praxia-connect.service
wait_for 20 grep -q "mcp remove" "$T/claude-calls"
systemctl --user kill --signal=KILL praxia-connect.service; sleep 1
check "2b killed after the 201: minted, no marker" test "$(mints)" = $((before + 1)) -a ! -e "$M"
rm -f "$T/slow-remove"
check "2b the timer recovers" timer_until_marker
check "2b installed is live" installed_is_live

touch "$R" "$T/fail-add"; run
check "2c mcp add failing: no marker" no_marker
rm -f "$T/fail-add"
check "2c the timer recovers" timer_until_marker
check "2c installed is live" installed_is_live

touch "$R" "$T/fail-agent"; run
check "2d the restart failing: no marker" no_marker
rm -f "$T/fail-agent"; systemctl --user reset-failed claude-agent.service 2>/dev/null
systemctl --user start claude-agent.service; inv=$(agent_inv)
check "2d the timer recovers" timer_until_marker
check "2d and restarts the running session" restarted
check "2d the session is up again" systemctl --user is-active --quiet claude-agent.service
check "2d installed is live" installed_is_live

echo "== 4. a timer run and a reconnect at once"
# --no-block returns at once: the run and the request race
systemctl --user start --no-block praxia-connect.service; "$here/praxia-connect" --again
wait_for 30 sh -c "! systemctl --user is-active --quiet praxia-connect.service && test ! -e '$R' && test -e '$M'"
sleep 1
check "4 one installed token, the live one" installed_is_live

echo "== 6. start-up: a failed first attempt is recovered by the started timer, in the same boot"
rm -f "$M"; echo 503 > "$T/mode"; inv=$(agent_inv)
run
check "6 the first attempt fails" no_marker
systemctl --user enable --now praxia-connect.timer 2>/dev/null
check "6 the timer is active, not only enabled" systemctl --user is-active --quiet praxia-connect.timer
echo ok > "$T/mode"
check "6 the timer's run connects" wait_for 30 test -e "$M"
check "6 and restarts the session" restarted
systemctl --user disable --now praxia-connect.timer 2>/dev/null

echo "== 7. no PRAXIA_URL: nothing is written"
rm -f "$M"; touch "$R"; before=$(mints); cp "$H/.claude.json" "$T/before.json"
printf 'CONNECT_RESOURCE=connect-app\n' > "$T/agentvm.conf"
check "7 the run succeeds" run
check "7 no mint, no marker, the config untouched" \
  sh -c "test '$(mints)' = '$before' -a ! -e '$M' && cmp -s '$T/before.json' '$H/.claude.json'"
conf

echo "== 8b. a hanging Praxia ends the run in about a minute"
echo hang > "$T/mode"; touch "$R"; t0=$(date +%s); run; t=$(( $(date +%s) - t0 ))
check "8b the run ended after ${t}s (55..75), failed" sh -c "[ $t -ge 55 ] && [ $t -le 75 ] && test ! -e '$M'"
echo ok > "$T/mode"

check "the journal never held a token" journal_clean
echo "== $fails failure(s)"
[ "$fails" -eq 0 ]
