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
            let m = match r.keyword.as_str() {
                "defgateway" => self.render_gateway(env, r)?,
                "defciliumnetworkpolicy" => self.render_cilium_netpol(env, r)?,
                "defpodmonitor" => self.render_podmonitor(env, r)?,
                // BPF resources don't map directly to a single CR —
                // they're emitted as ConfigMaps holding the spec
                // metadata; the actual loader is a separate
                // DaemonSet/Job outside this backend's scope.
                "defbpf-program" | "defbpf-map" | "defbpf-policy" => {
                    self.render_bpf_configmap(env, r)?
                }
                other => {
                    return Err(RenderError::Unsupported(other.to_string()));
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

    fn render_gateway(&self, env: &Env, r: &Resource) -> Result<Manifest, RenderError> {
        let value = &r.value;
        // The Gateway's identifier comes from `gateway_class_name`
        // (it's how the upstream CRD names them). We don't use it
        // here beyond validating presence — the env-derived name
        // overrides it for K8s metadata.name. Refusing to render
        // when it's missing keeps the output strictly conformant
        // with the CRD's required-fields contract.
        let _class_name = string_field(value, "gateway_class_name").ok_or_else(|| {
            RenderError::Resource {
                keyword: r.keyword.clone(),
                name: "<unnamed>".into(),
                message: "missing gateway_class_name".into(),
            }
        })?;
        // Gateway names are typically derived from the env. The
        // upstream Gateway CRD spec is very large; we round-trip
        // the typed JSON value as the `spec` payload and trust
        // the field names match (the forge-generated structs use
        // the same names as the upstream schema).
        let manifest = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "metadata": self.metadata(env, &format!("{}-gateway", env.spec.name)),
            "spec": value,
        });
        Ok(Manifest {
            kind: "yaml".into(),
            path: format!("gateways/{}-gateway.yaml", env.spec.name),
            content: yaml_string(&manifest)?,
        })
    }

    fn render_cilium_netpol(&self, env: &Env, r: &Resource) -> Result<Manifest, RenderError> {
        let name = string_field(&r.value, "name").unwrap_or("policy");
        let manifest = json!({
            "apiVersion": "cilium.io/v2",
            "kind": "CiliumNetworkPolicy",
            "metadata": self.metadata(env, name),
            "spec": &r.value,
        });
        Ok(Manifest {
            kind: "yaml".into(),
            path: format!("network-policies/{name}.yaml"),
            content: yaml_string(&manifest)?,
        })
    }

    fn render_podmonitor(&self, env: &Env, r: &Resource) -> Result<Manifest, RenderError> {
        let name = string_field(&r.value, "name").unwrap_or("podmonitor");
        let manifest = json!({
            "apiVersion": "monitoring.coreos.com/v1",
            "kind": "PodMonitor",
            "metadata": self.metadata(env, name),
            "spec": &r.value,
        });
        Ok(Manifest {
            kind: "yaml".into(),
            path: format!("monitoring/{name}.yaml"),
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
