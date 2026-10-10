#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
OUT="$ROOT/build"
rm -rf "$OUT"
mkdir -p "$OUT/classes"

javac -source 8 -target 8 -d "$OUT/classes" \
  "$ROOT/src/main/java/dev/glide/runtime/GlideRuntimeAgent.java"

cat > "$OUT/manifest.mf" <<'EOF'
Manifest-Version: 1.0
Premain-Class: dev.glide.runtime.GlideRuntimeAgent
Agent-Class: dev.glide.runtime.GlideRuntimeAgent
Can-Redefine-Classes: false
Can-Retransform-Classes: false
EOF

jar cfm "$OUT/glide-runtime-agent.jar" "$OUT/manifest.mf" -C "$OUT/classes" .
echo "GLIDE_RUNTIME_AGENT=$OUT/glide-runtime-agent.jar"
