//! DDK CLI – command-line interface for Delphi project management.
//!
//! Thin wrapper around `ddk_core::commands`. Shares the same RON-based
//! state as ddk-server (LSP) and ddk-mcp-server, so changes made via the
//! CLI are automatically picked up by the other tools.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, Write};

use ddk_core::commands;
use ddk_core::commands::CompileFilterOptions;
use ddk_core::projects::{CompilerConfigurations, ProjectsData};
use ddk_core::state::Stateful;

/// Whether a compile TARGET should be treated as a project file (ad-hoc
/// compile) rather than a project id/name reference. Decided purely by
/// extension so a bare name like "be" or "123" is never mistaken for a file.
fn is_project_file(target: &str) -> bool {
    let lower = target.to_lowercase();
    lower.ends_with(".dproj") || lower.ends_with(".dpr") || lower.ends_with(".dpk")
}

/// Whether a `run` TARGET should be treated as a file path (project file or
/// executable) rather than a project id/name reference.
fn is_run_target_file(target: &str) -> bool {
    is_project_file(target) || target.to_lowercase().ends_with(".exe")
}

/// DDK – Delphi Development Kit CLI
#[derive(Parser)]
#[command(name = "ddk", version, about, long_about = None)]
struct Cli {
    /// Output results as JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage Delphi projects.
    #[command(subcommand, name = "project", visible_alias = "projects")]
    Project(ProjectCmd),

    /// Manage Delphi compiler configurations.
    #[command(subcommand)]
    Compiler(CompilerCmd),

    /// Compile a project. Compiles the active project by default.
    ///
    /// TARGET may be a project ID, a project name (same as --project), or a
    /// path to a .dproj/.dpr/.dpk. A path is compiled ad-hoc (without adding it
    /// to a workspace); choose its compiler with --compiler.
    Compile {
        /// What to compile: a project ID, a project name, or a path to a
        /// .dproj/.dpr/.dpk. Anything ending in .dproj/.dpr/.dpk is treated as
        /// a file (ad-hoc compile); otherwise as a project ID or name.
        /// Mutually exclusive with --project.
        #[arg(conflicts_with = "project")]
        target: Option<String>,

        /// Rebuild from scratch instead of incremental compile.
        #[arg(long)]
        rebuild: bool,

        /// Project to compile: a numeric ID or a project name. A name that
        /// matches several projects lists the candidates instead of compiling.
        #[arg(long, short)]
        project: Option<String>,

        /// Compiler configuration for an ad-hoc file TARGET: an exact key
        /// (e.g. "12.0") or product name (e.g. "Delphi 12"). Defaults to the
        /// newest installed compiler. Only meaningful when TARGET is a file.
        #[arg(long, short = 'c', requires = "target")]
        compiler: Option<String>,

        /// Build configuration override for an ad-hoc file TARGET
        /// (e.g. "Debug", "Release"). Only meaningful when TARGET is a file.
        #[arg(long, requires = "target")]
        config: Option<String>,

        /// Target platform override for an ad-hoc file TARGET
        /// (e.g. "Win32", "Win64"). Only meaningful when TARGET is a file.
        #[arg(long, requires = "target")]
        platform: Option<String>,

        /// Show warning lines verbatim instead of suppressing them.
        #[arg(long)]
        show_warnings: bool,

        /// Show hint lines verbatim instead of suppressing them.
        #[arg(long)]
        show_hints: bool,

        /// Emit a per-file `<file>: X warn, Y hint` summary for any
        /// warnings/hints that were not shown verbatim.
        #[arg(long)]
        summarize_diagnostics: bool,

        /// Encoding used to decode compiler output, e.g. "utf-8",
        /// "windows-1252", "oem". Defaults to "oem", which auto-detects the
        /// active console output codepage (what `chcp` sets). Overrides the
        /// DDK_COMPILER_ENCODING environment variable.
        #[arg(long, short = 'e')]
        encoding: Option<String>,

        /// Exit with a non-zero process code when the compile fails (mirrors
        /// the JSON `code`). Off by default so existing callers that only parse
        /// JSON keep working.
        #[arg(long)]
        fail_on_error: bool,

        /// Extra arguments passed verbatim to MSBuild, after `--`
        /// (e.g. `ddk compile be -- /p:DCC_Define=FOO /m`). Appended after the
        /// built-in Config/Platform args, so a `/p:` override here wins. Ignored
        /// for a bare .dpr/.dpk TARGET, which is compiled with dcc, not MSBuild.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        msbuild_args: Vec<String>,
    },

