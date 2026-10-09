//! Startup options shared by the DynamoDB HTTP server and the MCP HTTP
//! transport.
//!
//! Lives outside both server modules because either can be compiled without
//! the other (`mcp-server` without `http-server` is a supported build).

/// Options controlling the output of the DynamoDB HTTP server
/// (`server::start_with_options`) and the MCP HTTP transport
/// (`mcp::serve_http_with_options`).
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    suppress_informational_messages: bool,
}

impl ServerOptions {
    /// Suppress informational server output: the listening lines and the
    /// shutdown line. Warnings and errors still print.
    pub const fn quiet() -> Self {
        Self {
            suppress_informational_messages: true,
        }
    }

    pub(crate) const fn suppresses_informational_messages(&self) -> bool {
        self.suppress_informational_messages
    }
}
