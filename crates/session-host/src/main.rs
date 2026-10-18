use anyhow::{Context, Result};
use pigade_session_host::{Command, Launch};
use pigade_terminal::Geometry;
use std::path::PathBuf;
use uuid::Uuid;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("PigADE {} (stage-0 prototype)", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("detect-providers") => {
            anyhow::ensure!(args.len() == 1, "detect-providers takes no arguments");
            println!(
                "{}",
                serde_json::to_string(&pigade_session_host::detect_providers())?
            );
            Ok(())
        }
        Some("host-inspect") => {
            anyhow::ensure!(args.len() == 2, "host-inspect SOCKET");
            let result =
                pigade_session_host::maintenance_client::inspect(std::path::Path::new(&args[1]))?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        Some("host-shutdown-idle") => {
            anyhow::ensure!(
                args.len() == 3,
                "host-shutdown-idle SOCKET JSON_EXPECTED_INSPECTION"
            );
            let expected: pigade_session_host::maintenance_client::Inspection =
                serde_json::from_str(&args[2]).context("invalid expected host inspection")?;
            let result = pigade_session_host::maintenance_client::shutdown(
                std::path::Path::new(&args[1]),
                &expected,
            )?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        Some("host-reconcile") => {
            anyhow::ensure!(
                args.len() == 3,
                "host-reconcile SOCKET JSON_EXPECTED_INSPECTION"
            );
            let expected: pigade_session_host::maintenance_client::Inspection =
                serde_json::from_str(&args[2]).context("invalid expected host inspection")?;
            let result = pigade_session_host::maintenance_client::reconcile(
                std::path::Path::new(&args[1]),
                &expected,
            )?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        Some("serve") => {
            let root = PathBuf::from(
                args.get(1)
                    .context("serve STATE_DIRECTORY [--runtime DIRECTORY]")?,
            );
            match &args[2..] {
                [] => pigade_session_host::serve(root),
                [flag, runtime] if flag == "--runtime" => {
                    pigade_session_host::serve_with_runtime(root, PathBuf::from(runtime))
                }
                _ => anyhow::bail!("serve STATE_DIRECTORY [--runtime DIRECTORY]"),
            }
        }
        Some("__git_commit_run") => {
            anyhow::ensure!(args.len() == 2, "invalid Git commit helper invocation");
            pigade_session_host::run_git_commit_helper(std::path::Path::new(&args[1]))
        }
        Some("__git_commit_hook") => {
            anyhow::ensure!(args.len() >= 3, "invalid Git hook helper invocation");
            let code = pigade_session_host::git_commit_hook_helper(
                std::path::Path::new(&args[1]),
                &args[2],
                &args[3..],
            )?;
            std::process::exit(code);
        }
        Some("__helper") => pigade_session_host::helper(
            std::path::Path::new(args.get(1).context("socket")?),
            args.get(2).context("nonce")?,
            std::path::Path::new(args.get(3).context("executable")?),
            &args[4..],
        ),
        Some("replay") => {
            println!(
                "{}",
                serde_json::to_string(&pigade_session_host::replay_journal(
                    std::path::Path::new(args.get(1).context("replay JOURNAL")?)
                )?)?
            );
            Ok(())
        }
        Some("verify-recovery") => {
            println!(
                "{}",
                serde_json::to_string(&pigade_session_host::verify_recovery(
                    std::path::Path::new(args.get(1).context("verify-recovery JOURNAL")?)
                )?)?
            );
            Ok(())
        }
        Some("list") => call(&args, Command::List),
        Some("launch") => {
            let cwd = std::fs::canonicalize(
                args.get(2)
                    .context("launch SOCKET CWD EXECUTABLE [ARGS...]")?,
            )?;
            let executable = std::fs::canonicalize(args.get(3).context("executable")?)?;
            call(
                &args,
                Command::Launch {
                    spec: Launch {
                        env: Default::default(),
                        session: Uuid::new_v4(),
                        generation: Uuid::new_v4(),
                        operation: Uuid::new_v4(),
                        executable,
                        args: args[4..].to_vec(),
                        cwd,
                        geometry: Geometry::new(24, 80)?,
                    },
                },
            )
        }
        Some("request") => call(
            &args,
            serde_json::from_str(args.get(2).context("request SOCKET JSON_COMMAND")?)?,
        ),
        _ => anyhow::bail!(
            "PigADE stage-0 host (not MVP)\nserve STATE_DIRECTORY [--runtime PRIVATE_DIRECTORY]\nlist SOCKET\nlaunch SOCKET CWD /absolute/executable [ARGS...]\nrequest SOCKET JSON_COMMAND\nhost-inspect SOCKET\nhost-shutdown-idle SOCKET JSON_EXPECTED_INSPECTION\nhost-reconcile SOCKET JSON_EXPECTED_INSPECTION\nreplay JOURNAL\nverify-recovery JOURNAL"
        ),
    }
}
fn call(args: &[String], command: Command) -> Result<()> {
    let response = pigade_session_host::request(
        std::path::Path::new(args.get(1).context("socket path")?),
        command,
    )?;
    println!("{}", serde_json::to_string(&response)?);
    if let Some(error) = response.error {
        anyhow::bail!(error);
    }
    Ok(())
}
