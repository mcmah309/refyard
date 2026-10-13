//! Command-line interface for shared resource discovery, lookup, validation, and cleanup.

use std::{
    fs,
    io::{self, Write},
    path::{PathBuf, absolute},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use eros::{Context, Result, context};
use serde::Serialize;

use refyard::{
    cleanup::cleanup,
    configuration::resolve_locations,
    discovery::{ScanOptions, register, scan},
    errors::{user_error, user_message},
    graph::{references, resource_statuses, validate},
    manifest::{Artifact, ResourceKey, ensure_registry, load_registry, validate_resources},
    storage::{
        load_index, load_repositories, lock_state, physical_path, save_repositories, write_yaml,
    },
};

// Command data
//------------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Track shared file and directory dependencies across repositories"
)]
struct Cli {
    /// Override the registry in the system's per-user Refyard configuration directory.
    #[arg(long, global = true, env = "REFYARD_REGISTRY")]
    registry: Option<PathBuf>,
    /// Override the system's per-user Refyard state directory.
    #[arg(long, global = true, env = "REFYARD_STATE_DIR")]
    state_dir: Option<PathBuf>,
    /// Emit JSON instead of human-readable YAML.
    #[arg(long, global = true)]
    json: bool,
    /// Also print internal error diagnostics on stderr; may include paths and source details.
    #[arg(long, global = true)]
    debug_errors: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage durable repository registrations.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Manage local resource mappings. Does not download or create resources.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Discover manifests under all saved roots and rebuild the index.
    Scan {
        /// Additional roots to remember and scan, alongside all previously saved roots.
        roots: Vec<PathBuf>,
        /// Authorize cleanup after a complete, error-free scan of all configured roots.
        #[arg(long, conflicts_with_all = ["respect_gitignore", "ignore_hidden"])]
        full: bool,
        /// Honor .gitignore, Git excludes, and global Git ignores during discovery.
        #[arg(long)]
        respect_gitignore: bool,
        /// Skip hidden entries during discovery.
        #[arg(long)]
        ignore_hidden: bool,
    },
    /// List indexed project dependencies.
    Projects,
    /// List registered resources with indexed dependents and current availability.
    Resources,
    /// Find projects that reference this resource ID.
    Refs {
        id: String,
        /// Restrict the lookup to this exact revision; omitted means all revisions.
        #[arg(long)]
        revision: Option<String>,
    },
    /// Show the indexed dependency graph, including scan errors and unresolved IDs.
    Graph,
    /// Validate live manifests and registry paths. Exits 1 if any warnings are found.
    Validate,
    /// Preview unreferenced registered locations, or explicitly delete eligible resources.
    Cleanup {
        /// Delete resources after verifying a prior full scan and repeating it successfully.
        #[arg(long)]
        delete: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RepoCommand {
    /// Register repository directories or refyard.yaml files.
    Add {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Forget a repository, including one that is no longer available.
    Remove { path: PathBuf },
    /// Show durable repositories and filesystem scan roots.
    List,
}

#[derive(Debug, Subcommand)]
enum RegistryCommand {
    /// Add a unique resource identity mapped to an existing file or directory.
    Add {
        id: String,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        notes: Option<String>,
    },
    /// Remove a mapping without touching its file or directory.
    Remove {
        id: String,
        #[arg(long)]
        revision: Option<String>,
    },
    /// Show the authoritative machine registry.
    List,
}

// Functions
//------------------------------------------------------------------------------

fn main() -> ExitCode {
    let cli = Cli::parse();
    let debug_errors = cli.debug_errors;
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {}", user_message(&error));
            if debug_errors {
                eprintln!("diagnostics: {error:?}");
            }
            ExitCode::FAILURE
        }
    }
}

#[context("execute Refyard command")]
fn run(cli: Cli) -> Result<ExitCode> {
    let locations = resolve_locations(cli.registry.as_deref(), cli.state_dir.as_deref())?;
    let state_directory = absolute(&locations.state_directory)?;
    let _lock = lock_state(&state_directory)?;
    ensure_registry(&locations.registry)?;
    let registry_path = physical_path(&locations.registry)?;
    let mut repositories = load_repositories(&state_directory)?;
    match cli.command {
        Command::Repo { command } => {
            match command {
                RepoCommand::Add { paths } => {
                    for path in paths {
                        register(&mut repositories, &path)?;
                    }
                    save_repositories(&state_directory, &repositories)?;
                }
                RepoCommand::Remove { path } => {
                    let path = absolute(path)?;
                    let path = if path.file_name().is_some_and(|name| name == "refyard.yaml")
                        && !path.is_dir()
                    {
                        physical_path(path.parent().expect("manifest has a parent"))?
                    } else {
                        physical_path(&path)?
                    };
                    let previous_count = repositories.repositories.len();
                    repositories
                        .repositories
                        .retain(|repository| repository != &path);
                    if previous_count == repositories.repositories.len() {
                        return Err(user_error(format!(
                            "Repository is not registered: {}. Run `refyard repo list` to see known projects.",
                            path.display()
                        )));
                    }
                    save_repositories(&state_directory, &repositories)?;
                }
                RepoCommand::List => {}
            }
            print_output(&repositories, cli.json)?;
        }
        Command::Registry { command } => {
            let mut registry = load_registry(&registry_path)?;
            match command {
                RegistryCommand::Add {
                    id,
                    revision,
                    path,
                    notes,
                } => {
                    let path = fs::canonicalize(&path)
                        .with_user_context(|| format!("Could not add resource at {}. Supply an existing, accessible location.", path.display()))?;
                    if !path.is_file() && !path.is_dir() {
                        return Err(user_error("Resource must be a regular file or directory."));
                    }
                    registry.artifacts.push(Artifact {
                        resource: ResourceKey { id, revision },
                        path,
                        notes,
                    });
                    validate_resources(
                        registry.artifacts.iter().map(|artifact| &artifact.resource),
                    )?;
                    write_yaml(&registry_path, &registry).user_context(
                        "Could not save the registry. Check directory permissions and free space.",
                    )?;
                }
                RegistryCommand::Remove { id, revision } => {
                    let key = ResourceKey { id, revision };
                    let previous_count = registry.artifacts.len();
                    registry
                        .artifacts
                        .retain(|artifact| artifact.resource != key);
                    if registry.artifacts.len() == previous_count {
                        return Err(user_error(format!(
                            "resource identity is not registered: {} revision {:?}",
                            key.id, key.revision
                        )));
                    }
                    write_yaml(&registry_path, &registry).user_context(
                        "Could not save the registry. Check directory permissions and free space.",
                    )?;
                }
                RegistryCommand::List => {}
            }
            print_output(&registry, cli.json)?;
        }
        Command::Scan {
            roots,
            full,
            respect_gitignore,
            ignore_hidden,
        } => {
            let index = scan(
                &state_directory,
                &mut repositories,
                &roots,
                ScanOptions {
                    full,
                    respect_gitignore,
                    ignore_hidden,
                },
            )?;
            print_output(&index, cli.json)?;
            if !index.issues.is_empty() {
                return Ok(ExitCode::FAILURE);
            }
        }
        Command::Projects => {
            let index = load_index(&state_directory, &repositories)?;
            print_output(&index.projects, cli.json)?;
        }
        Command::Resources => {
            let index = load_index(&state_directory, &repositories)?;
            let registry = load_registry(&registry_path)?;
            print_output(
                &resource_statuses(&registry_path, &registry, &index.projects),
                cli.json,
            )?;
        }
        Command::Refs { id, revision } => {
            let index = load_index(&state_directory, &repositories)?;
            let registry = load_registry(&registry_path)?;
            print_output(
                &references(&index, &registry, &id, revision.as_deref()),
                cli.json,
            )?;
        }
        Command::Graph => print_output(&load_index(&state_directory, &repositories)?, cli.json)?,
        Command::Validate => {
            let registry = load_registry(&registry_path)?;
            let report = validate(&repositories, &registry_path, &registry);
            print_output(&report, cli.json)?;
            if !report.issues.is_empty() {
                return Ok(ExitCode::FAILURE);
            }
        }
        Command::Cleanup { delete } => {
            print_output(
                &cleanup(&state_directory, &registry_path, delete)?,
                cli.json,
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_output(value: &impl Serialize, json: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut stdout, value)?;
        writeln!(stdout)?;
    } else {
        stdout.write_all(yaml_serde::to_string(value)?.as_bytes())?;
    }
    Ok(())
}
