#!/usr/bin/env bash
#
# The milestone, live, in three terminals' worth of one script.
#
#   1. node A founds a community with #general
#   2. nodes B and C join it from A's invite
#   3. all three exchange signed messages, peer to peer
#   4. A shuts down; B and C carry on talking
#   5. A comes back and catches up on everything it missed
#
# Nothing here is simulated. Three separate processes, three separate
# databases, real sockets. The only thing they are told about each other is the
# invite string, exactly as a person would paste it into a chat.
#
# Usage:  scripts/demo.sh [--release] [--keep]
#
# Runs on Linux and macOS, and on Windows under Git Bash.

set -euo pipefail

# Job control, so each node's pipeline gets its own process group and can be
# stopped as a unit. Without it a killed node leaves its `tail` behind, and that
# orphan keeps this script's stdout open long after the script has finished.
set -m

# Git Bash rewrites arguments that look like Unix paths, which would turn
# `/create` into `C:/Program Files/Git/create`. This switches that off.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR="$ROOT/.kahui-demo"
PROFILE_ARGS=()
BUILD_DIR="debug"
KEEP=0

for arg in "$@"; do
  case "$arg" in
    --release) PROFILE_ARGS=(--release); BUILD_DIR="release" ;;
    --keep)    KEEP=1 ;;
    -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

# Resolved after the build, not before: on a clean checkout neither candidate
# exists yet, and guessing here picked the wrong one on Linux.
KAHUI=""

resolve_binary() {
  local candidate
  for candidate in "$ROOT/target/$BUILD_DIR/kahui" "$ROOT/target/$BUILD_DIR/kahui.exe"; do
    if [ -x "$candidate" ]; then
      KAHUI="$candidate"
      return 0
    fi
  done
  return 1
}

# The node is a native Windows executable, so under Git Bash it must be handed
# Windows paths. Given a Unix-style "/c/Users/...", Windows reads the leading
# slash as "root of the current drive" and quietly writes somewhere else
# entirely. On Linux and macOS this is the identity function.
winpath() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf %s "$1"
  fi
}

# Fixed ports so a restarted node comes back where its peers last saw it.
PORT_A=47101
PORT_B=47102
PORT_C=47103

bold()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
step()  { printf '\n\033[1;36m── %s\033[0m\n' "$*"; }
note()  { printf '   \033[2m%s\033[0m\n' "$*"; }
fail()  { printf '\n\033[1;31mFAILED: %s\033[0m\n' "$*" >&2; exit 1; }
pass()  { printf '   \033[1;32m✓\033[0m %s\n' "$*"; }

# Stops a node and everything in its pipeline.
kill_group() {
  local pgid="$1"
  # The leading dash targets the process group, so `tail` goes with the node.
  kill -TERM -- "-$pgid" 2>/dev/null || kill -TERM "$pgid" 2>/dev/null || true
}

cleanup() {
  local name pidfile
  for name in a b c; do
    pidfile="$RUN_DIR/$name.pid"
    if [ -f "$pidfile" ]; then
      kill_group "$(cat "$pidfile")"
      rm -f "$pidfile"
    fi
  done
  sleep 0.5
  if [ "$KEEP" -eq 0 ]; then
    # Windows can hold a database file briefly after its process exits.
    rm -rf "$RUN_DIR" 2>/dev/null || { sleep 1; rm -rf "$RUN_DIR" 2>/dev/null || true; }
  else
    note "state kept in $RUN_DIR"
  fi
}
trap cleanup EXIT

# Waits for a pattern to appear in a node's log.
await() {
  local name="$1" pattern="$2" what="$3" timeout="${4:-40}"
  local log="$RUN_DIR/$name.log"
  local deadline=$(( SECONDS + timeout ))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if grep -qF -- "$pattern" "$log" 2>/dev/null; then
      return 0
    fi
    sleep 0.2
  done
  echo "--- last 25 lines of $name.log ---" >&2
  tail -25 "$log" >&2 || true
  fail "timed out after ${timeout}s waiting for $what"
}

