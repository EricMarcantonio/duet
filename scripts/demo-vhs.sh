#!/usr/bin/env bash
#
# Record the Duet panel being used, grade it into a VHS take, and emit the two
# committed artifacts the README embeds: docs/demo.mp4 and docs/demo.gif.
#
# Pipeline (every stage is re-runnable; nothing is written into docs/ until both
# size limits pass):
#
#   1  preflight   ffmpeg/ffprobe, cliclick, cargo build, screen unlocked, a
#                  screen-capture device available -> actionable message, exit 1
#   2  performance DUET_MOCK=1 DUET_WINDOWED=1 in the background, positioned with
#                  osascript at a fixed rect, driven with cliclick (~34 s)
#   3  capture     ffmpeg avfoundation screen capture (fallback: screencapture -v)
#   4  grade       one documented ffmpeg filter chain (tape bleed, grain,
#                  scanlines, vignette, PLAY badge, running timecode). The two
#                  text elements come from drawtext when this ffmpeg has it, or
#                  from Pillow-rendered PNGs composited with `overlay` when it
#                  does not (see the overlay capability block in preflight)
#   5  encode      docs/demo.mp4 (h264, yuv420p, +faststart) and
#                  docs/demo.gif (palettegen/paletteuse, 13 fps, 640 px wide)
#   6  assert      GIF <= 8 MB, MP4 <= 6 MB, then move both into docs/
#
# Environment:
#   DEMO_DRY_RUN=1     print every command it would run, with the resolved window
#                      rect and click coordinates, then exit 0. Nothing is
#                      launched, recorded or written.
#   DEMO_MOCK=<v>      DUET_MOCK variant to drive (default 1; see src/ui/demo.rs)
#   DEMO_KEEP=1        keep the temp dir (raw capture, filter script, log)
#   DEMO_SKIP_BUILD=1  reuse the existing ./target/debug/duet
#   DEMO_WIN_X/Y       override the window's top-left position in screen points
#   DEMO_APP_NAME      override the process name used with System Events (duet)
#   DEMO_AVFOUNDATION_INPUT
#                      override the avfoundation input, e.g. "1:none"
#   DEMO_ALLOW_NO_BADGE=1
#                      record without the VHS text overlay (PLAY badge + running
#                      timecode) at all.
#   DEMO_FORCE_DRAWTEXT=1
#                      prefer the drawtext overlay path even on a machine where the
#                      filter probe says otherwise. The overlay has two paths:
#                      drawtext (needs an ffmpeg built with libfreetype, which
#                      Homebrew's formula passes) and a Pillow PNG fallback for
#                      builds without it. Forcing drawtext when the filter really is
#                      absent degrades to the fallback with a warning instead of
#                      emitting a filter graph ffmpeg would reject.
#
# Permissions: run this from a terminal that already holds Screen Recording and
# Accessibility permission (System Settings > Privacy & Security). Without them
# clicks are dropped and/or the capture is all black; the script detects a black
# frame and says which permission is missing.
#
# Usage: scripts/demo-vhs.sh
#        DEMO_DRY_RUN=1 scripts/demo-vhs.sh
#
set -euo pipefail

