//! The tatara-script stdlib. Each module registers a family of FFI
//! primitives on `Interpreter<ScriptCtx>`; `install_stdlib` is the
//! single entry point — call after `Interpreter::new()`.

use tatara_lisp_eval::Interpreter;

use crate::script_ctx::ScriptCtx;

// Core scripting families
pub mod cli;
pub mod crypto_extra;
pub mod encoding;
pub mod env;
pub mod fs;
pub mod hash;
pub mod http;
pub mod io;
pub mod json;
pub mod list_ext;
pub mod log;
pub mod os;
pub mod process;
pub mod regex;
pub mod sops;
pub mod string;
pub mod string_ext;
pub mod time;
pub mod toml;
pub mod yaml;

/// Install every stdlib family. Order doesn't matter — each family owns
/// its own namespace.
pub fn install_stdlib(interp: &mut Interpreter<ScriptCtx>) {
    cli::install(interp);
    crypto_extra::install(interp);
    encoding::install(interp);
    env::install(interp);
    fs::install(interp);
    hash::install(interp);
    http::install(interp);
    io::install(interp);
    json::install(interp);
    list_ext::install(interp);
    log::install(interp);
    os::install(interp);
    process::install(interp);
    regex::install(interp);
    sops::install(interp);
    string::install(interp);
    string_ext::install(interp);
    time::install(interp);
    toml::install(interp);
    yaml::install(interp);
}
