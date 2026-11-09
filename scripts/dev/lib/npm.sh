dev_npm_lock_hash() {
  node -e '
    const fs = require("node:fs");
    const crypto = require("node:crypto");
    process.stdout.write(crypto.createHash("sha256").update(fs.readFileSync("package-lock.json")).digest("hex"));
  '
}

ensure_dev_npm_dependencies() {
  local lock_hash stamped_hash="" stamp_path=".lightspeed/dev-npm-lock.sha256"
  lock_hash="$(dev_npm_lock_hash)" || return
  if [[ -f "$stamp_path" ]]; then
    stamped_hash="$(<"$stamp_path")"
  fi
  if [[ ! -x node_modules/.bin/tsx || ! -x node_modules/.bin/vite || ! -x node_modules/.bin/astro || "$stamped_hash" != "$lock_hash" ]]; then
    echo "[bootstrap] Installing root npm workspace dependencies..."
    npm "$@" || return
    mkdir -p "$(dirname "$stamp_path")" || return
    lock_hash="$(dev_npm_lock_hash)" || return
    printf '%s\n' "$lock_hash" > "$stamp_path"
  fi
}
