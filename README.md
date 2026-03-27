# clipboard-mcp

Cross-platform MCP server for system clipboard access. Lets AI assistants (Claude, Cursor, etc.) read and write your clipboard.

## Tools

| Tool | Description |
|------|-------------|
| `get_clipboard` | Get the current text content from the system clipboard |
| `set_clipboard` | Set text content to the system clipboard |

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
claude mcp add clipboard clipboard-mcp
```

## Platform Support

- Windows (x86_64)
- macOS (x86_64, aarch64)
- Linux X11/Wayland (x86_64, aarch64)

## How It Works

Uses [arboard](https://github.com/1Password/arboard) for native clipboard access on all platforms. Communicates via MCP protocol over stdio.

## License

MIT
