#!/usr/bin/env bash
# One-time Rust workspace scaffold for BEASTUBE. Safe to re-run: only writes missing files.
set -euo pipefail
cd "$(dirname "$0")/.."

write_if_missing() { # path, content-from-stdin
  local path="$1"
  if [ -e "$path" ]; then echo "skip  $path"; cat >/dev/null; return; fi
  mkdir -p "$(dirname "$path")"
  cat > "$path"
  echo "write $path"
}

write_if_missing Cargo.toml <<'TOML'
[workspace]
resolver = "2"
members = ["src-tauri", "crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.94"
license = "GPL-3.0-or-later"
authors = ["BEASTUBE contributors"]
repository = "https://github.com/beastube/beastube"
publish = false

[workspace.dependencies]
# --- internal crates ---
beastube-core = { path = "crates/beastube-core" }
beastube-db = { path = "crates/beastube-db" }
beastube-network = { path = "crates/beastube-network" }
beastube-cache = { path = "crates/beastube-cache" }
beastube-tasks = { path = "crates/beastube-tasks" }
beastube-provider = { path = "crates/beastube-provider" }
beastube-provider-youtube = { path = "crates/beastube-provider-youtube" }
beastube-filtering = { path = "crates/beastube-filtering" }
beastube-gateway = { path = "crates/beastube-gateway" }
beastube-playback = { path = "crates/beastube-playback" }

# --- async runtime ---
tokio = { version = "1.53", features = ["rt-multi-thread", "macros", "sync", "time", "fs", "signal", "io-util", "net"] }
tokio-util = { version = "0.7", features = ["rt", "time", "io"] }
futures = "0.3"
bytes = "1"

# --- serialization ---
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# --- errors / logging ---
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json", "time"] }
tracing-appender = "0.2"

# --- networking (0.12 line is shared with rustypipe so one client/pool serves both) ---
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls-webpki-roots", "http2", "gzip", "brotli", "stream", "json", "charset"] }
http = "1"
axum = { version = "0.8", default-features = false, features = ["http1", "tokio"] }
url = "2"
percent-encoding = "2"

# --- database ---
sqlx = { version = "0.9", default-features = false, features = ["runtime-tokio", "sqlite", "migrate", "macros", "time", "json"] }

# --- provider ---
rustypipe = { version = "0.11", default-features = false, features = ["rustls-tls-webpki-roots"] }

# --- utilities ---
time = { version = "0.3", features = ["serde", "formatting", "parsing", "macros"] }
uuid = { version = "1", features = ["v7", "serde"] }
moka = { version = "0.12", features = ["future"] }
blake3 = "1"
regex = "1"
parking_lot = "0.12"
async-trait = "0.1"
sysinfo = { version = "0.39", default-features = false, features = ["system"] }
windows = { version = "0.61", features = ["Win32_Foundation", "Win32_Graphics_Dxgi", "Win32_System_Power", "Win32_System_SystemInformation", "Win32_Storage_FileSystem", "Win32_UI_WindowsAndMessaging"] }

# --- tauri ---
tauri = { version = "2.11", features = ["tray-icon", "image-png", "protocol-asset"] }
tauri-build = "2.6"
tauri-plugin-updater = "2.11"
tauri-plugin-notification = "2.4"
tauri-plugin-global-shortcut = "2.3"
tauri-plugin-single-instance = "2.4"
tauri-plugin-opener = "2.5"
tauri-plugin-dialog = "2.7"
tauri-plugin-process = "2.3"

# --- testing ---
tempfile = "3"
proptest = "1"
wiremock = "0.6"
criterion = { version = "0.8", features = ["async_tokio"] }

[profile.release]
codegen-units = 1
lto = "fat"
opt-level = 3
panic = "abort"
strip = true
debug = false

[profile.dev]
opt-level = 0
debug = 1
incremental = true

# Heavy dependencies are optimized even in dev so the app is usable while developing.
[profile.dev.package."*"]
opt-level = 2
debug = false

[profile.bench]
inherits = "release"
debug = true
strip = false
TOML

crate() { # name description
  local name="$1" desc="$2"
  local deps
  deps="$(cat)"
  write_if_missing "crates/beastube-$name/Cargo.toml" <<TOML
[package]
name = "beastube-$name"
description = "$desc"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
publish.workspace = true

[dependencies]
$deps

[dev-dependencies]
tokio = { workspace = true, features = ["test-util"] }
tempfile.workspace = true
TOML
  write_if_missing "crates/beastube-$name/src/lib.rs" <<RS
//! $desc
RS
}

crate core "BEASTUBE core domain types, identifiers, errors, settings and event definitions" <<'DEPS'
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
time.workspace = true
uuid.workspace = true
url.workspace = true
tracing.workspace = true
DEPS

crate db "BEASTUBE local SQLite storage: migrations and repositories" <<'DEPS'
beastube-core.workspace = true
sqlx.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
time.workspace = true
tracing.workspace = true
DEPS

crate network "BEASTUBE network manager: priorities, dedup, retry, cancellation, concurrency" <<'DEPS'
beastube-core.workspace = true
reqwest.workspace = true
tokio.workspace = true
tokio-util.workspace = true
futures.workspace = true
bytes.workspace = true
http.workspace = true
url.workspace = true
thiserror.workspace = true
tracing.workspace = true
parking_lot.workspace = true
serde.workspace = true
DEPS

crate cache "BEASTUBE multi-layer cache (memory, disk, indexed) and thumbnail manager" <<'DEPS'
beastube-core.workspace = true
beastube-network.workspace = true
tokio.workspace = true
tokio-util.workspace = true
moka.workspace = true
blake3.workspace = true
bytes.workspace = true
thiserror.workspace = true
tracing.workspace = true
serde.workspace = true
serde_json.workspace = true
parking_lot.workspace = true
time.workspace = true
DEPS

crate tasks "BEASTUBE prioritized task scheduler with cancellation, retry, timeout and metrics" <<'DEPS'
beastube-core.workspace = true
tokio.workspace = true
tokio-util.workspace = true
futures.workspace = true
thiserror.workspace = true
tracing.workspace = true
parking_lot.workspace = true
serde.workspace = true
DEPS

crate provider "BEASTUBE provider abstraction: search, video, channel, playlist, playback, subtitle traits" <<'DEPS'
beastube-core.workspace = true
async-trait.workspace = true
serde.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
url.workspace = true
DEPS

crate provider-youtube "BEASTUBE YouTube provider adapter built on rustypipe (Innertube)" <<'DEPS'
beastube-core.workspace = true
beastube-provider.workspace = true
beastube-network.workspace = true
rustypipe.workspace = true
reqwest.workspace = true
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
url.workspace = true
regex.workspace = true
time.workspace = true
DEPS

crate filtering "BEASTUBE content/ad filtering subsystem: rule engine, adapters, rollback, diagnostics" <<'DEPS'
beastube-core.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
regex.workspace = true
url.workspace = true
parking_lot.workspace = true
time.workspace = true
DEPS

crate gateway "BEASTUBE loopback media gateway: authenticated localhost proxy for media segments and thumbnails" <<'DEPS'
beastube-core.workspace = true
beastube-network.workspace = true
beastube-cache.workspace = true
beastube-filtering.workspace = true
axum.workspace = true
http.workspace = true
tokio.workspace = true
tokio-util.workspace = true
futures.workspace = true
bytes.workspace = true
reqwest.workspace = true
thiserror.workspace = true
tracing.workspace = true
parking_lot.workspace = true
url.workspace = true
serde.workspace = true
DEPS

crate playback "BEASTUBE playback session orchestration: manifest generation, position checkpoints, recovery policy" <<'DEPS'
beastube-core.workspace = true
beastube-provider.workspace = true
beastube-gateway.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
time.workspace = true
DEPS

write_if_missing src-tauri/Cargo.toml <<'TOML'
[package]
name = "beastube-app"
description = "BEASTUBE desktop application shell (Tauri 2)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
publish.workspace = true

[lib]
name = "beastube_app_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build.workspace = true

[dependencies]
beastube-core.workspace = true
beastube-db.workspace = true
beastube-network.workspace = true
beastube-cache.workspace = true
beastube-tasks.workspace = true
beastube-provider.workspace = true
beastube-provider-youtube.workspace = true
beastube-filtering.workspace = true
beastube-gateway.workspace = true
beastube-playback.workspace = true
tauri.workspace = true
tauri-plugin-updater.workspace = true
tauri-plugin-notification.workspace = true
tauri-plugin-global-shortcut.workspace = true
tauri-plugin-single-instance.workspace = true
tauri-plugin-opener.workspace = true
tauri-plugin-dialog.workspace = true
tauri-plugin-process.workspace = true
tokio.workspace = true
tokio-util.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
tracing-appender.workspace = true
time.workspace = true
uuid.workspace = true
url.workspace = true
parking_lot.workspace = true
sysinfo.workspace = true
windows.workspace = true

[features]
default = []
devtools = ["tauri/devtools"]
TOML

write_if_missing src-tauri/build.rs <<'RS'
fn main() {
    tauri_build::build()
}
RS

write_if_missing src-tauri/src/main.rs <<'RS'
// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    beastube_app_lib::run()
}
RS

