#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(dirname "$SCRIPT_DIR")
FINDEX_HOME=${FINDEX_HOME:-"$HOME/.findex"}
INSTALL_DIR=${FINDEX_INSTALL_DIR:-"$FINDEX_HOME/bin"}

FEATURE_ARGS=""
if [ "${FINDEX_CUDA:-0}" = "1" ]; then
  FEATURE_ARGS="--features cuda"
fi

echo "Building Findex in release mode..."
# shellcheck disable=SC2086
cargo build --release -p findex-cli $FEATURE_ARGS --manifest-path "$PROJECT_ROOT/Cargo.toml"
mkdir -p "$INSTALL_DIR"
cp "$PROJECT_ROOT/target/release/findex-cli" "$INSTALL_DIR/findex"
chmod +x "$INSTALL_DIR/findex"

if [ "${FINDEX_SKIP_MODEL:-0}" != "1" ]; then
  echo "Acquiring pinned embedding and reranking models..."
  "$INSTALL_DIR/findex" models
fi

PROFILE=${FINDEX_PROFILE:-"$HOME/.profile"}
PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""
MODEL_LINE="export FINDEX_MODEL_POLICY=offline"
touch "$PROFILE"
grep -Fqx "$PATH_LINE" "$PROFILE" || printf '\n%s\n' "$PATH_LINE" >> "$PROFILE"

mkdir -p "$FINDEX_HOME"
if [ "${FINDEX_SKIP_MODEL:-0}" != "1" ]; then
  grep -Fqx "$MODEL_LINE" "$PROFILE" || printf '%s\n' "$MODEL_LINE" >> "$PROFILE"
  cat > "$FINDEX_HOME/mcp-config.json" <<EOF
{
  "mcpServers": {
    "findex": {
      "command": "$INSTALL_DIR/findex",
      "args": ["--db-path", ".findex_db", "mcp"],
      "env": {
        "FINDEX_MODEL_POLICY": "offline"
      }
    }
  }
}
EOF
else
  cat > "$FINDEX_HOME/mcp-config.json" <<EOF
{
  "mcpServers": {
    "findex": {
      "command": "$INSTALL_DIR/findex",
      "args": ["--db-path", ".findex_db", "mcp"]
    }
  }
}
EOF
fi

echo "Findex installed at $INSTALL_DIR/findex"
echo "MCP configuration written to $FINDEX_HOME/mcp-config.json"
echo "Restart your shell or source $PROFILE to update PATH."