say() { printf '\n== %s\n' "$*"; }
note() { printf '   %s\n' "$*"; }
die() {
    printf '\n!! %s\n' "$*" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# Window geometry — the ONE place to re-tune the choreography
# ---------------------------------------------------------------------------
# src/main.rs opens the window at 400x600 (`with_inner_size(LogicalSize::new(
# 400.0, 600.0))`). Windowed mode is a full-size content view with a hidden
# title bar, so the web view starts at the window's top-left corner: every
# offset below is measured in points from that corner, and every click the
# script issues is `window origin + offset`.
#
# The offsets come from the CSS in assets/main.css:
#   .windowed .header     min-height 46
#   .content              padding-top 2              -> content starts at y = 48
#   .hero                 padding 14; .primary height 38, margin-top 12
#   hero head             eyebrow 13 + title 19/line + sub 16/line
#   .picker               margin-top 10; padding 14 12 10
#   .picker .pick-list    margin-top 8; .pick row height 32, pitch 34
#   .section-row          18 top margin + ~15 + 7 bottom margin
#   .listener             padding 12; .listener-head min-height 40
#   .slider-row.big       margin-top 12; input height 24 (thumb 22)
#
# Two layout states matter, because the hero grows or shrinks by one text line
# and that moves everything under it by ~19 px:
#   state A/B  hero sub or title on ONE line -> pick rows at 242, 276, 310
#                                               (centres 258, 292, 326)
#   state C/D  hero title on TWO lines       -> pick rows at 261, 295, 329
#                                               (centres 277, 311, 345)
# Y_PICK_ROW1/2/3 are the A/B centres, Y_PICK_ROW1_LIVE the C/D centre of row 1.
# A row is 32 px tall, so each click has ~16 px of slack: if one lands on the
# wrong control, move that single offset by +-17 and re-run.
WIN_W=400
WIN_H=600

# Window-relative click coordinates.
X_CTA=200             # hero CTA (full width); its centre
X_PICK_NAME=150       # picker row, over the name (row spans x 24..376)
X_DONE=357            # picker header "Done" link (spans 342..372)
X_SLIDER_FROM=199     # listener volume thumb at 52 % (track x 50..338)
X_SLIDER_TO=268       # ... dragged to ~78 %

Y_CTA_OPEN_PICKER=152 # "Choose headphones": button spans 141..179 when the hero
                      # sub wraps to two lines, 125..163 when it does not
Y_CTA_LISTEN=139      # "Listen together": spans 130..168 with a two-line title,
                      # 111..149 with a one-line title
# With DUET_MOCK=1 the picker sorts connected-first, then by name, so the rows
# are: 1 Alex's AirPod Pro 2's (connected), 2 Alex's AirPods Pro (its connect
# always fails in the mock — never tick it), 3 Sam's AirPods Pro #2 (not
# connected, so ticking it shows "Connecting…" for ~2 s).
Y_PICK_ROW1=258       # first paired pair, before the group exists
Y_PICK_ROW2=292       # second row (kept for re-tuning / extra beats)
Y_PICK_ROW3=326       # third row: the pair that is not connected yet (ticked first)
Y_PICK_ROW1_LIVE=277  # first row once both pairs are connected / group is live
Y_SLIDER=487          # first listener's volume slider, with the picker open
Y_DONE=226            # picker header "Done" link

# ---------------------------------------------------------------------------
# Timing — the performance is ~34 s (the plan asks for ~35 s). These are the only
# knobs for pacing: stretch one to slow the take down, shorten them all if the
# GIF will not fit its budget.
# ---------------------------------------------------------------------------
T_START=3.0      # let the window paint after the capture starts
T_OPEN=2.2       # click "Choose headphones"; the picker drops in
T_CONNECTING=2.0 # hold on the row's "Connecting…" spinner
T_CONNECT=3.5    # the mock's connect takes 2 s (src/ui/demo.rs) + UI settle
T_TOGETHER=5.5   # "Listen together": the Duet output is created, cards slide in
T_DRAG=4.5       # volume drag plus the readout settling
T_UNTICK=4.5     # untick a pair; the group shrinks back to one listener
T_DONE=5.0       # close the picker; the hero settles
T_TAIL=3.0       # hold the finished state before the capture stops

# The recorder starts before the first click, so its spin-up has to be part of
# the capture duration or the tail of the take is cut off.
REC_LEAD=1.5

# ---------------------------------------------------------------------------
# Output / encode settings
# ---------------------------------------------------------------------------
FPS=30
MP4_W=720        # 400x600 -> 720x1080 (aspect kept via scale=-2)
MP4_CRF=26       # lower = bigger; the 6 MB cap is checked before docs/ is written
GIF_FPS=13
GIF_W=640
GIF_COLORS=96    # tape grain is expensive in a palette; 96 keeps it under 8 MB
GIF_DENOISE=3:2:6:6 # hqdn3d luma/chroma spatial+temporal; the GIF's size lever
# Size levers, in the order worth trying when a cap is hit: GIF_COLORS (96 -> 64),
# then the T_* timings (a shorter take), then GIF_W. The grain is the expensive
# part of both formats; the MP4 keeps all of it.
MAX_GIF_BYTES=$((8 * 1024 * 1024))
MAX_MP4_BYTES=$((6 * 1024 * 1024))

# Placement of the two text elements, in output pixels. Shared by both overlay
# paths (drawtext x/y and the `overlay` x/y of the Pillow PNGs) so the two looks
# stay on the same baseline.
BADGE_X=22       # left edge of "PLAY ▶"
BADGE_Y=18       # top edge of "PLAY ▶"
TC_X=22          # left edge of the running timecode
TC_BOTTOM=16     # gap between the timecode's bottom edge and the frame bottom

DRY_RUN=0
if [[ -n "${DEMO_DRY_RUN:-}" && "${DEMO_DRY_RUN}" != "0" ]]; then DRY_RUN=1; fi
KEEP=0
if [[ -n "${DEMO_KEEP:-}" && "${DEMO_KEEP}" != "0" ]]; then KEEP=1; fi

MOCK="${DEMO_MOCK:-1}"
APP_NAME="${DEMO_APP_NAME:-duet}"
WINDOW_TIMEOUT=60

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO_ROOT/target/debug/duet"
DOCS="$REPO_ROOT/docs"

TMP_BASE="${TMPDIR:-/tmp}"
TMP_BASE="${TMP_BASE%/}"
TMP="$(mktemp -d "$TMP_BASE/duet-demo.XXXXXX")"
RAW="$TMP/capture.mkv"          # avfoundation path
RAW_SC="$TMP/capture-sc.mov"    # screencapture -v fallback
GRADE_FILTER="$TMP/grade.filter"
OVERLAY_PY="$TMP/overlay_pngs.py" # Pillow fallback renderer (written at run time)
BADGE_PNG="$TMP/badge-play.png"   # "PLAY ▶" still, overlaid for the whole clip
TC_PATTERN="$TMP/tc_%03d.png"     # one timecode frame per second of footage
MP4_TMP="$TMP/demo.mp4"
GIF_TMP="$TMP/demo.gif"
APP_LOG="$TMP/duet.log"

APP_PID=""
REC_PID=""

# ---------------------------------------------------------------------------
# Command plumbing: every external command goes through `run`, so DEMO_DRY_RUN
# prints the exact, fully-resolved command list in execution order.
# ---------------------------------------------------------------------------
fmt_cmd() { # printable form of argv, faithful enough to copy-paste
    local out=() a esc
    for a in "$@"; do
        # Quote anything a shell would interpret: whitespace, quotes, globs, $.
        if [[ "$a" == *[[:space:]]* || ! "$a" =~ ^[-A-Za-z0-9_@%+=:,./]+$ ]]; then
            esc="${a//\\/\\\\}"   # \        -> \\
            esc="${esc//\"/\\\"}"   # "         -> \"
            esc="${esc//\$/\\\$}"   # $         -> \$
            esc="${esc//\`/\\\`}"   # backtick  -> \`
            out+=("\"$esc\"")
        else
            out+=("$a")
        fi
    done
    printf '%s' "${out[*]}"
}

run() { # log the command, then execute it unless this is a dry run
    printf '   $ %s\n' "$(fmt_cmd "$@")"
    if ((DRY_RUN)); then
        return 0
    fi
    "$@"
}

pause() {
    printf '   … wait %ss\n' "$1"
    if ((DRY_RUN)); then
        return 0
    fi
    sleep "$1"
}

write_text_file() { # path, content
    local path="$1" content="$2"
    if ((DRY_RUN)); then
        printf '   # writes %s:\n' "$path"
        printf '%s\n' "$content" | sed 's/^/   | /'
        return 0
    fi
    printf '%s\n' "$content" >"$path"
}

have() { command -v "$1" >/dev/null 2>&1; }

need() { # command, install hint
    if have "$1"; then
        note "ok: $1 ($(command -v "$1"))"
        return 0
    fi
    if ((DRY_RUN)); then
        note "MISSING (dry run continues): $1 — $2"
        return 0
    fi
    die "$1 not found — install it with: $2"
}

cleanup() { # always stop the recorder and the app, and never leave a temp dir
    local code=$?
    trap - EXIT INT TERM
    if [[ -n "$REC_PID" ]] && kill -0 "$REC_PID" 2>/dev/null; then
        kill "$REC_PID" 2>/dev/null || true
        wait "$REC_PID" 2>/dev/null || true
    fi
    if [[ -n "$APP_PID" ]] && kill -0 "$APP_PID" 2>/dev/null; then
        kill "$APP_PID" 2>/dev/null || true
        wait "$APP_PID" 2>/dev/null || true
    fi
    if ((KEEP)); then
        printf '\n   temp dir kept: %s\n' "$TMP" >&2
    else
        rm -rf "$TMP"
    fi
    exit "$code"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# 1. Preflight — refuse to record garbage
# ---------------------------------------------------------------------------
say "Preflight"
if ((DRY_RUN)); then
    note "DEMO_DRY_RUN=1: printing the plan only, nothing will run"
fi
note "repo: $REPO_ROOT"
note "temp: $TMP"

need ffmpeg "brew install ffmpeg"
need ffprobe "brew install ffmpeg"
need cliclick "brew install cliclick"
need cargo "install Rust from https://rustup.rs (then re-run)"
need osascript "osascript ships with macOS; this script is macOS-only"
need ioreg "ioreg ships with macOS; this script is macOS-only"

if ((DRY_RUN)); then
    note "skip (dry run): cargo build"
elif [[ -n "${DEMO_SKIP_BUILD:-}" ]]; then
    note "skip: DEMO_SKIP_BUILD is set, reusing $BIN"
else
    if ! (cd "$REPO_ROOT" && cargo build); then
        die "cargo build failed — fix the build before recording; nothing was captured"
    fi
    note "ok: cargo build"
fi

if ((!DRY_RUN)); then
    [[ -x "$BIN" ]] || die "no executable at $BIN — run cargo build (or drop DEMO_SKIP_BUILD)"
    mkdir -p "$DOCS"
fi

# A locked screen captures as a black frame; catch it before recording 30 s of
# nothing. `CGSSessionScreenIsLocked` is only present (as <true/>) while locked.
screen_is_locked() {
    ioreg -n Root -d1 -a 2>/dev/null |
        grep -A1 'CGSSessionScreenIsLocked' |
        grep -q '<true/>'
}

if have ioreg; then
    if screen_is_locked; then
        if ((!DRY_RUN)); then
            die "the screen is locked — unlock it and re-run; a locked screen records an all-black frame"
        fi
        note "WARNING: the screen is locked right now (a real run would exit 1)"
    else
        note "ok: screen unlocked"
    fi
fi

# AVFoundation hides its screen-capture device unless the calling process holds
# Screen Recording permission, so never assume index 1: read the device list and
# fall back to `screencapture -v` when no screen device is listed.
detect_screen_input() {
    if [[ -n "${DEMO_AVFOUNDATION_INPUT:-}" ]]; then
        printf '%s' "$DEMO_AVFOUNDATION_INPUT"
        return 0
    fi
    local listing idx
    listing="$(ffmpeg -hide_banner -f avfoundation -list_devices true -i "" 2>&1 || true)"
    idx="$(printf '%s\n' "$listing" |
        sed -n 's/.*\[\([0-9][0-9]*\)\] Capture screen.*/\1/p' |
        head -1)"
    if [[ -n "$idx" ]]; then
        printf '%s:none' "$idx"
    fi
}

CAPTURE_MODE=avfoundation
AV_INPUT=""
if have ffmpeg; then
    AV_INPUT="$(detect_screen_input)"
    if [[ -z "$AV_INPUT" ]]; then
        CAPTURE_MODE=screencapture
        note "avfoundation lists no \"Capture screen\" device (Screen Recording not"
        note "granted to this terminal, or no screen device) -> using the"
        note "screencapture -v fallback. Grant Screen Recording and re-run for the"
        note "preferred path: System Settings > Privacy & Security > Screen Recording."
    else
        note "ok: avfoundation screen input \"$AV_INPUT\""
    fi
else
    AV_INPUT="1:none"
    note "MISSING (dry run continues): ffmpeg — assuming screen input \"1:none\""
fi
if ((DRY_RUN)); then
    note "capture mode: $CAPTURE_MODE"
fi

# Font for the PLAY badge and the timecode: it must carry U+25B6 (BLACK
# RIGHT-POINTING TRIANGLE) for the "▶". First existing candidate wins. Drawtext
# loads it directly; the Pillow fallback hands the same path to ImageFont.
FONT_PATH=""
for cand in \
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf" \
    "/Library/Fonts/Arial Unicode.ttf" \
    "/System/Library/Fonts/Menlo.ttc" \
    "/System/Library/Fonts/Helvetica.ttc"; do
    if [[ -f "$cand" ]]; then
        FONT_PATH="$cand"
        break
    fi
done
if [[ -z "$FONT_PATH" ]]; then
    if ((DRY_RUN)); then
        FONT_PATH="/System/Library/Fonts/Menlo.ttc"
        note "MISSING (dry run continues): an overlay font; assuming $FONT_PATH"
    else
        die "no usable font for the text overlay (looked in /System/Library/Fonts)"
    fi
else
    note "ok: font $FONT_PATH"
fi

# The badge and the running timecode are text, and there are two ways to draw
# them — pick the best one this machine can do:
#
#   drawtext  ffmpeg's own text filter. Needs a build with libfreetype
#             (`--enable-libfreetype`, which Homebrew's formula passes). One
#             filter chain, no extra assets: the preferred path.
#   pil       this ffmpeg has no drawtext (a hand-rolled build may not), so the
#             same two elements are rendered as transparent PNGs with python3 +
#             Pillow and composited with `overlay`. The badge is one still
#             overlaid for the whole clip; the timecode is one small PNG per
#             second, `tc_%03d.png`, stepped at 1 fps. Same text, same colours,
#             same shadow offsets as the drawtext stages.
#   none      DEMO_ALLOW_NO_BADGE: skip the overlay entirely.
#
# Both paths write the same $MP4_TMP/$GIF_TMP and go through the same size caps;
# only the filter graph and the encoder's input list differ.
OVERLAY=""
FORCE_DRAWTEXT=0
if [[ -n "${DEMO_FORCE_DRAWTEXT:-}" && "${DEMO_FORCE_DRAWTEXT}" != "0" ]]; then
    FORCE_DRAWTEXT=1
fi

have_drawtext() { ffmpeg -hide_banner -filters 2>/dev/null | grep -qw drawtext; }
have_pillow() { python3 -c 'import PIL.Image, PIL.ImageDraw, PIL.ImageFont' >/dev/null 2>&1; }

if [[ -n "${DEMO_ALLOW_NO_BADGE:-}" && "${DEMO_ALLOW_NO_BADGE}" != "0" ]]; then
    OVERLAY="none"
    note "ok: overlay path: none (DEMO_ALLOW_NO_BADGE is set, no PLAY badge/timecode)"
else
    if have ffmpeg && have_drawtext; then
        OVERLAY="drawtext"
        note "ok: overlay path: drawtext (this ffmpeg has the drawtext filter)"
    elif ((FORCE_DRAWTEXT)); then
        note "WARNING: DEMO_FORCE_DRAWTEXT is set, but this ffmpeg has no drawtext filter"
        note "         (built without libfreetype). Refusing to emit a filter graph ffmpeg"
        note "         would reject; degrading to the next available overlay path."
    fi
    if [[ -z "$OVERLAY" ]]; then
        if have ffmpeg && have python3 && have_pillow; then
            OVERLAY="pil"
            note "ok: overlay path: pil (no drawtext -> PNG overlay via python3 + Pillow)"
        elif ((DRY_RUN)); then
            OVERLAY="pil"
            note "MISSING (dry run continues): neither drawtext nor python3 + Pillow;"
            note "         assuming the pil overlay path"
        else
            note "this ffmpeg has no drawtext filter (built without libfreetype), and the"
            note "Pillow fallback is unavailable (python3 + PIL.Image)."
            die "cannot draw the PLAY badge and running timecode. Fix with: brew reinstall ffmpeg (its formula enables freetype), or python3 -m pip install Pillow for the fallback, or record without the overlay: DEMO_ALLOW_NO_BADGE=1"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# 2. Fixed window rect, derived from the primary display's size in points
# ---------------------------------------------------------------------------
say "Window rect"
primary_screen_size() { # prints "WIDTH,HEIGHT" in points
    # shellcheck disable=SC2016 # that is a JS template literal, not a shell expansion
    osascript -l JavaScript -e \
        'ObjC.import("AppKit"); const f = $.NSScreen.mainScreen.frame; `${f.size.width},${f.size.height}`' \
        2>/dev/null
}

SCREEN_W=""
SCREEN_H=""
if have osascript; then
    size_raw="$(primary_screen_size || true)"
    case "$size_raw" in
        *,*) SCREEN_W="${size_raw%%,*}"; SCREEN_H="${size_raw##*,}" ;;
    esac
