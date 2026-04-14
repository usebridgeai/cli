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

mod cli;
mod commands;
mod config;
mod context;
mod error;
mod provider;
mod update;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let timeout = cli.timeout;

    // Suppress the passive notice when the user is already running `bridge update`
    let is_update_cmd = matches!(cli.command, Commands::Update { .. });

    // Check for available updates synchronously from cache only.
    // If the cache is stale, a background task refreshes it for the next run.
    let update_notice = update::check_update_notice();

    let result = match cli.command {
        Commands::Init => commands::init::execute().await,
        Commands::Connect {
            target,
            provider_type,
            name,
        } => commands::connect::execute(target, provider_type, name).await,
        Commands::Remove { name } => commands::remove::execute(name).await,
        Commands::Status => commands::status::execute(timeout).await,
        Commands::Ls { from } => commands::ls::execute(from, timeout).await,
        Commands::Read { path, from, limit } => {
            commands::read::execute(path, from, limit, timeout).await
        }
        Commands::Update { check } => commands::update::execute(check).await,
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "bridge", &mut std::io::stdout());
            return;
        }
    };

    // Print update notice to stderr for interactive sessions only.
    // Suppressed when: non-TTY (agents, pipes, CI), BRIDGE_NO_UPDATE_CHECK is set,
    // or the user is already running `bridge update`.
    if !is_update_cmd {
        print_update_notice(update_notice);
    }

    if let Err(e) = result {
        eprintln!("{}", serde_json::to_string_pretty(&e.to_json()).unwrap());
        std::process::exit(e.exit_code());
    }
}

fn print_update_notice(version: Option<String>) {
    use std::io::IsTerminal;
    if let Some(v) = version {
        if std::io::stderr().is_terminal() {
            eprintln!();
            eprintln!("  A new version of bridge is available: {v}");
            eprintln!("  Run `bridge update` to upgrade.");
            eprintln!();
        }
    }
}
