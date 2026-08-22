#!/usr/bin/env bash
#
# Drives the macOS Home app to open a paired camera's live view, then classifies
# which HAP streaming path the controller chose.
#
# The Home app on macOS has NO "Add Accessory" command, so pairing cannot be
# automated here; pair once from an iPhone or iPad. Everything after pairing is
# automatable and is what this script does.
#
# Usage:
#   home-live-view.sh --camera "Deck" [--log /tmp/hkfs.log] [--wait 20]
#                     [--shot /tmp/home-live-view.png]

set -euo pipefail

camera=""
log_file=""
wait_seconds=20
screenshot="/tmp/home-live-view.png"

while [ $# -gt 0 ]; do
	case "$1" in
	--camera)
		camera="$2"
		shift 2
		;;
	--log)
		log_file="$2"
		shift 2
		;;
	--wait)
		wait_seconds="$2"
		shift 2
		;;
	--shot)
		screenshot="$2"
		shift 2
		;;
	*)
		printf 'unknown argument: %s\n' "$1" >&2
		exit 2
		;;
	esac
done

if [ -z "$camera" ]; then
	printf 'usage: %s --camera NAME [--log FILE] [--wait SECONDS] [--shot PNG]\n' "$0" >&2
	exit 2
fi

if ! osascript -e 'tell application "System Events" to return name of first process whose frontmost is true' >/dev/null 2>&1; then
	cat >&2 <<-'EOF'
		Accessibility permission is missing.

		System Settings > Privacy & Security > Accessibility, then enable the
		terminal or editor running this script. Automation must also allow
		control of "System Events" and "Home".
	EOF
	exit 1
fi

log_offset=0
if [ -n "$log_file" ] && [ -f "$log_file" ]; then
	log_offset=$(wc -c <"$log_file" | tr -d ' ')
fi

click_result=$(
	osascript <<-APPLESCRIPT
		on findBtn(el, depth, maxDepth, target)
		  if depth > maxDepth then return missing value
		  tell application "System Events"
		    try
		      if role of el is "AXButton" and description of el is target then return el
		    end try
		    try
		      repeat with child in (UI elements of el)
		        set f to my findBtn(child, depth + 1, maxDepth, target)
		        if f is not missing value then return f
		      end repeat
		    end try
		  end tell
		  return missing value
		end findBtn

		tell application "Home" to activate
		delay 2
		tell application "System Events" to tell process "Home"
		  -- A previously opened live view leaves an extra window in front, so
		  -- dismiss anything modal before looking for the accessory grid.
		  repeat 3 times
		    key code 53
		    delay 0.6
		  end repeat
		  set b to missing value
		  repeat with w in windows
		    set b to my findBtn(w, 0, 20, "$camera")
		    if b is not missing value then exit repeat
		  end repeat
		  if b is missing value then return "NOTFOUND"
		  click b
		  return "CLICKED"
		end tell
	APPLESCRIPT
)

if [ "$click_result" = "NOTFOUND" ]; then
	printf 'camera tile "%s" was not found in the Home app\n' "$camera" >&2
	printf 'it must be paired first, which requires an iPhone or iPad\n' >&2
	exit 1
fi

printf 'opened "%s"; observing for %ss\n' "$camera" "$wait_seconds"
sleep "$wait_seconds"

screencapture -x "$screenshot" 2>/dev/null && printf 'screenshot: %s\n' "$screenshot"

if [ -z "$log_file" ] || [ ! -f "$log_file" ]; then
	printf 'no log file supplied; classify the path manually\n'
	exit 0
fi

# tracing colours its key=value pairs, which breaks every pattern below unless
# the escape sequences are removed first.
captured=$(tail -c "+$((log_offset + 1))" "$log_file" | sed $'s/\033\[[0-9;]*m//g')
printf -- '--- characteristics the controller touched ---\n'
printf '%s' "$captured" | grep -oE 'peer=[0-9.]+' | sort -u | sed 's/^/  /' || true
printf '%s' "$captured" | grep -oE 'id=1\.[0-9,.]+' | sort -u | sed 's/^/  read /' || true
printf '%s' "$captured" | grep -oE 'iid=[0-9]+ enabled=true' | sort -u | sed 's/^/  subscribed /' || true

if printf '%s' "$captured" | grep -qi 'solicit'; then
	printf '\nRESULT: WebRTC path (SolicitOffer)\n'
	exit 0
fi
if printf '%s' "$captured" | grep -qiE 'setupendpoints|setup_endpoints'; then
	printf '\nRESULT: legacy RTP path (SetupEndpoints)\n'
	printf 'that path needs SRTP and an RFC 6184 packetizer, neither of which exists here\n'
	exit 3
fi

# iids 38-44 are the legacy CameraRTPStreamManagement characteristics; reading
# them without writing SetupEndpoints means the controller inspected the legacy
# service and gave up rather than choosing the WebRTC service.
if printf '%s' "$captured" | grep -qE 'id=1\.(38|39|40|41|42|43|44)'; then
	printf '\nRESULT: controller inspected the LEGACY RTP service and stopped\n'
	printf 'it read the legacy capability characteristics and never touched the\n'
	printf 'WebRTC service, so it does not implement the 2026 HKSV WebRTC spec\n'
	exit 5
fi

printf '\nRESULT: no stream request observed\n'
printf 'the controller connected without asking for media, or never connected\n'
exit 4