fi
if ! [[ "${SCREEN_W:-}" =~ ^[0-9]+$ && "${SCREEN_H:-}" =~ ^[0-9]+$ ]]; then
    size_raw="$(osascript -e 'tell application "Finder" to get bounds of window of desktop' 2>/dev/null || true)"
    case "$size_raw" in # e.g. "-99, -1080, 1821, 1117"
        *,*,*,*)
            SCREEN_W="$(printf '%s' "$size_raw" | awk -F', *' '{print $3 - $1}')"
            SCREEN_H="$(printf '%s' "$size_raw" | awk -F', *' '{print $4 - $2}')"
            ;;
    esac
fi
if ! [[ "${SCREEN_W:-}" =~ ^[0-9]+$ && "${SCREEN_H:-}" =~ ^[0-9]+$ ]]; then
    if ((DRY_RUN)); then
        SCREEN_W=1728
        SCREEN_H=1117
        note "MISSING (dry run continues): display size; assuming ${SCREEN_W}x${SCREEN_H} points"
    else
        die "could not read the primary display size via osascript (NSScreen or Finder)"
    fi
fi
note "display: ${SCREEN_W}x${SCREEN_H} points"

WX="${DEMO_WIN_X:-$(((SCREEN_W - WIN_W) / 2))}"
WY="${DEMO_WIN_Y:-$(((SCREEN_H - WIN_H) / 2))}"
if ((WX < 0)); then WX=0; fi
if ((WY < 0)); then WY=0; fi
if ((WX + WIN_W > SCREEN_W || WY + WIN_H > SCREEN_H)); then
    die "the ${WIN_W}x${WIN_H} window does not fit on a ${SCREEN_W}x${SCREEN_H} display at ($WX,$WY) — set DEMO_WIN_X / DEMO_WIN_Y"
