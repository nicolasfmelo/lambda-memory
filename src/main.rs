//! CLI composition root: resident MCP runtime and operator maintenance commands.
#![recursion_limit = "256"]
use lambda_memory::integrations::provider_config::ProviderConfig;
#[cfg(feature = "memory-runtime")]
mod runtime_launcher;
#[cfg(feature = "memory-runtime")]
mod runtime_recovery;

const USAGE: &str = "\
lambda-memory: local, source-bound memory service for coding agents.

Usage: lambda-memory <COMMAND>

Commands:
  --serve-mcp                Run the resident MCP runtime over stdio
  --inspect-dream-recovery   Print the sources blocked by a failed consolidation
  --recover-dream-sources    Retry one approved blocked batch (approval JSON on stdin)
  --show-provider-config     Print the pinned OpenRouter model profile
  -h, --help                 Print this help
  --version                  Print the version

MCP clients should launch the `lambda-memory` bridge, not this binary directly;
the bridge starts and authenticates the runtime for you.

https://github.com/nicolasfmelo/lambda-memory";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            println!("{USAGE}");
            return;
        }
        Some("--version") => {
            println!("lambda-memory {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some(command) => command,
    };
    match command {
        "--serve-mcp" | "--inspect-dream-recovery" | "--recover-dream-sources" => {
            run_runtime_command(command)
        }
        "--show-provider-config" => match ProviderConfig::from_environment() {
            Ok(providers) => println!(
                "planner={}; embedding={}; selector={}",
                providers.generator_candidates.join(","),
                providers.embedding_model,
                providers.selector_model
            ),
            Err(error) => {
                eprintln!("Configuration rejected: {error}");
                std::process::exit(2);
            }
        },
        unknown => {
            eprintln!("unknown argument: {unknown}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(feature = "memory-runtime")]
fn run_runtime_command(command: &str) {
    use lambda_memory::domain_models::runtime::RuntimeError;
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| RuntimeError::StorageUnavailable)
        .and_then(|runtime| match command {
            "--serve-mcp" => runtime.block_on(runtime_launcher::run()),
            "--inspect-dream-recovery" => runtime.block_on(runtime_recovery::run(true)),
            _ => runtime.block_on(runtime_recovery::run(false)),
        });
    if let Err(reason) = result {
        if command == "--serve-mcp" {
            eprintln!("{reason}");
        } else {
            eprintln!("{reason}; no automatic recovery retry");
        }
        std::process::exit(2);
    }
}

#[cfg(not(feature = "memory-runtime"))]
fn run_runtime_command(command: &str) {
    eprintln!("{command} requires the memory-runtime feature");
    std::process::exit(2);
}
