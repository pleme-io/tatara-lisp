//! tatara-script — the scripting binary.
//!
//! Usage:
//!   tatara-script path/to/script.tlisp [arg ...]
//!
//! Reads the .tlisp file, expands macros via tatara-lisp, evaluates each
//! form against a `ScriptCtx` host with the full stdlib (http, json, yaml,
//! sops, file I/O, env, sha256, string ops) registered.
//!
//! Command-line args after the script path land in `ScriptCtx::argv`;
//! scripts access them via `(argv)` / `(argv-get 0)`.
//!
//! Exit code 0 on clean success, 1 on parse / eval / FFI error, 2 on
//! usage error (missing script path etc).

use std::process::ExitCode;

use tatara_lisp::read_spanned;
use tatara_lisp_eval::Interpreter;
use tatara_lisp_script::{install_stdlib, ScriptCtx};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(script_path) = args.first() else {
        eprintln!("usage: tatara-script <script.tlisp> [arg ...]");
        return ExitCode::from(2);
    };

    let src = match std::fs::read_to_string(script_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tatara-script: cannot read {script_path}: {e}");
            return ExitCode::from(2);
        }
    };

    let forms = match read_spanned(&src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tatara-script: parse error in {script_path}: {e:?}");
            return ExitCode::from(1);
        }
    };

    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    install_stdlib(&mut interp);

    let mut ctx = ScriptCtx::with_argv(args.iter().skip(1).cloned());

    match interp.eval_program(&forms, &mut ctx) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tatara-script: eval error: {e:?}");
            ExitCode::from(1)
        }
    }
}
