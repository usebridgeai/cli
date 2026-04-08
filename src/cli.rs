// Bridge CLI - One CLI. Any storage. Every agent.
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
    about = "One CLI. Any storage. Every agent.",
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
        /// URI of the data source (e.g., file://./data, postgres://localhost/db)
        #[arg(value_hint = ValueHint::Url)]
        uri: String,

        /// Name for this provider connection
        #[arg(long = "as")]
        name: String,
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

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}
