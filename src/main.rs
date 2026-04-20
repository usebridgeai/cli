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
mod mcp;
mod provider;
mod update;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands, GenerateTarget, McpAction};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let timeout = cli.timeout;
    use std::io::IsTerminal;
    let should_check_updates =
        should_run_passive_update_check(&cli.command, std::io::stderr().is_terminal());

    // Check for available updates from the local cache (no blocking I/O).
    // If the cache is stale, a background HTTP fetch is started concurrently
    // with the main command and awaited (with timeout) before exit.
    let mut update_notice = if should_check_updates {
        update::check_update_notice()
    } else {
        update::UpdateNotice {
            version: None,
            refresh: None,
        }
    };

    let result = match cli.command {
        Commands::Init => commands::init::execute().await,
        Commands::Connect {
            target,
            provider_type,
            name,
            force,
            no_verify,
        } => {
            commands::connect::execute(target, provider_type, name, force, no_verify, timeout).await
        }
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
        Commands::Generate { target } => match target {
            GenerateTarget::Mcp {
                from,
                connection,
                schema,
                name,
                base_url_env,
                bearer_env,
                out,
                force,
            } => {
                commands::generate::execute_mcp(
                    from,
                    connection,
                    schema,
                    name,
                    base_url_env,
                    bearer_env,
                    out,
                    force,
                    timeout,
                )
                .await
            }
        },
        Commands::Mcp { action } => match action {
            McpAction::Serve { manifest } => commands::mcp::execute_serve(manifest, timeout).await,
        },
    };

    if let Err(e) = result {
        eprintln!("{}", serde_json::to_string_pretty(&e.to_json()).unwrap());
        // Skip the update notice on error — stderr must stay valid JSON,
        // and the user cares about the error, not the upgrade prompt.
        update::wait_for_refresh(&mut update_notice).await;
        std::process::exit(e.exit_code());
    }

    // Print update notice to stderr for interactive sessions only.
    // Suppressed when: non-TTY (agents, pipes, CI), BRIDGE_NO_UPDATE_CHECK is set,
    // or the user is already running `bridge update`.
    if should_check_updates {
        print_update_notice(update_notice.version.as_deref());
    }

    // Give the background cache refresh a chance to finish before the runtime
    // shuts down. This runs after all output is printed, so the user sees no
    // delay for fast commands. Capped at 500 ms — if GitHub is slow we just
    // skip; the cache will be refreshed on the next invocation.
    update::wait_for_refresh(&mut update_notice).await;
}

fn print_update_notice(version: Option<&str>) {
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

fn should_run_passive_update_check(command: &Commands, stderr_is_terminal: bool) -> bool {
    if !stderr_is_terminal {
        return false;
    }

    !matches!(
        command,
        Commands::Update { .. }
            | Commands::Completions { .. }
            | Commands::Mcp {
                action: McpAction::Serve { .. }
            }
    )
}

#[cfg(test)]
mod tests {
    use super::should_run_passive_update_check;
    use crate::cli::{Commands, GenerateTarget, McpAction};
    use clap_complete::Shell;

    #[test]
    fn passive_update_check_is_disabled_for_mcp_serve() {
        let command = Commands::Mcp {
            action: McpAction::Serve {
                manifest: "petstore.mcp.yaml".into(),
            },
        };

        assert!(!should_run_passive_update_check(&command, true));
    }

    #[test]
    fn passive_update_check_requires_a_terminal() {
        let command = Commands::Generate {
            target: GenerateTarget::Mcp {
                from: vec!["openapi".into(), "spec.yaml".into()],
                connection: None,
                schema: None,
                name: "petstore".into(),
                base_url_env: None,
                bearer_env: None,
                out: "out.yaml".into(),
                force: false,
            },
        };

        assert!(!should_run_passive_update_check(&command, false));
        assert!(should_run_passive_update_check(&command, true));
    }

    #[test]
    fn passive_update_check_is_disabled_for_completions() {
        let command = Commands::Completions { shell: Shell::Bash };

        assert!(!should_run_passive_update_check(&command, true));
    }
}
