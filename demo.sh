#!/bin/sh
# Run the scripted demo, and optionally record it.
#
#   ./demo.sh                    play it
#   ./demo.sh --record           play it and write delta-demo.mov
#   ./demo.sh --record out.mov 40    ...to a name, for that many seconds
#
# The app drives itself: it types into its own composer on a timer rather than
# being fed synthetic keystrokes. That matters because a composing input source
# (Pinyin, Kotoeri, anything) turns fake keypresses into whatever it likes, and
# stealing focus mid-take is rude besides.
set -e
cd "$(dirname "$0")"

APP="target/DeltaMock.app/Contents/MacOS/DeltaMock"
# `./demo.sh v2 ...` plays the proactive script instead of the human one.
SCRIPT=1
if [ "$1" = "v2" ]; then SCRIPT=v2; shift; fi
./bundle.sh --no-open

pkill -f "$APP" 2>/dev/null || true
sleep 1

# Detach stdout/stderr: if the app inherits them, anything piping this script
# waits on the app to exit before it sees EOF.
DELTA_MOCK_DEMO="$SCRIPT" nohup "$APP" >/tmp/delta-mock-demo.log 2>&1 &
PID=$!
sleep 2
osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $PID) to true" >/dev/null 2>&1 || true
sleep 1

if [ "$1" != "--record" ]; then
    echo "demo playing (pid $PID). It runs for about 25s; the window stays up afterwards."
    exit 0
fi

OUT="${2:-delta-demo.mov}"
SECONDS_TO_RECORD="${3:-30}"

# Record just the window, so the rest of the desktop stays out of the take.
RECT=$(osascript -e "tell application \"System Events\" to tell (first process whose unix id is $PID) to get {position, size} of front window" 2>/dev/null | tr -d ' ')
if [ -z "$RECT" ]; then
    echo "could not read the window bounds; recording the whole screen instead" >&2
    screencapture -v -V "$SECONDS_TO_RECORD" "$OUT"
else
    screencapture -v -V "$SECONDS_TO_RECORD" -R"$RECT" "$OUT"
fi

echo "wrote $OUT"
echo "Keep the window frontmost for the whole take -- the region records whatever"
echo "is on screen there, so clicking away mid-recording spoils it."
