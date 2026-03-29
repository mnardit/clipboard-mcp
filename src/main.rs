use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use arboard::Clipboard;
use clap::Parser;
use rmcp::{
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::{
        stdio,
        streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig,
            StreamableHttpService,
        },
    },
    ErrorData, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use axum::middleware;

/// Maximum concurrent watch_clipboard calls.
static WATCH_SEMAPHORE: Semaphore = Semaphore::const_new(5);

/// Maximum text size returned to the MCP client (100 KB).
const MAX_TEXT_BYTES: usize = 100 * 1024;

/// Maximum text size accepted by set_clipboard (1 MB).
const MAX_SET_BYTES: usize = 1024 * 1024;

// --- CLI ---

/// Cross-platform MCP server for system clipboard access
#[derive(Parser)]
#[command(name = "clipboard-mcp", version)]
struct Cli {
    /// Run as HTTP server instead of stdio
    #[arg(long)]
    http: bool,

    /// Port for HTTP server
    #[arg(long, default_value = "3100")]
    port: u16,

    /// Bind address for HTTP server
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
}

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

/// Read clipboard HTML synchronously. Returns `Ok(None)` if no HTML available.
fn read_clipboard_html() -> Result<Option<String>, ErrorData> {
    let mut clipboard = Clipboard::new().map_err(|e| {
        ErrorData::internal_error(format!("Failed to access clipboard: {e}"), None)
    })?;
    match clipboard.get().html() {
        Ok(html) => Ok(Some(html)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(ErrorData::internal_error(
            format!("Failed to read clipboard HTML: {e}"),
            None,
        )),
    }
}

/// Read clipboard text on a blocking thread.
async fn read_clipboard_async() -> Result<Option<String>, ErrorData> {
    tokio::task::spawn_blocking(read_clipboard_text)
        .await
        .map_err(|e| ErrorData::internal_error(format!("Task failed: {e}"), None))?
}

/// Read clipboard HTML on a blocking thread.
async fn read_clipboard_html_async() -> Result<Option<String>, ErrorData> {
    tokio::task::spawn_blocking(read_clipboard_html)
        .await
        .map_err(|e| ErrorData::internal_error(format!("Task failed: {e}"), None))?
}

/// Probe which clipboard formats are currently available.
fn probe_clipboard_formats() -> Result<Vec<&'static str>, ErrorData> {
    let mut cb = Clipboard::new().map_err(|e| {
        ErrorData::internal_error(format!("Failed to access clipboard: {e}"), None)
    })?;
    let mut formats = Vec::new();

    if cb.get_text().is_ok() {
        formats.push("text");
    }
    if cb.get().html().is_ok() {
        formats.push("html");
    }
    if cb.get_image().is_ok() {
        formats.push("image");
    }
    if cb.get().file_list().is_ok() {
        formats.push("files");
    }

    Ok(formats)
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

/// Format clipboard text for MCP response, truncating if needed.
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

/// On Linux, tracks the thread holding the clipboard alive so we can unpark it.
#[cfg(target_os = "linux")]
static CLIPBOARD_THREAD: std::sync::Mutex<Option<std::thread::Thread>> =
    std::sync::Mutex::new(None);

/// Unpark and replace the previous Linux clipboard holder thread.
#[cfg(target_os = "linux")]
fn release_clipboard_thread() {
    let mut guard = CLIPBOARD_THREAD.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = guard.take() {
        old.unpark();
    }
}

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

    #[tool(description = "Get HTML content from the system clipboard. \
                          Returns the HTML markup if available, \
                          or a message if no HTML content is on the clipboard.")]
    async fn get_clipboard_html(&self) -> Result<CallToolResult, ErrorData> {
        match read_clipboard_html_async().await? {
            Some(html) if html.is_empty() => Ok(CallToolResult::success(vec![Content::text(
                "[clipboard HTML is empty]",
            )])),
            Some(html) => Ok(format_clipboard_response(html)),
            None => Ok(CallToolResult::success(vec![Content::text(
                "[clipboard does not contain HTML content]",
            )])),
        }
    }

    #[tool(description = "List which content formats are currently on the clipboard. \
                          Probes for text, HTML, image, and file list.")]
    async fn list_clipboard_formats(&self) -> Result<CallToolResult, ErrorData> {
        let formats = tokio::task::spawn_blocking(probe_clipboard_formats)
            .await
            .map_err(|e| ErrorData::internal_error(format!("Task failed: {e}"), None))??;

        if formats.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "[clipboard is empty — no formats detected]",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Available clipboard formats: {}",
                formats.join(", ")
            ))]))
        }
    }

    #[tool(description = "Clear all content from the system clipboard")]
    async fn clear_clipboard(&self) -> Result<CallToolResult, ErrorData> {
        #[cfg(target_os = "linux")]
        release_clipboard_thread();

        tokio::task::spawn_blocking(|| {
            let mut clipboard = Clipboard::new().map_err(|e| {
                ErrorData::internal_error(format!("Failed to access clipboard: {e}"), None)
            })?;
            clipboard.clear().map_err(|e| {
                ErrorData::internal_error(format!("Failed to clear clipboard: {e}"), None)
            })
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("Task failed: {e}"), None))??;

        Ok(CallToolResult::success(vec![Content::text(
            "Clipboard cleared",
        )]))
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

        #[cfg(target_os = "linux")]
        {
            // Use std::thread::spawn (NOT tokio spawn_blocking) because the thread
            // parks to keep the X11/Wayland clipboard alive.
            // Thread handle is sent back via oneshot so we can atomically replace
            // the previous thread on the async side — no race between unpark and store.
            let text = args.text.clone();
            let (tx, rx) =
                tokio::sync::oneshot::channel::<Result<std::thread::Thread, String>>();
            std::thread::spawn(move || {
                let mut clipboard = match Clipboard::new() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(format!("{e}")));
                        return;
                    }
                };
                if let Err(e) = clipboard.set_text(&text) {
                    let _ = tx.send(Err(format!("{e}")));
                    return;
                }
                // Send our handle back before parking.
                let _ = tx.send(Ok(std::thread::current()));
                std::thread::park();
            });
            let thread_handle = rx
                .await
                .map_err(|_| {
                    ErrorData::internal_error("Clipboard task failed".to_string(), None)
                })?
                .map_err(|e| {
                    ErrorData::internal_error(format!("Failed to set clipboard: {e}"), None)
                })?;
            // Atomically replace: unpark old thread, store new one.
            {
                let mut guard = CLIPBOARD_THREAD
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(old) = guard.replace(thread_handle) {
                    old.unpark();
                }
            }
        }

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
        let _permit = WATCH_SEMAPHORE.try_acquire().map_err(|_| {
            ErrorData::invalid_params(
                "Too many concurrent watch_clipboard calls".to_string(),
                None,
            )
        })?;

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
                 Tools: get_clipboard, get_clipboard_html, set_clipboard, \
                 watch_clipboard, list_clipboard_formats, clear_clipboard."
                    .to_string(),
            )
    }
}