fi
note "window: ${WIN_W}x${WIN_H} at ($WX,$WY)  [cropped out of a full-screen capture]"
note "clicks (window-relative, x,y):"
note "  picker CTA      $X_CTA,$Y_CTA_OPEN_PICKER      Listen together  $X_CTA,$Y_CTA_LISTEN"
note "  pick rows       $X_PICK_NAME,{$Y_PICK_ROW1|$Y_PICK_ROW2|$Y_PICK_ROW3}  row 1 live  $X_PICK_NAME,$Y_PICK_ROW1_LIVE"
note "  volume slider   $X_SLIDER_FROM,$Y_SLIDER -> $X_SLIDER_TO,$Y_SLIDER        Done  $X_DONE,$Y_DONE"

# Absolute click targets = window origin + the offsets documented above.
abs_x() { printf '%s' "$((WX + $1))"; }
abs_y() { printf '%s' "$((WY + $1))"; }

click() { # window-relative x, y
    local x y
    x="$(abs_x "$1")"
    y="$(abs_y "$2")"
    run cliclick "m:$x,$y"
    run cliclick "w:120" "c:$x,$y"
}

# ---------------------------------------------------------------------------
# 3. Capture duration (derived, so re-tuning a timing cannot desync it)
# ---------------------------------------------------------------------------
PERFORM="$(awk -v a="$T_START" -v b="$T_OPEN" -v c="$T_CONNECTING" -v d="$T_CONNECT" \
    -v e="$T_TOGETHER" -v f="$T_DRAG" -v g="$T_UNTICK" -v h="$T_DONE" -v i="$T_TAIL" \
    'BEGIN { printf "%.1f", a + b + c + d + e + f + g + h + i }')"
