use std::time::Duration;

use anyhow::Result;
use arboard::Clipboard;
use rmcp::{
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use serde::Deserialize;

/// Maximum text size returned to the MCP client (100 KB).
const MAX_TEXT_BYTES: usize = 100 * 1024;

/// Maximum text size accepted by set_clipboard (1 MB).
const MAX_SET_BYTES: usize = 1024 * 1024;

// --- Tool parameter types ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetClipboardArgs {
    /// The text to place on the clipboard
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WatchClipboardArgs {
    /// Timeout in seconds to wait for a clipboard change (default: 30, max: 300)
    timeout_secs: Option<u32>,
}

// --- Helpers ---

/// Read clipboard text synchronously. Returns `Ok(None)` for non-text content.
fn read_clipboard_text() -> Result<Option<String>, ErrorData> {
    let mut clipboard = Clipboard::new().map_err(|e| {
        ErrorData::internal_error(format!("Failed to access clipboard: {e}"), None)
    })?;
    match clipboard.get_text() {
        Ok(text) => Ok(Some(text)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(ErrorData::internal_error(
            format!("Failed to read clipboard: {e}"),
            None,
        )),
    }
}

/// Read clipboard text on a blocking thread to avoid stalling the async executor.
async fn read_clipboard_async() -> Result<Option<String>, ErrorData> {
    tokio::task::spawn_blocking(read_clipboard_text)
        .await
        .map_err(|e| ErrorData::internal_error(format!("Task failed: {e}"), None))?
}

/// Truncate text at a char boundary.
fn truncate_text(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Format clipboard text for the MCP response, truncating if needed.
fn format_clipboard_response(text: String) -> CallToolResult {
    let (output, truncated) = truncate_text(&text, MAX_TEXT_BYTES);
    if truncated {
        let total = text.len();
        CallToolResult::success(vec![Content::text(format!(
            "[clipboard text truncated: showing {MAX_TEXT_BYTES} of {total} bytes]\n{output}"
        ))])
    } else {
        CallToolResult::success(vec![Content::text(text)])
    }
}

// --- Server ---

#[derive(Clone)]
struct ClipboardServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ClipboardServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Get the current text content from the system clipboard")]
    async fn get_clipboard(&self) -> Result<CallToolResult, ErrorData> {
        match read_clipboard_async().await? {
            Some(text) if text.is_empty() => Ok(CallToolResult::success(vec![Content::text(
                "[clipboard is empty]",
            )])),
            Some(text) => Ok(format_clipboard_response(text)),
            None => Ok(CallToolResult::success(vec![Content::text(
                "[clipboard is empty or contains non-text content]",
            )])),
        }
    }

    #[tool(description = "Set text content to the system clipboard")]
    async fn set_clipboard(
        &self,
        Parameters(args): Parameters<SetClipboardArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.text.len() > MAX_SET_BYTES {
            return Err(ErrorData::invalid_params(
                format!(
                    "Text too large: {} bytes (max {} bytes)",
                    args.text.len(),
                    MAX_SET_BYTES
                ),
                None,
            ));
        }

        // On Linux (X11/Wayland), the clipboard is owned by the process — content is
        // lost when the Clipboard handle is dropped. We keep the handle alive on a
        // background thread to persist the content until another app takes ownership.
        // A oneshot channel reports success/failure before the thread parks.
        #[cfg(target_os = "linux")]
        {
            let text = args.text.clone();
            let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
            tokio::task::spawn_blocking(move || {
                let mut clipboard = match Clipboard::new() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(format!("{e}")));
                        return;
                    }
                };
                match clipboard.set_text(&text) {
                    Ok(()) => {
                        let _ = tx.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("{e}")));
                        return;
                    }
                }
                // Keep Clipboard alive so X11/Wayland selection persists.
                // Thread stays parked until process exits or a new set_clipboard
                // call takes selection ownership (making this thread irrelevant).
                std::thread::park();
            });
            rx.await
                .map_err(|_| {
                    ErrorData::internal_error("Clipboard task failed".to_string(), None)
                })?
                .map_err(|e| {
                    ErrorData::internal_error(
                        format!("Failed to set clipboard: {e}"),
                        None,
                    )
                })?;
        }

        // On macOS/Windows, the OS pasteboard stores data server-side —
        // content persists after Clipboard is dropped.
        #[cfg(not(target_os = "linux"))]
        {
            let text = args.text.clone();
            tokio::task::spawn_blocking(move || {
                let mut clipboard = Clipboard::new().map_err(|e| {
                    ErrorData::internal_error(
                        format!("Failed to access clipboard: {e}"),
                        None,
                    )
                })?;
                clipboard.set_text(&text).map_err(|e| {
                    ErrorData::internal_error(
                        format!("Failed to set clipboard: {e}"),
                        None,
                    )
                })
            })
            .await
            .map_err(|e| ErrorData::internal_error(format!("Task failed: {e}"), None))??;
        }

        let char_count = args.text.chars().count();
        let preview = args
            .text
            .char_indices()
            .nth(100)
            .map(|(i, _)| &args.text[..i])
            .unwrap_or(&args.text);
        let preview_clean: String = preview
            .chars()
            .map(|c| match c {
                '\n' => '↵',
                '\r' => '⏎',
                '\t' => '⇥',
                c if c.is_control() => '·',
                c => c,
            })
            .collect();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Clipboard set ({char_count} chars): \"{preview_clean}\""
        ))]))
    }

    #[tool(
        description = "Wait for the clipboard text to change. \
                       Blocks up to timeout_secs seconds (default 30, max 300) until new text appears. \
                       Only detects text content changes; non-text clipboard changes (images, files) \
                       are reported as a generic change event. \
                       Changes that revert within the 500ms poll interval may not be detected."
    )]
    async fn watch_clipboard(
        &self,
        Parameters(args): Parameters<WatchClipboardArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let timeout_secs = args.timeout_secs.unwrap_or(30).min(300);
        let poll_interval = Duration::from_millis(500);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs as u64);

        let initial = read_clipboard_async().await.ok().flatten();
        let initial_len = initial.as_ref().map(|s| s.len());

        let mut consecutive_errors: u32 = 0;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(CallToolResult::success(vec![Content::text(
                    "[timeout — clipboard unchanged]",
                )]));
            }

            tokio::time::sleep(remaining.min(poll_interval)).await;

            let current = match read_clipboard_async().await {
                Ok(val) => {
                    consecutive_errors = 0;
                    val
                }
                Err(_) => {
                    consecutive_errors += 1;
                    if consecutive_errors >= 5 {
                        return Err(ErrorData::internal_error(
                            "Clipboard access failed repeatedly".to_string(),
                            None,
                        ));
                    }
                    continue;
                }
            };

            let current_len = current.as_ref().map(|s| s.len());
            if current_len != initial_len || current != initial {
                return match current {
                    Some(text) if !text.is_empty() => Ok(format_clipboard_response(text)),
                    Some(_) => Ok(CallToolResult::success(vec![Content::text(
                        "[clipboard changed but is now empty]",
                    )])),
                    None => Ok(CallToolResult::success(vec![Content::text(
                        "[clipboard changed to non-text content]",
                    )])),
                };
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for ClipboardServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "clipboard-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Cross-platform MCP server for system clipboard access. \
                 Use get_clipboard to read, set_clipboard to write, \
                 and watch_clipboard to wait for changes."
                    .to_string(),
            )
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("clipboard-mcp starting");

    let service = ClipboardServer::new()
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;

    service.waiting().await?;
    Ok(())
}
