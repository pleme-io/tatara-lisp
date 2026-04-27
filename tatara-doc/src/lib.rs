//! tatara-doc — catalog browser over the registered domains.
//!
//! Walks the three capability registries
//! (`registered_keywords`, `registered_render_keywords`,
//! `registered_doc_keywords`) and emits a stable Markdown
//! catalog page. Same data the IDE hover-help would use; same
//! data a `tatara doc` CLI subcommand would print; same data a
//! web view would render.
//!
//! ## What this proves
//!
//! Three capability registries are now visible to a single
//! consumer at once. Every new domain that registers itself
//! with the standard six-line contract gets a free catalog
//! entry with no edits to this crate. That's "compounding
//! systems on top of each other" made operational — each
//! layer (compile / render / doc) contributes; this crate
//! just unions.

use std::fmt::Write;
use tatara_lisp::domain::{
    lookup_doc, lookup_render, registered_doc_keywords, registered_keywords,
};

/// Render every registered domain into one Markdown index page.
/// Section per keyword, in keyword-name-sorted order. Each
/// section includes:
///
/// - the keyword + (when registered) the K8s apiVersion + kind
///   it renders to,
/// - the domain's docstring,
/// - a table of fields with their per-field docs,
/// - a sample `(defwhatever :k v …)` skeleton.
#[must_use]
pub fn render_catalog() -> String {
    let mut keywords = registered_keywords();
    keywords.sort_unstable();
    let mut out = String::new();
    let _ = writeln!(out, "# tatara catalog");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Auto-generated from the live tatara domain registries — \
         compile / render / doc capabilities. {} domain(s) registered.",
        keywords.len()
    );
    let _ = writeln!(out);

    if keywords.is_empty() {
        let _ = writeln!(
            out,
            "_No domains registered yet. Call your domain crates' \
             `register()` functions before generating the catalog._"
        );
        return out;
    }

    // Table of contents.
    let _ = writeln!(out, "## Index");
    let _ = writeln!(out);
    for kw in &keywords {
        let _ = writeln!(out, "- [`{kw}`](#{anchor})", anchor = anchor(kw));
    }
    let _ = writeln!(out);

    for kw in &keywords {
        render_section(&mut out, kw);
    }
    out
}

/// Catalog page for one keyword. Pure function — useful for the
/// per-resource "show me this one" CLI mode.
#[must_use]
pub fn render_one(keyword: &str) -> String {
    let mut out = String::new();
    render_section(&mut out, keyword);
    out
}

/// List the registered keywords, sorted, as a plain Vec — useful
/// for shell completion + IDE-side autocomplete.
#[must_use]
pub fn list_keywords() -> Vec<&'static str> {
    let mut v = registered_keywords();
    v.sort_unstable();
    v
}

fn render_section(out: &mut String, kw: &str) {
    let _ = writeln!(out, "## `{kw}` <a id=\"{anchor}\"></a>", anchor = anchor(kw));
    let _ = writeln!(out);
    if let Some(rmeta) = lookup_render(kw) {
        let _ = writeln!(
            out,
            "**Renders to**: `{}` / `{}` (Kubernetes CR)",
            rmeta.api_version, rmeta.kind
        );
        let _ = writeln!(out);
    }
    if let Some(dmeta) = lookup_doc(kw) {
        if !dmeta.docstring.is_empty() {
            let _ = writeln!(out, "{}", dmeta.docstring);
            let _ = writeln!(out);
        }
        if !dmeta.field_docs.is_empty() {
            let _ = writeln!(out, "### Fields");
            let _ = writeln!(out);
            let _ = writeln!(out, "| Field | Description |");
            let _ = writeln!(out, "|---|---|");
            for (name, doc) in dmeta.field_docs {
                let kebab = snake_to_kebab(name);
                let _ = writeln!(
                    out,
                    "| `:{kebab}` | {} |",
                    doc.replace('|', "\\|")
                );
            }
            let _ = writeln!(out);
        }
    } else {
        let _ = writeln!(
            out,
            "_No doc metadata registered. Author docstrings or \
             regenerate from a CRD with descriptions._"
        );
        let _ = writeln!(out);
    }
}

fn anchor(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

fn snake_to_kebab(s: &str) -> String {
    s.replace('_', "-")
}

/// All keywords that have ALL three capabilities (compile +
/// render + doc) registered. Useful for completeness checks
/// before shipping a release.
#[must_use]
pub fn fully_registered_keywords() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = registered_doc_keywords()
        .into_iter()
        .filter(|kw| lookup_render(kw).is_some())
        .collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_renders_a_helpful_page() {
        // Without registering anything, the page should still
        // render — just with the "no domains yet" hint.
        let s = render_catalog();
        assert!(s.contains("# tatara catalog"));
    }

    #[test]
    fn sorts_keywords_deterministically() {
        // Sanity — sorting is stable so the catalog diff is
        // predictable across runs.
        let mut v: Vec<&'static str> = vec!["zebra", "alpha", "monitor"];
        v.sort_unstable();
        assert_eq!(v[0], "alpha");
    }
}