TOTAL="$(awk -v p="$PERFORM" -v lead="$REC_LEAD" 'BEGIN { printf "%.1f", p + lead }')"
# screencapture's -V wants whole seconds; ffmpeg's -t takes the exact float.
TOTAL_SC="$(awk -v t="$TOTAL" 'BEGIN { printf "%d", int(t + 0.999) }')"
note "performance: ${PERFORM}s, capture: ${TOTAL}s at ${FPS} fps (${REC_LEAD}s recorder lead-in)"

# ---------------------------------------------------------------------------
# 4. Launch, position, and drive the real UI
# ---------------------------------------------------------------------------
wait_for_window() {
    printf '   … wait up to %ss for the "%s" window\n' "$WINDOW_TIMEOUT" "$APP_NAME"
    if ((DRY_RUN)); then
        return 0
    fi
    local i
    for ((i = 0; i < WINDOW_TIMEOUT * 4; i++)); do
        if osascript -e "tell application \"System Events\" to return (exists window 1 of process \"$APP_NAME\")" 2>/dev/null | grep -q true; then
            return 0
        fi
        sleep 0.25
    done
    die "the Duet window never appeared — check $APP_LOG, and grant this terminal Accessibility permission (System Settings > Privacy & Security > Accessibility)"
}

osascript_sys_events() { # inner AppleScript run against `process "<APP_NAME>"`
    run osascript -e "tell application \"System Events\" to tell process \"$APP_NAME\" to $1"
}

perform() {
    say "Performance (~${PERFORM}s)"
    pause "$T_START"

    # 1. open the picker from the hero CTA ("Choose headphones")
    click "$X_CTA" "$Y_CTA_OPEN_PICKER"
    pause "$T_OPEN"

    # 2. tick the pair that is not connected yet -> the row shows "Connecting…"
    #    (the mock connect sleeps 2 s: src/ui/demo.rs LinkedBluetooth::connect)
    click "$X_PICK_NAME" "$Y_PICK_ROW3"
    pause "$T_CONNECTING"

    # 3. tick the already-connected pair -> two chosen, the CTA becomes usable
    click "$X_PICK_NAME" "$Y_PICK_ROW1"
    pause "$T_CONNECT"

    # 4. Listen together: the "Duet" output is created and the cards appear
    click "$X_CTA" "$Y_CTA_LISTEN"
    pause "$T_TOGETHER"

    # 5. drag the first listener's volume slider from 52 % to ~78 %.
    #    cliclick drags with `dd` (press at) … `du` (release at).
    run cliclick -e 12 "m:$(abs_x "$X_SLIDER_FROM"),$(abs_y "$Y_SLIDER")"
    run cliclick -e 30 "dd:$(abs_x "$X_SLIDER_FROM"),$(abs_y "$Y_SLIDER")" \
        "du:$(abs_x "$X_SLIDER_TO"),$(abs_y "$Y_SLIDER")"
    pause "$T_DRAG"

    # 6. untick the first pair: the group shrinks back to one listener
    click "$X_PICK_NAME" "$Y_PICK_ROW1_LIVE"
    pause "$T_UNTICK"

    # 7. close the picker
    click "$X_DONE" "$Y_DONE"
    pause "$T_TAIL"
}

# ---------------------------------------------------------------------------
# 5. Capture
# ---------------------------------------------------------------------------
capture_ffmpeg() {
    say "Capture (ffmpeg avfoundation, input \"$AV_INPUT\")"
    # Capture the whole screen and crop later: the crop is a fraction of the
    # captured frame, so it follows the display's pixel density (a Retina capture
    # is 2x the point rect) without having to detect that density first.
    # `pad` to even dimensions keeps yuv420p encoding legal on odd screen sizes.
    # `-t` makes ffmpeg stop itself: no signal juggling, no truncated file.
    run ffmpeg -y -hide_banner -loglevel warning \
        -f avfoundation -capture_cursor 1 -framerate "$FPS" -i "$AV_INPUT" \
        -t "$TOTAL" \
        -vf "pad=ceil(iw/2)*2:ceil(ih/2)*2" \
        -c:v libx264 -preset veryfast -crf 16 -pix_fmt yuv420p \
        "$RAW"
}

capture_screencapture() {
    say "Capture (screencapture -v fallback, ${WIN_W}x${WIN_H} at ${WX},${WY})"
    # -v video, -V duration in whole seconds, -C draw the cursor, -R rect in
    # points; the recording is already cropped to the window. It blocks for the
    # whole duration, so it is backgrounded here — as a real child of this shell,
    # so `wait` after the performance really waits for the file to be closed.
    printf '   $ %s\n' "$(fmt_cmd screencapture -v -V "$TOTAL_SC" -C -R"${WX},${WY},${WIN_W},${WIN_H}" "$RAW_SC")"
    if ((DRY_RUN)); then
        return 0
    fi
    screencapture -v -V "$TOTAL_SC" -C -R"${WX},${WY},${WIN_W},${WIN_H}" "$RAW_SC" &
    REC_PID=$!
}

