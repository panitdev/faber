#!/bin/sh
# faber agent installer. Served by faber itself, with the API URL below
# filled in at request time — this file is a template, not a script anyone
# should run from a checkout.
#
#   curl -fsSL https://faber.example.com/api/agent/install.sh | sh -s -- --token <token>
#
# By default everything happens under $HOME and nothing here uses sudo. The
# daemon runs as a systemd *user* service with exactly the privileges of
# whoever runs this command, so an install that needed root would be granting
# the daemon more than the account that asked for it.
#
# `--system` installs a system unit instead, and faber never puts it in the
# command it hands out — every host is one a user owns, so user scope is the
# only scope faber installs. It remains for a manual install by whoever
# administers the machine itself.

set -eu

API="@FABER_API@"
TOKEN=""
START="yes"
SYSTEM="no"

while [ $# -gt 0 ]; do
    case "$1" in
        --token) TOKEN="${2:-}"; shift 2 ;;
        # Writes the service unit but leaves it stopped, for anyone who
        # supervises this some other way.
        --no-start) START="no"; shift ;;
        --system) SYSTEM="yes"; shift ;;
        *) echo "faber-agent install: unrecognized argument '$1'" >&2; exit 2 ;;
    esac
done

if [ "$SYSTEM" = "yes" ] && [ "$(id -u)" != "0" ]; then
    echo "faber-agent install: --system writes a system unit and /etc/faber-agent; run it as root." >&2
    exit 1
fi

if [ -z "$TOKEN" ]; then
    echo "faber-agent install: --token is required." >&2
    echo "get one from the host's page in faber, then:" >&2
    echo "  curl -fsSL $API/api/agent/install.sh | sh -s -- --token <token>" >&2
    exit 2
fi

ARCH="$(uname -m)"
case "$ARCH" in
    # Only what faber actually builds. Naming the architecture beats letting
    # the download 404 into a confusing message, and beats far more the
    # alternative of handing over an x86_64 binary that dies later with
    # "exec format error".
    x86_64|amd64) ARCH="x86_64" ;;
    *)
        echo "faber-agent install: no agent binary is built for $ARCH (only x86_64 today)." >&2
        exit 1
        ;;
esac

OS="$(uname -s)"
if [ "$OS" != "Linux" ]; then
    echo "faber-agent install: the agent daemon is Linux-only; this is $OS." >&2
    exit 1
fi

# A system install puts the binary where a system unit may exec it: $HOME
# for whoever ran `sudo` is not a path root should be executing out of, and
# on a machine with no such account it does not exist at all.
if [ "$SYSTEM" = "yes" ]; then
    BIN_DIR="/usr/local/bin"
else
    BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
fi
BIN="$BIN_DIR/faber-agent"
mkdir -p "$BIN_DIR"

# Downloaded beside its destination and moved into place, so an interrupted
# transfer leaves no half-written binary a service unit might later exec.
TMP="$(mktemp "$BIN_DIR/.faber-agent.XXXXXX")"
trap 'rm -f "$TMP"' EXIT INT TERM

URL="$API/api/agent/binary/$ARCH"
echo "downloading $URL"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMP"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$TMP" "$URL"
else
    echo "faber-agent install: neither curl nor wget is available." >&2
    exit 1
fi

if [ ! -s "$TMP" ]; then
    echo "faber-agent install: the download was empty." >&2
    exit 1
fi

chmod 755 "$TMP"
mv -f "$TMP" "$BIN"
trap - EXIT INT TERM
echo "installed $BIN"

# The binary takes it from here: it generates a host keypair, trades the
# bootstrap token for a long-lived credential, and writes and starts its own
# systemd unit. The token is single-use, so this runs exactly once.
set -- install --token "$TOKEN" --api "$API"
if [ "$SYSTEM" = "yes" ]; then set -- "$@" --system; fi
if [ "$START" != "yes" ]; then set -- "$@" --no-start; fi
"$BIN" "$@"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: $BIN_DIR is not on your PATH; the service uses the absolute path regardless." ;;
esac
