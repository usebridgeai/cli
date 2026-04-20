// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License version 3
// as published by the Free Software Foundation.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use clap::{Parser, Subcommand, ValueHint};
use clap_complete::Shell;

#[derive(Parser, Debug)]
#[command(
    name = "bridge",
    version,
    about = "Any storage. Any agent. One CLI",
    long_about = "Bridge is a unified CLI that lets AI agents read context from any data source through a single interface."
)]
pub struct Cli {
    /// Timeout for provider operations in seconds
    #[arg(long, default_value_t = 30, global = true)]
    pub timeout: u64,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize a new bridge.yaml in the current directory
    Init,

    /// Add a provider connection
    Connect {
        /// Connection target: a literal URI or an environment variable name
        #[arg(value_name = "target", value_hint = ValueHint::Other)]
        target: String,

        /// Provider type. Required when <target> is an environment variable name.
        #[arg(long = "type")]
        provider_type: Option<String>,

        /// Name for this provider connection
        #[arg(long = "as")]
        name: String,

        /// Overwrite an existing connection with the same name
        #[arg(long)]
        force: bool,

        /// Skip connectivity verification and save the connection as-is
        #[arg(long = "no-verify")]
        no_verify: bool,
    },

    /// Remove a provider connection
    Remove {
        /// Name of the provider to remove
        name: String,
    },

    /// Show connection health for all providers
    Status,

    /// List contents of a provider
    Ls {
        /// Provider to list from (if omitted, lists all)
        #[arg(long)]
        from: Option<String>,
    },

    /// Read context from a provider
    Read {
        /// Path to read (file path for filesystem, table[/pk] for postgres)
        #[arg(value_hint = ValueHint::AnyPath)]
        path: String,

        /// Provider to read from
        #[arg(long)]
        from: String,

        /// Maximum number of rows to return (database providers only)
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },

    /// Update bridge to the latest version
    Update {
        /// Check for available updates without installing
        #[arg(long)]
        check: bool,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },

    /// Generate artifacts from a source (e.g. an MCP manifest from OpenAPI)
    Generate {
        #[command(subcommand)]
        target: GenerateTarget,
    },

    /// MCP (Model Context Protocol) commands
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum GenerateTarget {
    /// Generate a bridge.mcp/v1 manifest
    Mcp {
        /// Source kind. Use `--from openapi <path>` for an OpenAPI spec, or
        /// `--from db` together with `--connection <name>` for a Bridge DB
        /// connection.
        #[arg(long = "from", num_args = 1..=2, value_names = ["KIND", "PATH"])]
        from: Vec<String>,

        /// Name of a Bridge connection (from bridge.yaml) to introspect.
        /// Required when `--from db`.
        #[arg(long = "connection")]
        connection: Option<String>,

        /// Schema to introspect. Defaults to `public` for Postgres.
        #[arg(long = "schema")]
        schema: Option<String>,

        /// Name for the generated MCP server
        #[arg(long)]
        name: String,

        /// Environment variable holding the API base URL (OpenAPI only)
        #[arg(long = "base-url-env")]
        base_url_env: Option<String>,

        /// Environment variable holding the bearer token for the API (OpenAPI only)
        #[arg(long = "bearer-env")]
        bearer_env: Option<String>,

        /// Output path for the generated manifest
        #[arg(long, value_hint = ValueHint::FilePath)]
        out: String,

        /// Overwrite an existing manifest at `--out`
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum McpAction {
    /// Serve an MCP manifest as a live MCP server over stdio
    Serve {
        /// Path to the bridge.mcp/v1 manifest
        #[arg(value_hint = ValueHint::FilePath)]
        manifest: String,
    },

    /// Serve an MCP manifest remotely over HTTP (Streamable HTTP transport)
    ServeHttp {
        /// Path to the bridge.mcp/v1 manifest
        #[arg(value_hint = ValueHint::FilePath)]
        manifest: String,

        /// Address to bind (host:port). Defaults to 127.0.0.1:8080
        #[arg(long, env = "BRIDGE_MCP_BIND", default_value = "127.0.0.1:8080")]
        bind: String,

        /// Public base URL advertised in health output and startup logs. Bridge
        /// appends `/mcp`, `/healthz`, and `/readyz` to this value.
        #[arg(long = "public-url", env = "BRIDGE_MCP_PUBLIC_URL")]
        public_url: Option<String>,

        /// Maximum HTTP header bytes accepted per request.
        #[arg(
            long = "max-header-bytes",
            env = "BRIDGE_MCP_MAX_HEADER_BYTES",
            default_value_t = 32 * 1024
        )]
        max_header_bytes: usize,

        /// Maximum HTTP body bytes accepted per request.
        #[arg(
            long = "max-body-bytes",
            env = "BRIDGE_MCP_MAX_BODY_BYTES",
            default_value_t = 1024 * 1024
        )]
        max_body_bytes: usize,

        /// Wall-clock budget for reading a single HTTP request off the wire.
        #[arg(
            long = "read-timeout-secs",
            env = "BRIDGE_MCP_READ_TIMEOUT_SECS",
            default_value_t = 15
        )]
        read_timeout_secs: u64,

        /// Wall-clock budget for handling one HTTP request after it is read.
        /// Defaults to the global `--timeout` if omitted.
        #[arg(long = "request-timeout-secs", env = "BRIDGE_MCP_REQUEST_TIMEOUT_SECS")]
        request_timeout_secs: Option<u64>,

        /// Time to wait for in-flight requests during shutdown before forcing
        /// them closed.
        #[arg(
            long = "shutdown-grace-secs",
            env = "BRIDGE_MCP_SHUTDOWN_GRACE_SECS",
            default_value_t = 10
        )]
        shutdown_grace_secs: u64,

        /// Additional Origin values to accept in addition to loopback. Repeat
        /// for multiple. Requests with no Origin header are always allowed
        /// (non-browser clients do not send one); browser-originated requests
        /// must match loopback or an entry passed here.
        #[arg(long = "allow-origin", value_name = "ORIGIN")]
        allow_origin: Vec<String>,
    },
}
