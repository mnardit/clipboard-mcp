# clipboard-mcp

[![CI](https://github.com/mnardit/clipboard-mcp/workflows/CI/badge.svg)](https://github.com/mnardit/clipboard-mcp/actions)
[![Crates.io](https://img.shields.io/crates/v/clipboard-mcp.svg)](https://crates.io/crates/clipboard-mcp)
[![License: MIT](https://img.shields.io/crates/l/clipboard-mcp.svg)](LICENSE)

Cross-platform [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server that gives AI assistants direct read/write access to your system clipboard. **[Website](https://max.nardit.com/clipboard-mcp)**

Copy an error → ask Claude to fix it → the fix lands in your clipboard. No manual paste into chat, no manual copy from response.

```bash
cargo install clipboard-mcp
```

## Why clipboard-mcp?

- **Single binary** — no Python, no Node.js, no runtime to install
- **Native clipboard** — uses [arboard](https://github.com/1Password/arboard) by 1Password, not shell commands like `pbcopy`/`xclip`
- **Watch mode** — `watch_clipboard` lets agents react to what you copy in real-time
- **Cross-platform** — Windows, macOS (Intel + Apple Silicon), Linux (X11 + Wayland)
- **~250 lines of Rust** — small, auditable, no bloat

## Tools

| Tool | Description |
|------|-------------|
| `get_clipboard` | Read current text from the clipboard. Content over 100 KB is truncated. |
| `set_clipboard` | Write text to the clipboard. |
| `watch_clipboard` | Wait for clipboard text to change (default 30s, max 300s). Non-text changes reported as generic event. Changes reverting within 500ms poll interval may not be detected. |

### watch_clipboard parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `timeout_secs` | integer (optional) | 30 | Seconds to wait for a change (max 300) |

## Installation

```bash
cargo install clipboard-mcp
```

Or download a binary from [GitHub Releases](https://github.com/mnardit/clipboard-mcp/releases).

## Configuration

### Claude Desktop

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "clipboard": {
      "command": "clipboard-mcp"
    }
  }
}
```

### Claude Code

```bash
# Add for current project
claude mcp add clipboard clipboard-mcp

# Or add globally
claude mcp add --scope user clipboard clipboard-mcp
```

## Usage Examples

**Read and transform:**
> "Take whatever is on my clipboard and rewrite it in a more formal tone, then put the result back."

**Watch for changes:**
> "Watch my clipboard for 60 seconds. When I copy something, summarize it in one sentence."

**Round-trip:**
> "Get my clipboard, translate it to German, and set the translation back."

**Data transform:**
> Copy a CSV table → "Convert what's on my clipboard to JSON" → paste formatted JSON into your editor.

**Code from clipboard:**
> Copy a code snippet from a browser → "Review the code on my clipboard for bugs" → Claude reads it directly, no pasting into chat.

**Step-by-step agent output via clipboard history:**
> Run a multi-step task and `set_clipboard` after each step. With any clipboard manager (Paste, CopyQ, Klipper), you get a chronological log of every result — browse, search, and review the agent's work without switching windows.

## Platform Support

- **Windows** (x86_64) — clipboard persists via OS pasteboard
- **macOS** (x86_64, aarch64) — clipboard persists via OS pasteboard
- **Linux** (x86_64, aarch64) — X11 and Wayland (via `wl-data-control` protocol)

> **Linux note:** On Linux, clipboard content set by the server is handed off to your clipboard manager (KDE Klipper, GNOME Clipboard, clipman, etc.) when the selection is released. If no clipboard manager is running, content may not persist. All major desktop environments include one by default.

## How It Works

Single binary. Uses [arboard](https://github.com/1Password/arboard) (by 1Password) for native clipboard access. Communicates via MCP protocol over stdio. No runtime dependencies on Windows and macOS; Linux requires X11 libs or a Wayland compositor with `wl-data-control` support.

## Troubleshooting

**Linux: clipboard content disappears**
Ensure a clipboard manager is running. On bare window managers (i3, dwm), install `clipman`, `parcellite`, or `copyq`.

**Wayland: "clipboard is empty or contains non-text content"**
Your compositor must support the `wl-data-control` protocol. Sway, Hyprland, GNOME, and KDE all do. Older compositors may not.

**macOS: clipboard access denied**
Ensure the terminal running the MCP server has clipboard permissions in System Settings > Privacy & Security.

## Security

This server gives connected MCP clients full read/write access to your system clipboard:

- **`get_clipboard`** returns whatever is currently on your clipboard
- **`set_clipboard`** silently overwrites clipboard contents (max 1 MB)
- **`watch_clipboard`** returns the next thing you copy, verbatim

Only connect this server to AI sessions you trust. Do not use it in environments where sensitive data (passwords, tokens) may be on the clipboard.

## Contributing

Bug reports and pull requests are welcome. For major changes, please open an issue first.

## License

MIT
