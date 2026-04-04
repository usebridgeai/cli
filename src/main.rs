// Bridge CLI - One CLI. Any storage. Every agent.
// Copyright (c) 2026 Gabriel Beslic & Tomer Liran
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

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let timeout = cli.timeout;

    let result = match cli.command {
        Commands::Init => commands::init::execute().await,
        Commands::Connect { uri, name } => commands::connect::execute(uri, name).await,
        Commands::Remove { name } => commands::remove::execute(name).await,
        Commands::Status => commands::status::execute(timeout).await,
        Commands::Ls { from } => commands::ls::execute(from, timeout).await,
        Commands::Read { path, from, limit } => {
            commands::read::execute(path, from, limit, timeout).await
        }
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "bridge", &mut std::io::stdout());
            return;
        }
    };

    if let Err(e) = result {
        eprintln!("{}", serde_json::to_string_pretty(&e.to_json()).unwrap());
        std::process::exit(e.exit_code());
    }
}
