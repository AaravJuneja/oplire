```
   ___      ___    _       ___     ___     ___
  / _ \    | _ \  | |     |_ _|   | _ \   | __|
 | (_) |   |  _/  | |__    | |    |   /   | _|
  \___/   _|_|_   |____|  |___|   |_|_\   |___|
_|"""""|_| """ |_|"""""|_|"""""|_|"""""|_|"""""|
"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'
     OpenCode Limit Reset + Proxy
     by Berke Oruc
```

[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![AUR](https://img.shields.io/badge/AUR-3.0.0-blue?style=flat-square)](https://aur.archlinux.org/packages/oplire)

## What is oplire?

**oplire** is a dual-purpose tool for **free models of OpenCode Zen**
(like `muse-spark-1.3` free):

1. **WARP Rate Limit Reset** - Rotates your IP via Cloudflare WARP to reset OpenCode rate limits
2. **OpenCode V2 Responses Proxy** - Transparent reverse proxy for `/v1/responses` that works around the upstream `encrypted_content` replay bug on any model with encrypted reasoning (e.g. Muse Spark 1.3)

### How It Works

#### WARP Reset Mode
OpenCode tracks users by IP. When you hit the rate limit:
1. **Stops** the current WARP tunnel
2. **Clears** cached session data
3. **Creates** a new tunnel registration (new IP)
4. **Restarts** WARP with a fresh IP

#### Responses Proxy Mode
OpenCode V2 → oplire proxy (127.0.0.1:8080) → OpenCode Zen
- Exposes `/v1/responses` for Responses-API models with encrypted reasoning, like Muse Spark 1.3
- Two cases: replayed `encrypted_content` gets the stateless treatment
  (`store: false` + `reasoning: {summary: "auto"}`, stale
  `encrypted_content`/`id` stripped, one retry on 400 caller errors);
  clean traffic passes through untouched apart from auth
- Retries once on upstream 400 `encrypted_content` caller errors
- Retries once without `reasoning.summary` on org-verification 400s
- Forwards caller `Authorization` header as-is (normalized, no double `Bearer`)
- **Auto-resets WARP** on 429 rate limits — transparently

## Installation

> **Prerequisite:** start and connect Cloudflare WARP **manually** before
> using oplire (`warp-cli connect`, or the toggle in the Cloudflare One app).
> oplire never connects WARP on its own — `oplire reset` rotates an already
> connected tunnel, and every command below assumes WARP is up.

### Linux (AUR)
```bash
yay -S oplire
```

> Windows is not supported — oplire is WSL-only (`winget` manifest removed).
> macOS works via Homebrew below but WARP reset paths are Linux-first.

### macOS
```bash
brew install berkeoruc/oplire/oplire
```

### From Source
```bash
git clone https://github.com/BerkeOruc/oplire.git
cd oplire
cargo build --release
sudo cp target/release/oplire /usr/bin/oplire
```

## Usage

### Quick Start — Proxy for OpenCode V2
```bash
# Start the Responses proxy
oplire proxy

# With custom upstream
oplire proxy --upstream http://my-opencode-server:3000

# With API key fallback
oplire proxy --api-key <zen-api-key>
```

Point OpenCode V2 at the proxy:
```bash
export OPENAI_BASE_URL=http://127.0.0.1:8080/v1
export OPENAI_API_KEY=Bearer public
```

### WARP Reset Commands
```bash
oplire reset          # Full WARP tunnel reset
oplire quick-reset    # Fast IP rotation (no service restart)
oplire status         # Check WARP connection status
oplire stop           # Stop WARP tunnel
oplire install warp   # Install Cloudflare WARP
```

> **WSL + Windows WARP:** oplire is a WSL tool and shells out to `warp-cli`
> from `PATH`. If your WARP client runs on Windows, expose it to WSL with a
> symlink or wrapper script — a shell `alias` will **not** work (aliases do
> not apply to spawned processes, and it must be named `warp-cli`):
> ```bash
> sudo ln -s '/mnt/c/Program Files/Cloudflare/Cloudflare WARP/warp-cli.exe' /usr/local/bin/warp-cli
> # or: printf '#!/bin/bash\nexec /mnt/c/Program\\ Files/Cloudflare/Cloudflare\\ WARP/warp-cli.exe "$@"\n' > ~/.local/bin/warp-cli && chmod +x ~/.local/bin/warp-cli
> warp-cli status   # verify it resolves before running oplire reset
> ```

### Proxy Commands
```bash
oplire proxy                          # Start reverse proxy on :8080
oplire proxy --listen 0.0.0.0:9000    # Custom listen address
oplire daemon                         # Background daemon mode
oplire watch                          # Monitor OpenCode, auto-reset on 429
```

### Configuration
```bash
oplire config show    # Show current settings
oplire config set     # Save configuration
oplire config reset   # Reset to defaults
```

### Diagnostics
```bash
oplire doctor         # Check WARP, OpenCode setup
oplire about          # Show version and info
```

### OpenCode Plugin (V2)
The plugin registers `/proxy`, `/proxy-status`, `/proxy-stop` via `ctx.command`.
`plugin/oplire.ts` is not auto-discovered from `plugin/` — wire it in `opencode.jsonc`:
```jsonc
{
  "plugins": ["./plugin/oplire.ts"]
}
```
Or copy to auto-loaded dirs:
```bash
cp plugin/oplire.ts ~/.config/opencode/plugins/oplire.ts
```
Then restart OpenCode:
```
/proxy
/proxy 127.0.0.1:8080 http://localhost:3000
/proxy-status
/proxy-stop
```

## Encrypted reasoning fix

The proxy targets the `encrypted_content` replay bug affecting any model
that returns encrypted reasoning on OpenCode Zen (seen on
`muse-spark-1.3-contributor-free`, any reasoning effort).

**Root cause:** OpenCode replays caller-bound `encrypted_content` reasoning blobs across turns and after WARP IP rotations. Zen rejects these with:
```json
{
  "message": "reasoning `encrypted_content` was not issued to this caller"
}
```

**Fix:** The proxy branches per request (LiteLLM Auto-Router pattern).
Requests carrying `encrypted_content` get the stateless treatment:
1. Injects `store: false` + `reasoning: {summary: "auto"}` so Zen returns readable summaries instead of caller-bound blobs
2. Strips any `encrypted_content`/`id` from replayed reasoning input items before forwarding
3. Retries once on 400 `encrypted_content` errors after stripping, and once
   without `reasoning.summary` on org-verification 400s

Requests without `encrypted_content` pass through untouched apart from auth
normalization — no summaries injected, no ids stripped, no `store` forced.

Linux binary is primary and only. Under WSL, `warp-cli` must resolve from
`PATH` (see symlink note above) — oplire does not look for the Windows
install dir itself.

## Options

- `--verbose` - Detailed output
- `--dry-run` - Preview changes without executing
- `--json` - JSON output (status command)

## About

```
Version: 3.0.0
Language: Rust
Purpose: OpenCode V2 Responses proxy + rate limit reset
Infrastructure: Cloudflare WARP + Axum HTTP
Author: Berke Oruc
GitHub: https://github.com/BerkeOruc/oplire
```

## License

MIT License. See [LICENSE](LICENSE) for details.

---

Made by [Berke Oruc](https://github.com/BerkeOruc)