    /// Run a project's built executable directly. Runs the active project by
    /// default. Never compiles — the executable must already exist.
    ///
    /// TARGET may be a project ID, a project name (same as --project), or a
    /// path to a .dproj/.dpr/.dpk/.exe. A project-file path resolves to its
    /// managed project (like referencing it by name); a .exe path runs
    /// directly, bypassing project resolution entirely.
    Run {
        /// What to run: a project ID, a project name, or a path to a
        /// .dproj/.dpr/.dpk/.exe. Anything ending in one of those extensions
        /// is treated as a file; otherwise as a project ID or name. Mutually
        /// exclusive with --project.
        #[arg(conflicts_with = "project")]
        target: Option<String>,

        /// Project to run: a numeric ID or a project name. A name that
        /// matches several projects lists the candidates instead of running.
        #[arg(long, short)]
        project: Option<String>,

        /// Command-line arguments passed to the executable, overriding the
        /// project's saved Start Parameters for this run only.
        #[arg(long, short)]
        args: Option<String>,
    },

    /// Generate the `.delphilsp.json` settings file used by Embarcadero's
    /// DelphiLSP VS Code extension (code insight), without needing RAD Studio.
    ///
    /// TARGET may be a project ID, a project name, or a path to a
    /// .dproj/.dpr/.dpk. A path owned by no workspace is handled ad-hoc; pick
    /// its compiler with --compiler.
    #[command(name = "delphilsp-config", visible_alias = "delphilsp_config")]
    DelphiLspConfig {
        /// What to describe: a project ID, a project name, or a path to a
        /// .dproj/.dpr/.dpk. Omit to use the active project.
        target: Option<String>,

        /// Compiler configuration for an ad-hoc file TARGET: an exact key
        /// (e.g. "12.0") or product name (e.g. "Delphi 12"). Defaults to the
        /// newest installed compiler. Only meaningful when TARGET is a file
        /// that belongs to no workspace.
        #[arg(long, short = 'c')]
        compiler: Option<String>,

        /// Write the settings file here instead of next to the project's main
        /// source (useful for inspecting the output without overwriting an
        /// IDE-generated file).
        #[arg(long, short = 'o')]
        out: Option<String>,
    },

    /// Show environment info for the active project.
    Env,

    /// Print the DDK extension README.
    Info,

