//! `tatara-domain-forge` CLI — single-shot generator.
//!
//! ```bash
//! tatara-domain-forge \
//!   --input ./gateway-api.yaml \
//!   --name tatara-gateway-api \
//!   --output ./tatara-gateway-api
//! ```
//!
//! Reads a K8s CRD YAML bundle, lowers it into the IR, emits a
//! complete tatara-domain Rust crate. Re-running the command
//! overwrites the output unconditionally — the generated tree is
//! "boilerplate" (per repo-forge taxonomy), not "authored".

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "tatara-domain-forge")]
#[command(about = "Generate a tatara-lisp domain crate from typed inputs")]
struct Args {
    /// Input file. Currently a K8s CRD YAML (single or multi-doc).
    #[arg(long)]
    input: PathBuf,

    /// Crate name, by convention `tatara-{thing}`.
    #[arg(long)]
    name: String,

    /// Output directory (will be created if missing).
    #[arg(long)]
    output: PathBuf,

    /// Override author for `[package].authors` in the emitted manifest.
    #[arg(long, default_value = "Pleme.io <engineering@pleme.io>")]
    author: String,

    /// Crate version for the emitted manifest.
    #[arg(long, default_value = "0.1.0")]
    version: String,

    /// Skip writing — print to stdout instead. Useful for diff-ing.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let domain = match tatara_domain_forge::from_crd_yaml(&args.input, &args.name) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tatara-domain-forge: parse failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let opts = tatara_domain_forge::EmitOptions {
        author: args.author,
        version: args.version,
        ..Default::default()
    };
    let cargo = tatara_domain_forge::emit_cargo_toml(&domain, &opts);
    let lib = tatara_domain_forge::emit_lib_rs(&domain);
    let readme = tatara_domain_forge::emit_readme(&domain);
    if args.dry_run {
        println!("───── Cargo.toml ─────");
        println!("{cargo}");
        println!("───── src/lib.rs ─────");
        println!("{lib}");
        println!("───── README.md ─────");
        println!("{readme}");
        return std::process::ExitCode::SUCCESS;
    }
    let src_dir = args.output.join("src");
    if let Err(e) = std::fs::create_dir_all(&src_dir) {
        eprintln!("tatara-domain-forge: mkdir {}: {e}", src_dir.display());
        return std::process::ExitCode::FAILURE;
    }
    let writes = [
        (args.output.join("Cargo.toml"), cargo),
        (src_dir.join("lib.rs"), lib),
        (args.output.join("README.md"), readme),
    ];
    for (path, content) in writes {
        if let Err(e) = std::fs::write(&path, content) {
            eprintln!("tatara-domain-forge: write {}: {e}", path.display());
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote {}", path.display());
    }
    println!(
        "✓ generated {} from {} resource(s)",
        args.name,
        domain.resources.len()
    );
    std::process::ExitCode::SUCCESS
}
