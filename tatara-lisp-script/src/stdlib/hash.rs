//! Hash functions.
//!
//!   (sha256 STR)   → hex digest string
//!   (slugify NAME TYPE) → slug matching Pangea::Architectures::
//!                         CloudflareDnsRecords.derive_slug. Useful when a
//!                         script is emitting tofu import commands whose
//!                         resource addresses come from the Ruby
//!                         architecture's naming convention.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tatara_lisp_eval::{Arity, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "sha256",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "sha256", sp)?;
            let digest = Sha256::digest(s.as_bytes());
            Ok(Value::Str(Arc::from(hex::encode(digest))))
        },
    );

    interp.register_fn(
        "slugify",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let name = str_arg(&args[0], "slugify", sp)?;
            let kind = str_arg(&args[1], "slugify", sp)?;
            Ok(Value::Str(Arc::from(derive_slug(&name, &kind))))
        },
    );
}

/// Mirror of Pangea::Architectures::CloudflareDnsRecords.derive_slug so
/// tlisp scripts emit the same Terraform resource addresses as the Ruby
/// architecture. Any drift here is a bug — keep this and the Ruby method
/// in lockstep.
pub fn derive_slug(name: &str, kind: &str) -> String {
    let normalized = if name == "@" || name.is_empty() {
        "root".to_string()
    } else {
        let mut s = name.replace('.', "_");
        // "*_foo" → "wildcard_foo"
        if let Some(stripped) = s.strip_prefix("*_") {
            s = format!("wildcard_{stripped}");
        } else if let Some(stripped) = s.strip_prefix("*") {
            s = format!("wildcard{stripped}");
        }
        // Collapse runs of underscores so "resend__domainkey" → "resend_domainkey".
        while s.contains("__") {
            s = s.replace("__", "_");
        }
        s
    };
    format!("{}_{}", normalized, kind.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apex_normalizes_to_root() {
        assert_eq!(derive_slug("@", "CNAME"), "root_cname");
    }

    #[test]
    fn dots_become_underscores() {
        assert_eq!(derive_slug("api.staging", "CNAME"), "api_staging_cname");
    }

    #[test]
    fn wildcard_expands() {
        assert_eq!(derive_slug("*.staging", "CNAME"), "wildcard_staging_cname");
    }

    #[test]
    fn underscore_prefixed_dkim_preserved() {
        assert_eq!(
            derive_slug("resend._domainkey", "TXT"),
            "resend_domainkey_txt"
        );
    }

    #[test]
    fn plain_name_lowercases_type() {
        assert_eq!(derive_slug("www", "CNAME"), "www_cname");
        assert_eq!(derive_slug("send", "MX"), "send_mx");
    }
}