# Starts a node. Its stdin is a file we append to, so the script can type at it
# the way a person would.
start_node() {
  local name="$1" display="$2" port="$3"; shift 3
  local cmds="$RUN_DIR/$name.cmds"
  [ -f "$cmds" ] || : > "$cmds"
  local args=( --data-dir "$(winpath "$RUN_DIR/$name")" --name "$display" --port "$port" --no-mdns "$@" )
  # `tail -f` on a file we append to keeps the node's stdin open, so it stays
  # interactive exactly as it would with a person at the keyboard.
  #
  # The redirection wraps the whole subshell, not just the node. Otherwise
  # `tail` inherits this script's stdout and, being immortal, holds it open
  # long after the script itself has finished.
  (
    tail -n +1 -f "$cmds" | "$KAHUI" "${args[@]}"
  ) >> "$RUN_DIR/$name.log" 2>&1 &
  echo $! > "$RUN_DIR/$name.pid"
}

# Types a line at a running node.
say() { printf '%s\n' "$2" >> "$RUN_DIR/$1.cmds"; }

stop_node() {
  local name="$1"
  say "$name" "/quit"
  local pidfile="$RUN_DIR/$name.pid"
  [ -f "$pidfile" ] || return 0
  local pgid; pgid="$(cat "$pidfile")"
  local deadline=$(( SECONDS + 20 ))
  while kill -0 "$pgid" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do
    sleep 0.2
  done
  kill_group "$pgid"
  rm -f "$pidfile"
  # Give Windows a moment to release the database file before it is reopened.
  sleep 1
}

# --------------------------------------------------------------------------

bold "Kahui — a community hosted by its members"

step "Building"
( cd "$ROOT" && cargo build "${PROFILE_ARGS[@]}" -p kahui-cli ) || fail "build failed"
resolve_binary || fail "no kahui binary under $ROOT/target/$BUILD_DIR after building"
note "$KAHUI"

# Leftovers from an interrupted run would hold the log files open on Windows,
# and a stale node would answer sync requests it has no business answering.
if command -v taskkill >/dev/null 2>&1; then
  taskkill //F //IM kahui.exe >/dev/null 2>&1 || true
elif command -v pkill >/dev/null 2>&1; then
  pkill -f "target/.*/kahui" >/dev/null 2>&1 || true
fi
sleep 1

rm -rf "$RUN_DIR" 2>/dev/null || true
mkdir -p "$RUN_DIR"
for name in a b c; do
  : > "$RUN_DIR/$name.log"
done

# 1 -------------------------------------------------------------------------
step "1. Node A founds a community"
start_node a alice "$PORT_A" --exec "/create Aotearoa"
await a "invite: kahui1" "A to mint an invite"
INVITE="$(grep -m1 -o 'kahui1[A-Za-z0-9]*' "$RUN_DIR/a.log")"
pass "community created, #general opened"
note "invite ${INVITE:0:32}… (${#INVITE} chars)"

# 2 -------------------------------------------------------------------------
step "2. Nodes B and C join from that invite"
start_node b bob "$PORT_B" --exec "/join $INVITE"
start_node c carol "$PORT_C" --exec "/join $INVITE"
await b "joined Aotearoa" "B to join" 45
await c "joined Aotearoa" "C to join" 45
pass "B and C fetched and verified the history for themselves"

# 3 -------------------------------------------------------------------------
step "3. All three exchange signed messages"
say a "kia ora koutou"
await b "kia ora koutou" "B to receive A's message"
await c "kia ora koutou" "C to receive A's message"

say b "morena, this is bob"
say c "carol here too"
await a "morena, this is bob" "A to receive B's message"
await a "carol here too" "A to receive C's message"
await c "morena, this is bob" "C to receive B's message"
await b "carol here too" "B to receive C's message"
pass "every message reached every member"

# 3b ------------------------------------------------------------------------
step "3b. B and C form a direct link, not a star around A"

