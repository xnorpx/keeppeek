#!/bin/sh

set -eu

usage() {
	cat <<'EOF'
Usage:
  ./install-macos.sh [--prefix DIRECTORY]
  sudo ./install-macos.sh --service [--user USER]

Install KeepPeek on Apple Silicon Macs.

Without --service, KeepPeek is installed in ~/.local/bin by default and can
be run manually with keeppeek. --prefix selects a different installation
directory.

With --service, KeepPeek is installed in /usr/local/bin and started by
launchd as USER. USER defaults to the user that invoked sudo.
EOF
}

fail() {
	printf '%s\n' "error: $*" >&2
	exit 1
}

mode=manual
prefix=
prefix_set=false
service_user=

while [ "$#" -gt 0 ]; do
	case "$1" in
	--service)
		mode=service
		;;
	--user)
		shift
		[ "$#" -gt 0 ] || fail "--user requires a user name"
		service_user=$1
		;;
	--prefix)
		shift
		[ "$#" -gt 0 ] || fail "--prefix requires a directory"
		prefix=$1
		prefix_set=true
		;;
	--help | -h)
		usage
		exit 0
		;;
	*)
		fail "unknown option: $1"
		;;
	esac
	shift
done

[ "$(uname -s)" = Darwin ] || fail "this installer only runs on macOS"
[ "$(uname -m)" = arm64 ] || fail "KeepPeek for macOS requires Apple Silicon (arm64)"

script_dir=$(
	unset CDPATH
	cd -- "$(dirname -- "$0")"
	pwd
)
binary="$script_dir/keeppeek"
plist_template="$script_dir/com.keeppeek.plist"

if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
	fail "keeppeek binary is missing from this release"
fi

if [ "$mode" = manual ]; then
	[ -n "${HOME:-}" ] || fail "HOME must be set for a manual installation"
	if [ "$prefix_set" = false ]; then
		prefix="$HOME/.local/bin"
	fi

	install -d -m 755 "$prefix"
	install -m 755 "$binary" "$prefix/keeppeek"
	printf '%s\n' "installed KeepPeek at $prefix/keeppeek"
	printf '%s\n' "run $prefix/keeppeek to start KeepPeek manually"
	exit 0
fi

[ "$prefix_set" = false ] || fail "--prefix cannot be used with --service"
[ "$(id -u)" -eq 0 ] || fail "--service must be run with sudo"

if [ -z "$service_user" ]; then
	service_user=${SUDO_USER:-}
fi
[ -n "$service_user" ] || fail "specify the service account with --user"

id -u "$service_user" >/dev/null 2>&1 || fail "user does not exist: $service_user"
service_home=$(
	dscl . -read "/Users/$service_user" NFSHomeDirectory |
		sed -n 's/^NFSHomeDirectory: //p' |
		head -n 1
)
if [ -z "$service_home" ] || [ ! -d "$service_home" ]; then
	fail "unable to determine a home directory for $service_user"
fi

plist=/Library/LaunchDaemons/com.keeppeek.plist
temporary_plist=$(mktemp "${plist}.XXXXXX")
trap 'rm -f "$temporary_plist"' 0

install -o root -g wheel -m 644 "$plist_template" "$temporary_plist"
plutil -replace UserName -string "$service_user" "$temporary_plist"
plutil -replace EnvironmentVariables.HOME -string "$service_home" "$temporary_plist"
plutil -lint "$temporary_plist" >/dev/null

install -d -o root -g wheel -m 755 /usr/local/bin
if launchctl print system/com.keeppeek >/dev/null 2>&1; then
	launchctl bootout system/com.keeppeek
	attempt=0
	while launchctl print system/com.keeppeek >/dev/null 2>&1; do
		attempt=$((attempt + 1))
		[ "$attempt" -lt 10 ] || fail "existing KeepPeek service did not stop"
		sleep 1
	done
fi
install -o root -g wheel -m 755 "$binary" /usr/local/bin/keeppeek
mv -f "$temporary_plist" "$plist"

if ! launchctl bootstrap system "$plist"; then
	rm -f "$plist"
	fail "unable to bootstrap KeepPeek service"
fi
if ! launchctl enable system/com.keeppeek; then
	launchctl bootout system/com.keeppeek >/dev/null 2>&1 || :
	rm -f "$plist"
	fail "unable to enable KeepPeek service"
fi
if ! launchctl kickstart -k system/com.keeppeek; then
	launchctl bootout system/com.keeppeek >/dev/null 2>&1 || :
	rm -f "$plist"
	fail "unable to start KeepPeek service"
fi

printf '%s\n' "installed and started KeepPeek service for $service_user"
