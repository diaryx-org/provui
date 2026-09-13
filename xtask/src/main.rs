//! `cargo xtask <job>` — one CI job, run the same way locally and on a runner.
//!
//! The workflow in `.github/workflows/ci.yml` contains no job definitions. It
//! asks `cargo xtask ci-matrix` what the jobs are and runs `cargo xtask <id>`
//! for each, so adding a job, renaming one, or changing what it runs is an edit
//! to [`JOBS`] alone. `cargo xtask ci` runs the same list locally, in order.

use std::process::{Command, ExitCode};

/// One CI job: what it is called, what it runs, and what the runner has to
/// install before it can.
struct Job {
    /// `cargo xtask <id>`, and the matrix entry's key.
    id: &'static str,
    /// The display name, so a renamed job renames its required status check.
    name: &'static str,
    /// Rustup components, comma-joined for `dtolnay/rust-toolchain`.
    components: &'static str,
    /// The toolchain to install. `msrv` is the only job that is not `stable`;
    /// it reads the workspace's own `rust-version` so the two cannot drift.
    toolchain: &'static str,
    /// Whether the job compiles the workspace. Compiling needs Zig — `fig` and
    /// `twig`, under prov, flower and leaf alike, are Zig-backed and their
    /// `build.rs` runs `zig build` — and is worth caching. Formatting is not.
    builds: bool,
    /// Run in order; the job fails at the first step that does.
    steps: &'static [Step],
}

/// One command in a job.
struct Step {
    program: &'static str,
    args: &'static [&'static str],
    /// Environment set for this command alone.
    env: &'static [(&'static str, &'static str)],
}

/// The workspace's MSRV, read from `Cargo.toml` at compile time so the `msrv`
/// job cannot claim a floor the manifest does not.
const MSRV: &str = env!("CARGO_PKG_RUST_VERSION");

const JOBS: &[Job] = &[
    Job {
        id: "fmt",
        name: "Format",
        components: "rustfmt",
        toolchain: "stable",
        builds: false,
        steps: &[Step {
            program: "cargo",
            args: &["fmt", "--all", "--check"],
            env: &[],
        }],
    },
    Job {
        id: "clippy",
        name: "Clippy",
        components: "clippy",
        toolchain: "stable",
        builds: true,
        steps: &[Step {
            program: "cargo",
            args: &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            env: &[],
        }],
    },
    Job {
        id: "test",
        name: "Tests",
        components: "",
        toolchain: "stable",
        builds: true,
        steps: &[Step {
            program: "cargo",
            args: &["test", "--workspace"],
            env: &[],
        }],
    },
    Job {
        id: "docs",
        name: "Docs",
        components: "",
        toolchain: "stable",
        builds: true,
        // A broken intra-doc link is a broken promise on docs.rs, so it fails
        // here rather than being discovered after a publish.
        steps: &[Step {
            program: "cargo",
            args: &["doc", "--workspace", "--no-deps"],
            env: &[("RUSTDOCFLAGS", "-D warnings")],
        }],
    },
    Job {
        id: "package-isolation",
        name: "Package isolation",
        components: "",
        toolchain: "stable",
        builds: true,
        // `provui-core` is the crate that goes to crates.io, and it goes alone:
        // a consumer gets none of the features `provui-tui` turns on beside it
        // in this workspace. Checking it by itself is what proves it builds the
        // way a consumer sees it.
        steps: &[Step {
            program: "cargo",
            args: &["check", "-p", "provui-core", "--all-targets"],
            env: &[],
        }],
    },
    Job {
        id: "msrv",
        name: "MSRV",
        components: "",
        toolchain: MSRV,
        builds: true,
        // The published crate alone, because the floor is a promise to its
        // consumers and `provui-tui` does not keep it: the terminal raster path
        // under `leaf-ratatui` wants a newer compiler than the core does, and
        // the tui is `publish = false`, so nobody is promised anything about it.
        steps: &[Step {
            program: "cargo",
            args: &["check", "-p", "provui-core", "--all-targets"],
            env: &[],
        }],
    },
];

fn main() -> ExitCode {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        // The matrix the workflow fans out over, as one line of JSON.
        "ci-matrix" => {
            let entries: Vec<String> = JOBS
                .iter()
                .map(|j| {
                    format!(
                        r#"{{"id":"{}","name":"{}","components":"{}","toolchain":"{}","builds":{}}}"#,
                        j.id, j.name, j.components, j.toolchain, j.builds
                    )
                })
                .collect();
            println!("[{}]", entries.join(","));
            ExitCode::SUCCESS
        }
        // Everything CI runs, in order, on this machine.
        "ci" => {
            for job in JOBS {
                // The MSRV job needs a toolchain a contributor may not have,
                // and installing one behind their back would be rude.
                if job.id == "msrv" && !have_toolchain(MSRV) {
                    eprintln!("── {} — skipped (no {MSRV} toolchain installed)", job.name);
                    continue;
                }
                eprintln!("── {}", job.name);
                if !run(job) {
                    return ExitCode::FAILURE;
                }
            }
            eprintln!("── all green");
            ExitCode::SUCCESS
        }
        id => match JOBS.iter().find(|j| j.id == id) {
            Some(job) if run(job) => ExitCode::SUCCESS,
            Some(_) => ExitCode::FAILURE,
            None => {
                eprintln!("usage: cargo xtask <ci|ci-matrix|{}>", ids().join("|"));
                ExitCode::FAILURE
            }
        },
    }
}

fn ids() -> Vec<&'static str> {
    JOBS.iter().map(|j| j.id).collect()
}

fn have_toolchain(name: &str) -> bool {
    Command::new("rustup")
        .args(["run", name, "cargo", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run a job's steps in order, stopping at the first failure.
fn run(job: &Job) -> bool {
    for Step { program, args, env } in job.steps {
        let mut cmd = Command::new(program);
        // Locally, `msrv` has to be asked for by name; on a runner the
        // toolchain is already the only one installed and `+1.88` would be
        // wrong, so this is keyed on the toolchain actually being present.
        if job.toolchain != "stable" && have_toolchain(job.toolchain) {
            cmd.arg(format!("+{}", job.toolchain));
        }
        cmd.args(*args);
        for (k, v) in *env {
            cmd.env(k, v);
        }
        match cmd.status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("{} failed: {status}", job.name);
                return false;
            }
            Err(err) => {
                eprintln!("{} could not start `{program}`: {err}", job.name);
                return false;
            }
        }
    }
    true
}