# ---------------------------------------------------------------------------
# 6. Grade: one documented filter chain for the VHS look
# ---------------------------------------------------------------------------
# build_grade_filter prints the graph the MP4 encode consumes: a `-vf` chain for
# the drawtext/none paths, a `-filter_complex` graph for the pil path (which
# needs the extra overlay inputs). `overlay` runs last for the same reason
# drawtext does — the grain and the scanlines must never sit on top of the text.
build_grade_filter() {
    local parts=() graph
    if [[ "$CAPTURE_MODE" == "avfoundation" ]]; then
        # Fraction-of-frame crop == the window rect, whatever the density is.
        parts+=("crop=w=trunc(iw*${WIN_W}/${SCREEN_W}):h=trunc(ih*${WIN_H}/${SCREEN_H}):x=trunc(iw*${WX}/${SCREEN_W}):y=trunc(ih*${WY}/${SCREEN_H})")
    fi
    parts+=("scale=${MP4_W}:-2:flags=lanczos")              # fixed size, even height
    parts+=("fps=${FPS}")
    parts+=("eq=contrast=1.07:brightness=-0.02:saturation=0.72:gamma=1.02") # mild desat + contrast
    parts+=("rgbashift=rh=2:bh=-2")                         # red/blue displaced: tape bleed
    parts+=("chromashift=cbh=-3:crh=3")                     # chroma lags luma: VHS smear
    parts+=("noise=alls=12:allf=t+u")                       # grain, temporal + uniform
    parts+=("drawgrid=w=iw:h=2:t=1:c=black@0.10")           # scanlines every 2 px
    parts+=("vignette=angle=PI/5")                          # dark CRT corners
    if [[ "$OVERLAY" == "drawtext" ]]; then
        # Text last, so the grain and the scanlines never sit on top of it.
        parts+=("drawtext=fontfile='${FONT_PATH}':text='PLAY ▶':fontcolor=white@0.85:fontsize=22:x=${BADGE_X}:y=${BADGE_Y}:shadowcolor=black@0.55:shadowx=2:shadowy=2")
        parts+=("drawtext=fontfile='${FONT_PATH}':text='%{pts\\:hms}':fontcolor=white@0.70:fontsize=16:x=${TC_X}:y=h-th-${TC_BOTTOM}:shadowcolor=black@0.55:shadowx=1:shadowy=1")
    fi
    local IFS=,
    graph="${parts[*]}"
    if [[ "$OVERLAY" == "pil" ]]; then
        # Inputs: 0 the capture, 1 the badge still, 2 the 1 fps timecode sequence.
        #
        # Badge: a plain single-image input, NOT `-loop 1`. A looping second input
        # never ends, so overlay keeps repeating the last graded frame and the
        # encode never terminates. One frame plus `eof_action=repeat` covers the
        # whole clip and still ends with the video (measured: 300 frames in, 300
        # out).
        #
        # Timecode: `shortest=1` because a repeating second input that outlasts the
        # video stretches the output past the real clip length (measured: 331
        # frames out for 300 in). The sequence is generated a couple of frames
        # longer than the expected capture, so it can never be the input that ends
        # first; with `shortest=1` the video alone decides the length.
        graph="[0:v]${graph}[graded]"
        graph+=";[graded][1:v]overlay=x=${BADGE_X}:y=${BADGE_Y}:eof_action=repeat:shortest=0[badged]"
        graph+=";[badged][2:v]overlay=x=${TC_X}:y=main_h-overlay_h-${TC_BOTTOM}:eof_action=repeat:shortest=1[v]"
    fi
    printf '%s' "$graph"
}

build_gif_graph() { # one filter_complex string for palettegen/paletteuse
    printf '%s' "hqdn3d=${GIF_DENOISE},fps=${GIF_FPS},scale=${GIF_W}:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=${GIF_COLORS}:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
}

