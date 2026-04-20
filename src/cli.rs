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
}
