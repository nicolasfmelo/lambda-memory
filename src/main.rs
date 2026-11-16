//! CLI composition root: resident MCP runtime and operator maintenance commands.
#![recursion_limit = "256"]
use lambda_memory::integrations::provider_config::ProviderConfig;
#[cfg(all(feature = "memory-runtime", feature = "router-http"))]
mod runtime_embedding_recovery;
#[cfg(feature = "memory-runtime")]
mod runtime_identity_merge;
#[cfg(feature = "memory-runtime")]
mod runtime_launcher;
#[cfg(feature = "memory-runtime")]
mod runtime_lesson_backfill;
#[cfg(feature = "memory-runtime")]
mod runtime_promotion_migration;
#[cfg(feature = "memory-runtime")]
mod runtime_recovery;
#[cfg(feature = "memory-runtime")]
mod runtime_reflection_backfill;
#[cfg(feature = "memory-runtime")]
mod runtime_timeline_backfill;
#[cfg(feature = "memory-runtime")]
mod runtime_tool_metadata;
#[cfg(all(feature = "memory-runtime", feature = "router-http"))]
mod runtime_tree_migration;

const USAGE: &str = "\
lambda-memory: local, source-bound memory service for coding agents.

Usage: lambda-memory <COMMAND>

Commands:
  --serve-mcp                   Run the resident MCP runtime over stdio
  --inspect-dream-recovery      Print the sources blocked by a failed consolidation
  --recover-dream-sources       Retry one approved blocked batch (approval JSON on stdin)
  --inspect-embedding-recovery  Print interrupted embedding admissions for the project
  --recover-embeddings          Retry one approved embedding recovery (approval JSON on stdin)
  --inspect-embedding-index     Print the current embedding space and batch limit
  --prepare-embedding-index     Embed one approved batch into the current space (approval JSON on stdin)
  --reject-tool-metadata        List legacy tool-metadata memories; add --apply to retire them
  --inspect-tree                Print the flat memories a tree migration would curate
  --curate-existing             Curate the approved flat memories into the tree (approval JSON on stdin)
  --backfill-timeline           Dry run of automatic timeline entries for past decisions; add --apply to write
  --inspect-lesson-backfill     Print the historical sessions a lesson backfill would mark ready
  --lesson-backfill             Mark the approved historical sessions ready (approval JSON on stdin)
  --inspect-reflection-backfill Print the branches a reflection backfill would schedule
  --reflection-backfill         Schedule the approved reflection backfill (approval JSON on stdin)
  --inspect-sources             Print this store's sources for an identity merge
  --export-sources              Export sources for an approved identity merge
  --import-sources              Import exported sources into this store (JSON lines on stdin)
  --inspect-promotions          Print the preferences and decisions a user-memory migration would classify
  --promote-existing            Classify the approved nodes for the user store (approval JSON on stdin)
  --export-forwards             Export pending user-store copies for an offline transfer
  --forward-done                Acknowledge transferred copies (JSON on stdin)
  --inspect-copies              Report which promoted copies are active in the user store
  --retire-promoted             Retire project nodes whose user copy is active (approval JSON on stdin)
  --show-provider-config        Print the pinned OpenRouter model profile
  -h, --help                    Print this help
  --version                     Print the version

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
        "--serve-mcp"
        | "--inspect-dream-recovery"
        | "--recover-dream-sources"
        | "--reject-tool-metadata" => run_runtime_command(command),
        "--inspect-embedding-recovery"
        | "--recover-embeddings"
        | "--inspect-embedding-index"
        | "--prepare-embedding-index" => run_embedding_command(command),
        "--inspect-tree" | "--curate-existing" => run_tree_command(command),
        "--backfill-timeline"
        | "--inspect-lesson-backfill"
        | "--lesson-backfill"
        | "--inspect-reflection-backfill"
        | "--reflection-backfill"
        | "--inspect-sources"
        | "--export-sources"
        | "--import-sources"
        | "--inspect-promotions"
        | "--promote-existing"
        | "--export-forwards"
        | "--forward-done"
        | "--inspect-copies"
        | "--retire-promoted" => run_operator_command(command),
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
            "--reject-tool-metadata" => runtime.block_on(runtime_tool_metadata::run()),
            _ => runtime.block_on(runtime_recovery::run(false)),
        });
    if let Err(reason) = result {
        if command == "--serve-mcp" {
            eprintln!("{reason}");
        } else if command == "--reject-tool-metadata" {
            eprintln!("{reason}; no automatic retry");
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

#[cfg(all(feature = "memory-runtime", feature = "router-http"))]
fn run_embedding_command(command: &str) {
    use lambda_memory::domain_models::runtime::RuntimeError;
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| RuntimeError::StorageUnavailable)
        .and_then(|runtime| runtime.block_on(runtime_embedding_recovery::run(command)));
    if let Err(reason) = result {
        eprintln!("{reason}; no automatic recovery retry");
        std::process::exit(2);
    }
}

#[cfg(not(all(feature = "memory-runtime", feature = "router-http")))]
fn run_embedding_command(command: &str) {
    eprintln!("{command} requires the memory-runtime and router-http features");
    std::process::exit(2);
}

#[cfg(all(feature = "memory-runtime", feature = "router-http"))]
fn run_tree_command(command: &str) {
    use lambda_memory::domain_models::runtime::RuntimeError;
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| RuntimeError::StorageUnavailable)
        .and_then(|runtime| {
            runtime.block_on(runtime_tree_migration::run(command == "--inspect-tree"))
        });
    if let Err(reason) = result {
        eprintln!("{reason}; no automatic retry");
        std::process::exit(2);
    }
}

#[cfg(not(all(feature = "memory-runtime", feature = "router-http")))]
fn run_tree_command(command: &str) {
    eprintln!("{command} requires the memory-runtime and router-http features");
    std::process::exit(2);
}

/// Consented operator maintenance: backfills, identity merges and the user-memory migration.
#[cfg(feature = "memory-runtime")]
fn run_operator_command(command: &str) {
    use lambda_memory::domain_models::runtime::RuntimeError;
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| RuntimeError::StorageUnavailable)
        .and_then(|runtime| match command {
            "--backfill-timeline" => runtime.block_on(runtime_timeline_backfill::run()),
            "--inspect-lesson-backfill" | "--lesson-backfill" => runtime.block_on(
                runtime_lesson_backfill::run(command == "--inspect-lesson-backfill"),
            ),
            "--inspect-reflection-backfill" | "--reflection-backfill" => runtime.block_on(
                runtime_reflection_backfill::run(command == "--inspect-reflection-backfill"),
            ),
            "--inspect-sources" | "--export-sources" | "--import-sources" => {
                runtime.block_on(runtime_identity_merge::run(command))
            }
            _ => runtime.block_on(runtime_promotion_migration::run(command)),
        });
    if let Err(reason) = result {
        eprintln!("{reason}; no automatic retry");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "memory-runtime"))]
fn run_operator_command(command: &str) {
    eprintln!("{command} requires the memory-runtime feature");
    std::process::exit(2);
}
