#!/usr/bin/env bash
# Starts the program in a real window, has it compare two files, three files and two folders,
# and checks what each window showed:
#
#   scripts/gui-smoke.sh PATH/TO/oxidemerge [ARTIFACT_DIR]
#
# The program writes what it shows to the file named by OXIDEMERGE_SMOKE_REPORT and closes
# itself. On Linux without a display, the checks run on an Xvfb display of their own when Xvfb
# is installed. A window that cannot be made for lack of a display or of OpenGL is reported as
# such and does not fail the checks, unless GUI_SMOKE_STRICT=1. Logs, reports and, when a window
# hangs, a screenshot are left in ARTIFACT_DIR (default: a temporary folder, removed on success).
set -euo pipefail

bin=${1:?usage: gui-smoke.sh PATH/TO/oxidemerge [ARTIFACT_DIR]}
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
work=$(mktemp -d)
artifacts=${2:-$work/artifacts}
mkdir -p "$artifacts"
artifacts=$(cd "$artifacts" && pwd)
failed=0
xvfb=
cleanup() {
  local status=$?
  [ -z "$xvfb" ] || kill "$xvfb" 2>/dev/null || true
  [ "$failed$status" != 00 ] || rm -rf "$work"
}
trap cleanup EXIT

if [ "$(uname -s)" = Linux ] && [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
  if ! command -v Xvfb >/dev/null; then
    echo "gui-smoke: skipped, no display and no Xvfb"
    exit 0
  fi
  Xvfb -displayfd 3 -screen 0 1600x1000x24 -nolisten tcp 3>"$work/display" 2>"$artifacts/xvfb.log" &
  xvfb=$!
  for _ in $(seq 50); do
    [ -s "$work/display" ] && break
    sleep 0.2
  done
  [ -s "$work/display" ] || { echo "gui-smoke: Xvfb did not start"; cat "$artifacts/xvfb.log"; exit 1; }
  export DISPLAY=:$(cat "$work/display")
fi

cd "$work"
mkdir -p files left right
printf 'one\ntwo\nthree\n' > files/a.txt
printf 'one\n2\nthree\n' > files/b.txt
printf 'a\nleft\nc\n' > files/left.txt
printf 'a\nb\nc\n' > files/base.txt
printf 'a\nright\nc\n' > files/right.txt
printf 'same\n' > left/same.txt
printf 'same\n' > right/same.txt
printf 'left\n' > left/changed.txt
printf 'right\n' > right/changed.txt
export OXIDEMERGE_CONFIG_DIR=$work/config

screenshot() {
  local png=$artifacts/$1.png
  case "$(uname -s)" in
    Darwin) screencapture -x "$png" 2>/dev/null || true ;;
    Linux) command -v import >/dev/null && import -window root "$png" 2>/dev/null || true ;;
    MINGW* | MSYS* | CYGWIN*)
      powershell -NoProfile -Command "
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        \$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
        \$i = New-Object System.Drawing.Bitmap \$b.Width, \$b.Height
        [System.Drawing.Graphics]::FromImage(\$i).CopyFromScreen(\$b.Left, \$b.Top, 0, 0, \$i.Size)
        \$i.Save('$(cygpath -w "$png" 2>/dev/null || echo "$png")')" 2>/dev/null || true ;;
  esac
}

# check NAME EXPECTED-LINE-PATTERN... -- ARGS...
check() {
  local name=$1
  shift
  local patterns=()
  while [ "$1" != -- ]; do
    patterns+=("$1")
    shift
  done
  shift
  local report=$artifacts/$name.report log=$artifacts/$name.log status=0
  rm -f "$report"
  OXIDEMERGE_SMOKE_REPORT=$report "$bin" --lang en "$@" >"$log" 2>&1 &
  local pid=$! waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 120 ]; do
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    screenshot "$name"
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    echo "gui-smoke: $name: no answer after ${waited}s"
    failed=1
    return
  fi
  wait "$pid" || status=$?
  if [ ! -s "$report" ]; then
    if [ "$status" != 0 ] && [ "${GUI_SMOKE_STRICT:-0}" != 1 ] &&
      grep -qiE 'opengl|glutin|egl|wgl|glx|no suitable|display|winit' "$log"; then
      echo "::warning::gui-smoke: $name: no usable display or OpenGL here; the window was not checked"
      sed 's/^/    /' "$log"
      return
    fi
    echo "gui-smoke: $name: exited with $status before showing anything"
    sed 's/^/    /' "$log"
    failed=1
    return
  fi
  for p in "${patterns[@]}"; do
    if ! grep -qE -- "$p" "$report"; then
      echo "gui-smoke: $name: expected a line matching: $p"
      sed 's/^/    /' "$report"
      failed=1
      return
    fi
  done
  if grep -q '^error ' "$report"; then
    echo "gui-smoke: $name: the window showed an error"
    sed 's/^/    /' "$report"
    failed=1
    return
  fi
  echo "gui-smoke: $name: ok ($(head -n 1 "$report"))"
}

check two-files '^tab\* a\.txt ↔ b\.txt \| Some\(Different\)' -- files/a.txt files/b.txt
check three-files '^tab\* .*left\.txt.* \| Some\(Different\)' -- files/left.txt files/base.txt files/right.txt
check folders '^busy false' '^tab\* left ↔ right \| Some\(Different\)' -- left right
check identical '^tab\* a\.txt ↔ a\.txt \| Some\(Identical\)' -- files/a.txt files/a.txt

if [ "$failed" != 0 ]; then
  echo "gui-smoke: failed; logs and reports in $artifacts"
  exit 1
fi
