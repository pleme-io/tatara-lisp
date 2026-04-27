//! Kubernetes YAML backend — the simplest renderer in the
//! `Backend` family.
//!
//! For each resource in the env, produce one YAML manifest in
//! the upstream CR shape. The target reader is `kubectl apply`
//! (or FluxCD's Kustomize, since the manifests tree is
//! Kustomize-friendly by construction — one manifest per file,
//! deterministic file naming).
//!
//! Per-domain knowledge is encoded as small functions: each
//! `render_<kind>` takes the typed JSON value produced by the
//! domain's `TataraDomain::compile_from_args` and emits a YAML
//! document with the right `apiVersion` / `kind` / `metadata` /
//! `spec` shape.
//!
//! Unhandled domains (`defbpf-*`, future-domain CRDs we haven't
//! taught the backend) return `RenderError::Unsupported` so the
//! caller can fail-soft (skip them, route to a different
//! backend) or fail-hard.

use crate::backend::{Backend, Manifest, RenderError};
use serde_json::{json, Value};
use std::fmt::Write;
use tatara_env::compile::{Env, Resource};

/// Configuration for the renderer. Defaults match the typical
/// FluxCD / Kustomize layout most pleme-io clusters use today.
#[derive(Debug, Clone)]
pub struct KubernetesYaml {
    /// Default namespace for resources whose typed value
    /// doesn't override. The vast majority of resources won't
    /// override; centralizing here keeps env files terse.
    pub namespace: String,
    /// Labels added to every resource's metadata. Lifted from
    /// `env.spec.labels` plus any caller-supplied additions.
    pub extra_labels: Vec<(String, String)>,
}

impl Default for KubernetesYaml {
    fn default() -> Self {
        Self {
            namespace: "default".into(),
            extra_labels: Vec::new(),
        }
    }
}

impl Backend for KubernetesYaml {
    fn render(&self, env: &Env) -> Result<Vec<Manifest>, RenderError> {
        let mut out = Vec::new();
        for r in &env.resources {
            // Per-keyword overrides for kinds that need special
            // shaping (BPF resources don't map to a single CR;
            // they emit ConfigMaps + the substrate-built object
            // is loaded by a sibling DaemonSet).
            let m = match r.keyword.as_str() {
                "defbpf-program" | "defbpf-map" | "defbpf-policy" => {
                    self.render_bpf_configmap(env, r)?
                }
                _ => {
                    // Generic path — every domain that registers a
                    // `RenderableDomain` impl gets this for free.
                    // Adding a new CRD to the catalog now produces
                    // working YAML the moment the generated crate's
                    // `register()` is called.
                    if let Some(meta) = tatara_lisp::domain::lookup_render(&r.keyword) {
                        self.render_via_registry(env, r, &meta)?
                    } else {
                        return Err(RenderError::Unsupported(r.keyword.clone()));
                    }
                }
            };
            out.push(m);
        }
        Ok(out)
    }
}

impl KubernetesYaml {
    /// Compose the `metadata` block every K8s manifest needs.
    /// Pulls labels from the env spec + the renderer's own
    /// `extra_labels`. Keeps a `pleme.io/` prefix on env-derived
    /// labels so they don't collide with user-set conventions.
    fn metadata(&self, env: &Env, name: &str) -> Value {
        let mut labels = serde_json::Map::new();
        labels.insert("pleme.io/env".into(), json!(env.spec.name));
        for (k, v) in &env.spec.labels {
            labels.insert(format!("pleme.io/{k}"), json!(v));
        }
        for (k, v) in &self.extra_labels {
            labels.insert(k.clone(), json!(v));
        }
        json!({
            "name": name,
            "namespace": self.namespace,
            "labels": labels,
        })
    }

    /// Generic registry-driven render path. Works for any domain
    /// that registered itself via
    /// `tatara_lisp::domain::register_render::<T>()`. The caller
    /// has already looked up the metadata; we just compose the
    /// envelope.
    ///
    /// This is the **compounding seam**: every new CRD-shaped
    /// domain crate auto-renders the moment its `register()` is
    /// called. No edits to this file. No special-cased match arms.
    fn render_via_registry(
        &self,
        env: &Env,
        r: &Resource,
        meta: &tatara_lisp::RenderHandler,
    ) -> Result<Manifest, RenderError> {
        // Pick the resource's name. Order:
        //   1. the registered NAME_FIELD on the typed value
        //   2. fallback to `name` if NAME_FIELD missing
        //   3. fallback to env-derived `<env-name>-<kind>`
        let name = string_field(&r.value, meta.name_field)
            .or_else(|| string_field(&r.value, "name"))
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!("{}-{}", env.spec.name, meta.kind.to_lowercase())
            });
        let manifest = json!({
            "apiVersion": meta.api_version,
            "kind": meta.kind,
            "metadata": self.metadata(env, &name),
            "spec": &r.value,
        });
        // Filesystem layout: one directory per kind, lower-case.
        // Stable + grep-friendly + Kustomize-friendly.
        let dir = meta.kind.to_lowercase();
        let path = format!("{dir}/{name}.yaml");
        Ok(Manifest {
            kind: "yaml".into(),
            path,
            content: yaml_string(&manifest)?,
        })
    }

    fn render_bpf_configmap(&self, env: &Env, r: &Resource) -> Result<Manifest, RenderError> {
        let name = string_field(&r.value, "name").unwrap_or("bpf");
        let cm_name = format!("bpf-{}-{}", r.keyword.trim_start_matches("def"), name);
        // The ConfigMap holds the typed BPF spec as JSON. A
        // sibling DaemonSet (rendered by a different backend or
        // hand-authored once) reads it + loads the matching
        // .bpf.o object built by `substrate/lib/build/tatara/ebpf.nix`.
        let payload = serde_json::to_string_pretty(&r.value)
            .map_err(|e| RenderError::Yaml(format!("bpf json: {e}")))?;
        let manifest = json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": self.metadata(env, &cm_name),
            "data": {
                "spec.json": payload,
            },
        });
        Ok(Manifest {
            kind: "yaml".into(),
            path: format!("bpf/{cm_name}.yaml"),
            content: yaml_string(&manifest)?,
        })
    }
}

fn string_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.as_object()?.get(key)?.as_str()
}

/// Convert a `serde_json::Value` to a YAML string. Uses
/// `serde_yaml_ng` so the output is deterministic + reads cleanly
/// in `kubectl apply`.
fn yaml_string(v: &Value) -> Result<String, RenderError> {
    let yaml = serde_yaml_ng::to_string(v).map_err(|e| RenderError::Yaml(e.to_string()))?;
    // Add a leading `---\n` so multiple manifests can be
    // concatenated into one stream cleanly.
    let mut out = String::new();
    let _ = writeln!(out, "---");
    out.push_str(&yaml);
    Ok(out)
}