write_if_missing src-tauri/src/lib.rs <<'RS'
//! BEASTUBE application shell.

pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running BEASTUBE");
}
RS

write_if_missing src-tauri/tauri.conf.json <<'JSON'
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "BEASTUBE",
  "version": "0.1.0",
  "identifier": "app.beastube.desktop",
  "build": {
    "beforeDevCommand": "pnpm dev",
    "devUrl": "http://localhost:1420",
    "beforeBuildCommand": "pnpm build",
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [
      {
        "label": "main",
        "title": "BEASTUBE",
        "width": 1280,
        "height": 800,
        "minWidth": 480,
        "minHeight": 320,
        "visible": false,
        "center": true
      }
    ],
    "security": {
      "csp": null
    }
  },
  "bundle": {
    "active": true,
    "targets": ["nsis"],
    "icon": ["icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png", "icons/icon.ico"]
  }
}
JSON

write_if_missing src-tauri/capabilities/default.json <<'JSON'
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Baseline capability for the main window",
  "windows": ["main"],
  "permissions": ["core:default"]
}
JSON

write_if_missing src-tauri/icons/source.svg <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#1b1030"/>
      <stop offset="1" stop-color="#0b0716"/>
    </linearGradient>
    <linearGradient id="beam" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#ff5c5c"/>
      <stop offset="1" stop-color="#ff9a3c"/>
    </linearGradient>
  </defs>
  <rect x="64" y="64" width="896" height="896" rx="224" fill="url(#bg)"/>
  <rect x="64" y="64" width="896" height="896" rx="224" fill="none" stroke="#3b2a5c" stroke-width="16"/>
  <path d="M392 300 L392 724 L744 512 Z" fill="url(#beam)"/>
  <path d="M392 300 L392 724 L744 512 Z" fill="none" stroke="#ffd7b0" stroke-opacity="0.35" stroke-width="20" stroke-linejoin="round"/>
</svg>
SVG

echo "scaffold done"