/// Reject requests with an Origin header (browser-initiated).
/// CORS headers only prevent reading responses — they don't block execution.
/// This middleware blocks the request before any tool runs.
async fn reject_browser_requests(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    if request.headers().contains_key("origin") {
        return axum::response::Response::builder()
            .status(403)
            .body(axum::body::Body::from("Forbidden: browser requests not allowed"))
            .unwrap();
    }
    next.run(request).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Prevent clap from writing --help/--version to stdout (corrupts MCP stdio).
    let cli = Cli::try_parse().unwrap_or_else(|e| {
        use clap::error::ErrorKind;
        match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                eprintln!("{e}");
                std::process::exit(0);
            }
            _ => {
                let _ = e.print();
                std::process::exit(e.exit_code());
            }
        }
    });

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    if cli.http {
        let addr = format!("{}:{}", cli.host, cli.port);

        if cli.host != "127.0.0.1" && cli.host != "::1" && cli.host != "localhost" {
            tracing::warn!(
                "Binding to {} — clipboard is accessible from any reachable network interface. \
                 Use 127.0.0.1 for local-only access.",
                cli.host
            );
        }

        tracing::info!("clipboard-mcp HTTP server starting on {addr}");

        let ct = CancellationToken::new();
        // Stateless mode: no session accumulation, ClipboardServer is stateless anyway.
        let config = StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_cancellation_token(ct.child_token());

        let service = StreamableHttpService::new(
            || Ok(ClipboardServer::new()),
            Arc::new(LocalSessionManager::default()),
            config,
        );

        // Reject any request with an Origin header to prevent browser-based attacks.
        // CorsLayer only hides responses — this middleware blocks execution entirely.
        let router = axum::Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn(reject_browser_requests));
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        tracing::info!("clipboard-mcp HTTP server listening on {addr}");

        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                tokio::signal::ctrl_c().await.ok();
                ct.cancel();
            })
            .await?;
    } else {
        tracing::info!("clipboard-mcp starting (stdio)");

        let service = ClipboardServer::new()
            .serve(stdio())
            .await
            .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;

        service.waiting().await?;
    }

    Ok(())
}