# This is the property the next step depends on. If B and C were only ever
# reachable through A, then "the community survives A leaving" would be an
# accident waiting to be disproved rather than a claim. So check it.
PEER_B="$(grep -m1 'peer id' "$RUN_DIR/b.log" | awk '{print $NF}')"
PEER_C="$(grep -m1 'peer id' "$RUN_DIR/c.log" | awk '{print $NF}')"
[ -n "$PEER_B" ] && [ -n "$PEER_C" ] || fail "could not read B and C peer ids"

linked=0
for _ in $(seq 1 30); do
  say b "/peers"
  say c "/peers"
  sleep 1
  if grep -qF "$PEER_C" "$RUN_DIR/b.log" && grep -qF "$PEER_B" "$RUN_DIR/c.log"; then
    linked=1
    break
  fi
done
[ "$linked" -eq 1 ] || fail "B and C never connected to each other directly"
pass "B and C found each other through presence announcements"


# 4 -------------------------------------------------------------------------
step "4. Node A shuts down; B and C keep talking"
stop_node a
pass "A is gone"

say b "still here without alice"
say c "so am i"
await c "still here without alice" "C to receive B's message with A gone" 45
await b "so am i" "B to receive C's message with A gone" 45
pass "B and C are talking directly — the founder was never load-bearing"

# 5 -------------------------------------------------------------------------
step "5. Node A returns and catches up"
: > "$RUN_DIR/a.cmds"
start_node a alice "$PORT_A"
await a "still here without alice" "A to sync the message from B it missed" 60
await a "so am i" "A to sync the message from C it missed" 60
pass "A recovered both messages sent while it was offline"

say a "what did i miss"
await b "what did i miss" "B to receive A's message after its return" 45
await c "what did i miss" "C to receive A's message after its return" 45
pass "A is a full member again, continuing its own chain"

# 6 -------------------------------------------------------------------------
step "6. Every node independently holds the whole history"

# Note where each log has got to, so the report below shows only the answers to
# the questions asked next, and not messages that happened to arrive earlier.
declare -A MARK
for name in a b c; do
  MARK[$name]="$(wc -l < "$RUN_DIR/$name.log")"
  say "$name" "/status"
  say "$name" "/history"
done
sleep 3

for name in a b c; do
  grep -q " events, 3 members" "$RUN_DIR/$name.log" ||
    fail "node $name does not report three members"
done
pass "all three agree on the membership"

bold "What each node holds, asked of each independently"
TRANSCRIPTS=()
for pair in "a alice" "b bob" "c carol"; do
  set -- $pair
  answer="$(tail -n +"$(( ${MARK[$1]} + 1 ))" "$RUN_DIR/$1.log")"
  printf '\n\033[1m%s\033[0m (%s)\n' "$2" "$RUN_DIR/$1"
  printf '%s\n' "$answer" | grep -E "^  .* events," | tail -1 | sed 's/^/  /'
  transcript="$(printf '%s\n' "$answer" | grep -E "^\[[0-9]{2}:[0-9]{2}:[0-9]{2}\]")"
  printf '%s\n' "$transcript" | sed 's/^/  /'
  TRANSCRIPTS+=("$transcript")
done

# The strong claim is not that each node has the messages, but that all three
# render them in the same order, with nothing coordinating that order.
if [ "${TRANSCRIPTS[0]}" = "${TRANSCRIPTS[1]}" ] && [ "${TRANSCRIPTS[1]}" = "${TRANSCRIPTS[2]}" ]; then
  printf '\n'
  pass "all three rendered an identical transcript"
else
  fail "the three nodes disagree about the order of history"
fi


bold "Milestone complete"
cat <<'SUMMARY'
   A created a community and #general
   B and C joined it
   all three exchanged signed messages peer to peer
   A shut down while B and C kept chatting
   A returned and synchronised what it had missed
   each node persisted its own state locally

   No server was started. No account was created. Nothing outside these
   three directories was contacted.
SUMMARY