    /// Format a Delphi source file in-place.
    Format {
        /// Path to the file to format.
        file: String,
        /// Encoding of the source file, e.g. "utf-8", "windows-1252", "oem".
        /// Defaults to "utf-8" when not specified.
        #[arg(long, short = 'e')]
        encoding: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// List all known projects.
    List,
    /// Select a project by its numeric ID.
    Select {
        /// The project ID to select.
        id: usize,
    },
    /// Add a project file (.dproj/.dpr/.dpk) to a workspace.
    Add {
        /// Path to the project file to add.
        path: String,
        /// Target workspace name (or numeric id).
        workspace: String,
    },
    /// Create a new workspace bound to a compiler.
    #[command(name = "add_workspace", visible_alias = "add-workspace")]
    AddWorkspace {
        /// Name for the new workspace.
        name: String,
        /// Compiler key (e.g. "12.0") or product name (e.g. "Delphi 12").
        compiler: String,
    },
}

#[derive(Subcommand)]
enum CompilerCmd {
    /// List all available compiler configurations.
    List,
    /// Set the group project compiler by key.
    Set {
        /// The compiler configuration key (e.g. "12.0").
        key: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Ensure state files exist (creates defaults if first run).
    ProjectsData::initialize()?;
    CompilerConfigurations::initialize()?;

    let cli = Cli::parse();

    match cli.command {
        Commands::Info => {
            let readme = include_str!("../../README.md");
            println!("{readme}");
        }

        Commands::Env => {
            let info = commands::cmd_get_environment_info().await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print!("{info}");
            }
        }

        Commands::Project(cmd) => match cmd {
            ProjectCmd::List => {
                let result = commands::cmd_list_projects().await?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    print!("{result}");
                }
            }
            ProjectCmd::Select { id } => {
                let result = commands::cmd_select_project(id).await?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{result}");
                }
            }
            ProjectCmd::Add { path, workspace } => {
                let result = commands::cmd_add_project(path, workspace).await?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{result}");
                }
            }
            ProjectCmd::AddWorkspace { name, compiler } => {
                let result = commands::cmd_add_workspace(name, compiler).await?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{result}");
                }
            }
        },

        Commands::Compiler(cmd) => match cmd {
            CompilerCmd::List => {
                let compilers = commands::cmd_list_compilers().await?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&compilers)?);
                } else if compilers.is_empty() {
                    println!("No compiler configurations available.");
                } else {
                    for c in &compilers {
                        println!("{c}");
                    }
                }
            }
            CompilerCmd::Set { key } => {
                let result = commands::cmd_set_group_compiler(key).await?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{result}");
                }
            }
        },

        Commands::Compile {
            target,
            rebuild,
            project,
            compiler,
            config,
            platform,
            show_warnings,
            show_hints,
            summarize_diagnostics,
            encoding,
            fail_on_error,
            msbuild_args,
        } => {
            // Resolve compiler-output encoding: --encoding wins, then the
            // DDK_COMPILER_ENCODING env var, else the "oem" default (which
            // auto-detects the console output codepage at decode time).
            if let Some(enc) = encoding
                .or_else(|| std::env::var("DDK_COMPILER_ENCODING").ok())
                .filter(|e| !e.trim().is_empty())
            {
                ddk_core::encoding::set_encoding(&enc);
            }
            let filter = CompileFilterOptions {
                trim_banners: true,
                show_warnings,
                show_hints,
                summarize_diagnostics,
            };
            // A TARGET ending in a project-file extension is an ad-hoc file
            // compile; otherwise it is a project reference (id or name), exactly
            // like --project. `--project` (when no TARGET) keeps working too.
            let (file_path, project_ref) = match target {
                Some(t) if is_project_file(&t) => (Some(t), None),
                Some(t) => (None, Some(t)),
                None => (None, project),
            };
            use commands::CompileOrAmbiguity;
            // Captured for --fail-on-error: (success, code).
            let mut outcome: Option<(bool, i32)> = None;
            if cli.json {
                let result = match file_path {
                    Some(p) => {
                        commands::cmd_compile_file(
                            p, compiler, config, platform, rebuild, filter, msbuild_args,
                        )
                        .await?
                    }
                    _ => {
                        commands::cmd_compile_ref(rebuild, project_ref, filter, msbuild_args).await?
                    }
                };
                match result {
                    CompileOrAmbiguity::Output(o) => {
                        outcome = Some((o.success, o.code));
                        println!("{}", serde_json::to_string_pretty(&o)?)
                    }
                    CompileOrAmbiguity::Ambiguity(a) => {
                        println!("{}", serde_json::to_string_pretty(&a)?)
                    }
                }
            } else {
                let stdout = std::sync::Arc::new(std::sync::Mutex::new(io::stdout()));
                let on_progress: commands::CompileProgressCallback =
                    std::sync::Arc::new(move |line: String| {
                        let mut handle = stdout.lock().unwrap();
                        let _ = writeln!(handle, "{line}");
                        let _ = handle.flush();
                    });
                let result = match file_path {
                    Some(p) => {
                        commands::cmd_compile_file_with_progress(
                            p, compiler, config, platform, rebuild, filter, msbuild_args,
                            Some(on_progress),
                        )
                        .await?
                    }
                    _ => {
                        commands::cmd_compile_ref_with_progress(
                            rebuild, project_ref, filter, msbuild_args, Some(on_progress),
                        )
                        .await?
                    }
                };
                match result {
                    CompileOrAmbiguity::Output(o) => {
                        // Output already streamed live via on_progress; just
                        // capture the outcome for --fail-on-error.
                        outcome = Some((o.success, o.code));
                    }
                    CompileOrAmbiguity::Ambiguity(a) => print!("{a}"),
                }
            }
            // Opt-in: propagate a failed compile to the process exit code.
            // Default (off) keeps the historical exit 0.
            if fail_on_error {
                if let Some((success, code)) = outcome {
                    if !success {
                        let _ = io::stdout().flush();
                        // Mirror the compiler exit code (fall back to 1 if it
                        // was 0/-1 despite the failure).
                        std::process::exit(if code > 0 { code } else { 1 });
                    }
                }
            }
        }

        Commands::Run { target, project, args } => {
            // A TARGET that looks like a project file or executable is a
            // file-path run; otherwise it is a project reference (id or
            // name), exactly like --project. `--project` (when no TARGET)
            // keeps working too.
            let (file_path, project_ref) = match target {
                Some(t) if is_run_target_file(&t) => (Some(t), None),
                Some(t) => (None, Some(t)),
                None => (None, project),
            };
            use commands::RunOrAmbiguity;
            let result = match file_path {
                Some(p) => commands::cmd_run_path(p, args).await?,
                _ => commands::cmd_run_ref(project_ref, args).await?,
            };
            match result {
                RunOrAmbiguity::Output(o) => {
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&o)?);
                    } else {
                        println!("{o}");
                    }
                }
                RunOrAmbiguity::Ambiguity(a) => {
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&a)?);
                    } else {
                        print!("{a}");
                    }
                }
            }
        }

        Commands::DelphiLspConfig { target, compiler, out } => {
            use commands::DelphiLspOrAmbiguity;
            match commands::cmd_delphilsp_config(target, compiler, out).await? {
                DelphiLspOrAmbiguity::Output(result) => {
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&result)?);
                    } else {
                        println!("{result}");
                    }
                }
                DelphiLspOrAmbiguity::Ambiguity(a) => {
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&a)?);
                    } else {
                        print!("{a}");
                    }
                }
            }
        }

        Commands::Format { file, encoding } => {
            let result = commands::cmd_format_file(file, encoding).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{result}");
            }
        }
    }

    Ok(())
}
