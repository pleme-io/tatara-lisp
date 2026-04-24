//! The tatara-script stdlib. Every module in this file registers a
//! family of FFI primitives on the `Interpreter<ScriptCtx>` passed in.
//!
//! Top-level `install_stdlib` is the single entry point — call it after
//! creating the interpreter, before `eval_program`.

use tatara_lisp_eval::Interpreter;

use crate::script_ctx::ScriptCtx;

pub mod env;
pub mod hash;
pub mod http;
pub mod io;
pub mod json;
pub mod sops;
pub mod string;
pub mod yaml;

/// Install every stdlib family on the given interpreter. Order does not
/// matter — each family owns its own namespace, no redefines.
pub fn install_stdlib(interp: &mut Interpreter<ScriptCtx>) {
    env::install(interp);
    hash::install(interp);
    http::install(interp);
    io::install(interp);
    json::install(interp);
    sops::install(interp);
    string::install(interp);
    yaml::install(interp);
}
