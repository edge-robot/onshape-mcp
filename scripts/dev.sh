#!/usr/bin/env bash
# Devcontainer launcher for rust-mcp-dev, owned by onshape-architekt.
#
#   ./scripts/dev.sh [--rebuild] [command ...]     (default command: bash)
#   ./scripts/dev.sh claude --agent rust-mcp-dev
#
# Every invocation starts a fresh, ephemeral container (--rm), so single-file
# read-only mounts never show stale content after host-side edits.
set -euo pipefail

IMAGE=localhost/onshape-mcp-dev:latest
NAME_PREFIX=onshape-mcp-dev
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
FOREIGN=$(cd "$REPO/../clawonshape" && pwd)
ENV_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/onshape-devcontainer/claude.env"
W=/workspaces
OWN="$W/onshape-mcp"

die() { echo "dev.sh: $*" >&2; exit 1; }

[[ -f "$ENV_FILE" ]] || die "missing $ENV_FILE (run 'claude setup-token' and store CLAUDE_CODE_OAUTH_TOKEN=<token>)"
[[ "$(stat -c %a "$ENV_FILE")" == 600 ]] || die "$ENV_FILE must have mode 0600"
git_name=$(git -C "$REPO" config user.name) || die "git user.name is not set"
git_email=$(git -C "$REPO" config user.email) || die "git user.email is not set"
# Use the host's timezone so dates in prompt/report file names, docs and memory match
# the architect's (the containers share the host clock; only the zone differs).
host_tz=$(timedatectl show -p Timezone --value 2>/dev/null || true)
host_tz=${host_tz:-Europe/Berlin}

if [[ "${1:-}" == --rebuild ]]; then
  shift
  podman build --target dev -t "$IMAGE" -f "$REPO/Containerfile" "$REPO"
elif ! podman image exists "$IMAGE"; then
  podman build --target dev -t "$IMAGE" -f "$REPO/Containerfile" "$REPO"
fi

# Mount points that must exist before they can be overlaid read-only.
mkdir -p "$REPO/crates/onshape-mcp-core/tests/contract" "$REPO/.claude/agents" "$REPO/.claude/agent-memory"
touch "$REPO/.claude/settings.json"

ro() { printf -- '--volume=%s:%s:ro' "$1" "$2"; }

args=(
  --rm -it --init
  --name "$NAME_PREFIX-$$"
  --userns=keep-id
  --cap-drop=all
  --security-opt=no-new-privileges
  --env-file "$ENV_FILE"
  -e CLAUDE_DEVCONTAINER=1
  -e TZ="$host_tz"
  -e HOME=/home/dev
  -e GIT_AUTHOR_NAME="$git_name" -e GIT_AUTHOR_EMAIL="$git_email"
  -e GIT_COMMITTER_NAME="$git_name" -e GIT_COMMITTER_EMAIL="$git_email"
  -e GIT_CONFIG_COUNT=2
  -e GIT_CONFIG_KEY_0=core.hooksPath -e GIT_CONFIG_VALUE_0=/opt/devcontainer/hooks
  -e GIT_CONFIG_KEY_1=safe.directory -e GIT_CONFIG_VALUE_1='*'
  -v onshape-mcp-dev-home:/home/dev:U
  -v onshape-mcp-dev-cargo-registry:/usr/local/cargo/registry:U
  # Own repo read-write ...
  -v "$REPO:$OWN:rw"
  # ... with architect-owned paths read-only on top.
  "$(ro "$REPO/crates/onshape-mcp-core/tests/contract" "$OWN/crates/onshape-mcp-core/tests/contract")"
  "$(ro "$REPO/.claude/settings.json" "$OWN/.claude/settings.json")"
  "$(ro "$REPO/.claude/agents" "$OWN/.claude/agents")"
  "$(ro "$REPO/Containerfile" "$OWN/Containerfile")"
  "$(ro "$REPO/.containerignore" "$OWN/.containerignore")"
  "$(ro "$REPO/.devcontainer" "$OWN/.devcontainer")"
  "$(ro "$REPO/scripts/dev.sh" "$OWN/scripts/dev.sh")"
  "$(ro "$REPO/Dockerfile" "$OWN/Dockerfile")"
  # Foreign repo read-only; its runtime state and secrets are hidden.
  "$(ro "$FOREIGN" "$W/clawonshape")"
  --mount "type=tmpfs,destination=$W/clawonshape/state,tmpfs-mode=0700"
  -w "$OWN"
)

[[ $# -gt 0 ]] || set -- bash
# Herdr recognises agents by argv[0]; podman hides the containerised claude process from
# it, so present the launcher as "claude" when it starts Claude Code.
if [[ "$1" == claude ]]; then
  exec -a claude podman run "${args[@]}" "$IMAGE" "$@"
fi
exec podman run "${args[@]}" "$IMAGE" "$@"