# ---------------------------------------------------------------------------
# 6b. Overlay assets — the Pillow fallback draws the text as PNGs
# ---------------------------------------------------------------------------
# drawtext needs an ffmpeg built with libfreetype. When that filter is absent the
# same two elements are drawn once with Pillow and composited by `overlay`:
#
#   badge-play.png     "PLAY ▶", one still, overlaid across the whole clip
#   tc_000.png …       the running timecode, one small frame per second, so the
#                      second overlay input, stepped at 1 fps, advances by itself
#
# 1 fps bounds the asset count (~35 frames for a 35 s take) and keeps every PNG a
# few hundred bytes. At fontsize 16 in a 720 px-wide encode the difference from
# 30 fps is invisible, and the `overlay` filter interpolates nothing either way.
write_overlay_py() {
    if ((DRY_RUN)); then
        printf '   # would write %s (the Pillow overlay renderer)\n' "$OVERLAY_PY"
        return 0
    fi
    cat >"$OVERLAY_PY" <<'PY'
#!/usr/bin/env python3
"""Render the VHS text overlay as transparent PNGs.

Used by scripts/demo-vhs.sh when the ffmpeg in PATH was built without
libfreetype, so it has no `drawtext` filter. It draws the same two elements the
drawtext stages draw:

    "PLAY ▶"   white @ 0.85, black shadow @ 0.55 offset (2, 2), 22 px
    timecode   white @ 0.70, black shadow @ 0.55 offset (1, 1), 16 px

The badge is one image; the timecode is one image per second, so ffmpeg can step
them as a 1 fps input. Each PNG is cropped to the text, so the overlay x/y in the
filter graph is the text's own top-left corner.

Usage: overlay_pngs.py FONT BADGE_PNG OUT_DIR TC_COUNT
"""
import os
import sys

from PIL import Image, ImageDraw, ImageFont

BADGE_TEXT = "PLAY \u25b6"
BADGE_FONT_SIZE = 22
BADGE_FILL = (255, 255, 255, 217)     # white @ 0.85
BADGE_SHADOW = (0, 0, 0, 140)         # black @ 0.55
BADGE_SHADOW_OFFSET = (2, 2)

TC_FONT_SIZE = 16
TC_FILL = (255, 255, 255, 179)        # white @ 0.70
TC_SHADOW = (0, 0, 0, 140)            # black @ 0.55
TC_SHADOW_OFFSET = (1, 1)


def render(font_path, text, size, fill, shadow, offset):
    """One tightly cropped RGBA image: shadow underneath, the text on top."""
    font = ImageFont.truetype(font_path, size)
    probe = ImageDraw.Draw(Image.new("RGBA", (1, 1)))
    left, top, right, bottom = probe.textbbox((0, 0), text, font=font)
    image = Image.new(
        "RGBA", (right - left + offset[0], bottom - top + offset[1]), (0, 0, 0, 0)
    )
    draw = ImageDraw.Draw(image)
    # Move the text bbox's own origin to (0, 0), then knock the shadow back by
    # the drawtext shadow offset.
    x, y = -left, -top
    draw.text((x + offset[0], y + offset[1]), text, font=font, fill=shadow)
    draw.text((x, y), text, font=font, fill=fill)
    return image


def timecode(seconds):
    """The shape drawtext's pts:hms format prints: H:MM:SS.mm."""
    return "%d:%02d:%05.2f" % (seconds // 3600, (seconds % 3600) // 60, seconds % 60)


def main(argv):
    if len(argv) != 5:
        sys.stderr.write("usage: overlay_pngs.py FONT BADGE_PNG OUT_DIR TC_COUNT\n")
        return 2
    font_path, badge_path, out_dir, count = (
        argv[1],
        argv[2],
        argv[3],
        int(argv[4]),
    )
    render(
        font_path, BADGE_TEXT, BADGE_FONT_SIZE, BADGE_FILL, BADGE_SHADOW,
        BADGE_SHADOW_OFFSET,
    ).save(badge_path)
    for second in range(count):
        render(
            font_path, timecode(second), TC_FONT_SIZE, TC_FILL, TC_SHADOW,
            TC_SHADOW_OFFSET,
        ).save(os.path.join(out_dir, "tc_%03d.png" % second))
    print("overlay: %s + %d timecode frames in %s" % (badge_path, count, out_dir))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
PY
}

overlay_frame_count() { # seconds of footage -> timecode PNGs to generate
    # One per second of the capture plus a couple of spares, bounded so a
    # pathological TOTAL can never spew hundreds of files. The spares are what
    # make `shortest=1` on the timecode overlay safe: it must never be the input
    # that ends first.
    awk -v t="$TOTAL" 'BEGIN { n = int(t); if (t > n) n++; n += 2; if (n > 120) n = 120; print n }'
}

render_overlay_assets() {
    say "Overlay ($OVERLAY)"
    case "$OVERLAY" in
    drawtext)
        note "drawtext in the grade chain: \"PLAY ▶\" at ${BADGE_X},${BADGE_Y}; timecode at ${TC_X}, bottom-${TC_BOTTOM}"
        ;;
    pil)
        TC_FRAMES="$(overlay_frame_count)"
        note "drawtext is unavailable -> one badge still + ${TC_FRAMES} timecode PNGs at 1 fps"
        write_overlay_py
        run python3 "$OVERLAY_PY" "$FONT_PATH" "$BADGE_PNG" "$TMP" "$TC_FRAMES"
        if ((!DRY_RUN)); then
            [[ -s "$BADGE_PNG" ]] || die "the Pillow overlay produced no badge PNG ($BADGE_PNG)"
            [[ -s "$TMP/tc_000.png" ]] || die "the Pillow overlay produced no timecode PNGs in $TMP"
        fi
        ;;
    none)
        note "DEMO_ALLOW_NO_BADGE: no PLAY badge, no running timecode"
        ;;
    esac
}

encode_mp4() {
    say "Encode MP4"
    local opts=(-an -c:v libx264 -preset slow -crf "$MP4_CRF" -pix_fmt yuv420p
        -movflags +faststart -r "$FPS" "$MP4_TMP")
    if [[ "$OVERLAY" == "pil" ]]; then
        # 0 the capture, 1 the badge still (one frame, repeated by eof_action), 2
        # the 1 fps timecode sequence. Same filters, same output file, same caps
        # as the drawtext path — only the input list and -map differ.
        run ffmpeg -y -hide_banner -loglevel warning \
            -i "$RAW_FILE" \
            -i "$BADGE_PNG" \
            -framerate 1 -start_number 0 -i "$TC_PATTERN" \
            -filter_complex "$GRADE_GRAPH" -map "[v]" "${opts[@]}"
    else
        run ffmpeg -y -hide_banner -loglevel warning -i "$RAW_FILE" \
            -vf "$GRADE_GRAPH" "${opts[@]}"
    fi
}

# ---------------------------------------------------------------------------
# 7. Size assertions — nothing lands in docs/ until both pass
# ---------------------------------------------------------------------------
human_bytes() { awk -v b="$1" 'BEGIN { printf "%.2f MB", b / 1048576 }'; }

assert_under() { # file, limit bytes, published name, retune hint
    local file="$1" limit="$2" label="$3" hint="$4" bytes
    [[ -f "$file" ]] || die "$label was not produced — the encode step failed"
    bytes="$(stat -f%z "$file")"
    note "$label: $bytes bytes ($(human_bytes "$bytes")), limit $limit bytes ($(human_bytes "$limit"))"
    if ((bytes > limit)); then
        die "$label is over its limit ($(human_bytes "$bytes") > $(human_bytes "$limit")) — $hint"
    fi
}

