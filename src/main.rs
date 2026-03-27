use anyhow::Result;
use arboard::Clipboard;
use rmcp::{
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use serde::Deserialize;

// --- Tool parameter types ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetClipboardArgs {
    /// The text to place on the clipboard
    text: String,
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
    fn get_clipboard(&self) -> Result<CallToolResult, ErrorData> {
        let mut clipboard = Clipboard::new().map_err(|e| {
            ErrorData::internal_error(format!("Failed to access clipboard: {e}"), None)
        })?;

        match clipboard.get_text() {
            Ok(text) if text.is_empty() => Ok(CallToolResult::success(vec![Content::text(
                "[clipboard is empty]",
            )])),
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(arboard::Error::ContentNotAvailable) => {
                Ok(CallToolResult::success(vec![Content::text(
                    "[clipboard contains non-text content]",
                )]))
            }
            Err(e) => Err(ErrorData::internal_error(
                format!("Failed to read clipboard: {e}"),
                None,
            )),
        }
    }

    #[tool(description = "Set text content to the system clipboard")]
    fn set_clipboard(
        &self,
        Parameters(args): Parameters<SetClipboardArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut clipboard = Clipboard::new().map_err(|e| {
            ErrorData::internal_error(format!("Failed to access clipboard: {e}"), None)
        })?;

        clipboard.set_text(&args.text).map_err(|e| {
            ErrorData::internal_error(format!("Failed to set clipboard: {e}"), None)
        })?;

        let len = args.text.len();
        let preview = if len > 100 {
            &args.text[..100]
        } else {
            &args.text
        };
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Clipboard set ({len} chars): \"{preview}\""
        ))]))
    }
}

#[tool_handler]
impl ServerHandler for ClipboardServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Cross-platform MCP server for system clipboard access. \
                 Use get_clipboard to read and set_clipboard to write."
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