frame_stats() { # file, seek seconds -> "<YAVG> <YMAX>" for one frame
    # YAVG is limited-range luma, so pure black reads 16, not 0: the real panel
    # measures ~55 (dark mode) / ~220 (light mode) with a bright YMAX (~235),
    # while a locked or unpermitted screen is a flat 16/16.
    ffmpeg -hide_banner -v error -ss "$2" -i "$1" -frames:v 1 \
        -vf "signalstats,metadata=print:file=-" -f null - 2>/dev/null |
        awk -F= '/lavfi.signalstats.YAVG/{a=$2} /lavfi.signalstats.YMAX/{m=$2} END{printf "%d %d\n", a, m}'
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
launch_app() {
    printf '   $ %s\n' "$(fmt_cmd env "DUET_MOCK=$MOCK" DUET_WINDOWED=1 \
        "DUET_CONFIG=$TMP/duet-config.json" "DUET_LOG=$APP_LOG" "$BIN")"
    if ((DRY_RUN)); then
        return 0
    fi
    # DUET_MOCK picks the mock variant (src/ui/demo.rs); DUET_WINDOWED forces a
    # normal window instead of the menu-bar popover (src/main.rs); DUET_CONFIG
    # and DUET_LOG keep the run out of the real nickname file and log.
    DUET_MOCK="$MOCK" DUET_WINDOWED=1 \
        DUET_CONFIG="$TMP/duet-config.json" DUET_LOG="$APP_LOG" \
        "$BIN" >"$TMP/app.out" 2>&1 &
    APP_PID=$!
    note "duet pid $APP_PID, log $APP_LOG"
}

say "Launch"
launch_app

wait_for_window

say "Position"
osascript_sys_events "set frontmost to true"
osascript_sys_events "set position of window 1 to {$WX, $WY}"
osascript_sys_events "set size of window 1 to {$WIN_W, $WIN_H}"
pause 1.0

# The capture starts first and stops itself at $TOTAL; the performance plays into
# it. Doing it in that order means the recorder never has to be signalled.
if ((DRY_RUN)); then
    if [[ "$CAPTURE_MODE" == "avfoundation" ]]; then
        capture_ffmpeg
    else
        capture_screencapture
    fi
    pause "$REC_LEAD"
    perform
elif [[ "$CAPTURE_MODE" == "avfoundation" ]]; then
    capture_ffmpeg &
    REC_PID=$!
    pause "$REC_LEAD"
    perform
    if ! wait "$REC_PID"; then
        die "ffmpeg exited non-zero while capturing — see its output above"
    fi
    REC_PID=""
else
    capture_screencapture
    pause "$REC_LEAD"
    perform
    wait "$REC_PID" 2>/dev/null || true
    REC_PID=""
fi

RAW_FILE="$RAW"
if [[ "$CAPTURE_MODE" != "avfoundation" ]]; then
    RAW_FILE="$RAW_SC"
fi

if ((!DRY_RUN)); then
    say "Capture sanity"
    if [[ ! -s "$RAW_FILE" ]]; then
        die "the capture file is empty ($RAW_FILE) — the recorder produced nothing. \`screencapture -v\` and avfoundation both need Screen Recording permission for the terminal that runs this script (System Settings > Privacy & Security > Screen Recording)"
    fi
    duration="$(ffprobe -v error -show_entries format=duration -of default=nw=1:nk=1 "$RAW_FILE" 2>/dev/null || echo 0)"
    note "captured ${duration}s (expected ~${TOTAL}s)"
    if ! awk -v d="$duration" -v want="$TOTAL" 'BEGIN { exit !(d + 0 >= want * 0.8) }'; then
        die "the capture is only ${duration}s of the expected ${TOTAL}s — check Screen Recording permission and re-run"
    fi
    luma="$(frame_stats "$RAW_FILE" 3 || true)"
    luma_avg="${luma%% *}"
    luma_max="${luma##* }"
    note "mid-capture frame: YAVG=${luma_avg:-?} YMAX=${luma_max:-?} (black is 16/16, the real panel ~55/235)"
    if ! awk -v a="${luma_avg:-0}" -v m="${luma_max:-0}" 'BEGIN { exit !(a + 0 >= 24 && m + 0 >= 100) }'; then
        die "the captured frame is essentially black — the screen is locked, or Screen Recording permission is missing (System Settings > Privacy & Security > Screen Recording: enable your terminal, then re-run)"
    fi
fi

say "Grade"
GRADE_GRAPH="$(build_grade_filter)"
# Keep a copy of the chain next to the capture (DEMO_KEEP=1 preserves it) and
# pass it inline: ffmpeg 9 dropped -filter_script, -vf takes the same graph.
write_text_file "$GRADE_FILTER" "$GRADE_GRAPH"

render_overlay_assets

encode_mp4

say "Encode GIF"
run ffmpeg -y -hide_banner -loglevel warning -i "$MP4_TMP" \
    -filter_complex "$(build_gif_graph)" -loop 0 "$GIF_TMP"

if ((DRY_RUN)); then
    say "Assert and publish"
    note "would fail if $GIF_TMP > $MAX_GIF_BYTES bytes (8 MiB)"
    note "would fail if $MP4_TMP > $MAX_MP4_BYTES bytes (6 MiB)"
    run mkdir -p "$DOCS"
    run mv "$MP4_TMP" "$DOCS/demo.mp4"
    run mv "$GIF_TMP" "$DOCS/demo.gif"
    say "Dry run complete"
    note "nothing was launched, recorded or written"
    exit 0
fi

say "Assert and publish"
assert_under "$GIF_TMP" "$MAX_GIF_BYTES" "docs/demo.gif" \
    "lower GIF_COLORS (96), raise the hqdn3d strength in GIF_DENOISE, or shorten the T_* timings"
assert_under "$MP4_TMP" "$MAX_MP4_BYTES" "docs/demo.mp4" \
    "raise MP4_CRF (28), lower MP4_W (600), or shorten the T_* timings"

run mkdir -p "$DOCS"
run mv "$MP4_TMP" "$DOCS/demo.mp4"
run mv "$GIF_TMP" "$DOCS/demo.gif"

say "Done"
note "wrote $DOCS/demo.mp4 ($(human_bytes "$(stat -f%z "$DOCS/demo.mp4")"))"
note "wrote $DOCS/demo.gif ($(human_bytes "$(stat -f%z "$DOCS/demo.gif")"))"
note "watch the take: open $DOCS/demo.mp4"
note "if a click landed on the wrong control, re-tune the X_*/Y_* offsets at the top of this script"
