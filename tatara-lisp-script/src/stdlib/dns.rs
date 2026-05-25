//! DNS provider domain — the `pleme/dns` surface.
//!
//! Provides the slash-named functions every `lareira-dns-reconciler`
//! computeunit calls:
//!
//!   (dns/upsert :provider :route53 :zone-id Z :credentials C
//!               :record-type :CNAME :name "x.quero.lol" :value "lb…"
//!               :ttl 60 :proxied false)        → {:status "ok" …}
//!   (dns/delete :provider … :zone-id … :credentials … :record-type …
//!               :name … :value … :ttl …)        → {:status "ok" …}
//!   (dns/list   :provider … :zone-id … :credentials …)
//!                                                → ((…) (…) …)
//!
//! Two providers are wired end to end:
//!
//!   * `:cloudflare` — REST over `api.cloudflare.com/client/v4`, Bearer
//!     token auth (creds key `api-token` / `token`). JSON wire format.
//!   * `:route53` — `ChangeResourceRecordSets` / `ListResourceRecordSets`
//!     over `route53.amazonaws.com`, **AWS SigV4** request signing (creds
//!     keys `access-key-id` + `secret-access-key`, optional
//!     `session-token` + `region`). XML wire format, built through a
//!     typed [`ChangeBatch`] `Display` impl — never `format!()`-of-XML.
//!
//! `:hetzner` / `:gcp` are declared in the reconciler's provider enum but
//! return a typed `unimplemented provider` error here — never a silent
//! wrong answer (org CLAUDE.md TYPED-SPEC rule).
//!
//! ## Testability
//!
//! All network egress goes through the [`DnsTransport`] trait — the
//! Environment seam from the org CLAUDE.md typed-spec-triplet rule. The
//! production impl ([`UreqTransport`]) wraps the shared `ureq::Agent`;
//! tests drive a [`MockTransport`] that records the request and returns a
//! canned response, so every provider path + the SigV4 signer is unit
//! tested with zero network. The signer is validated against AWS's
//! published GET-vanilla example vector.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tatara_lisp_eval::value::MapKey;
use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;

type HmacSha256 = Hmac<Sha256>;

const FN_UPSERT: &str = "dns/upsert";
const FN_DELETE: &str = "dns/delete";
const FN_LIST: &str = "dns/list";

// ─────────────────────────────────────────────────────────────────────
// Registration
// ─────────────────────────────────────────────────────────────────────

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(FN_UPSERT, Arity::AtLeast(2), |args: &[Value], ctx: &mut ScriptCtx, sp| {
        run(Action::Upsert, args, ctx, sp)
    });
    interp.register_fn(FN_DELETE, Arity::AtLeast(2), |args: &[Value], ctx: &mut ScriptCtx, sp| {
        run(Action::Delete, args, ctx, sp)
    });
    interp.register_fn(FN_LIST, Arity::AtLeast(2), |args: &[Value], ctx: &mut ScriptCtx, sp| {
        let kw = Kwargs::parse(args, FN_LIST, sp)?;
        let provider = kw.provider(FN_LIST, sp)?;
        let zone_id = kw.want_str("zone-id", FN_LIST, sp)?;
        let creds = kw.credentials(FN_LIST, sp)?;
        let transport = UreqTransport::new(ctx);
        let records = dns_list(&transport, provider, &zone_id, &creds)
            .map_err(|e| EvalError::native_fn(FN_LIST, e, sp))?;
        Ok(Value::List(Arc::new(records.into_iter().map(Record::into_value).collect())))
    });
}

/// Shared body for `dns/upsert` + `dns/delete` — they differ only in the
/// Route53 `Action` / Cloudflare verb.
fn run(action: Action, args: &[Value], ctx: &mut ScriptCtx, sp: tatara_lisp::Span) -> Result<Value, EvalError> {
    let fn_name = action.fn_name();
    let kw = Kwargs::parse(args, fn_name, sp)?;
    let req = ChangeRequest {
        provider: kw.provider(fn_name, sp)?,
        zone_id: kw.want_str("zone-id", fn_name, sp)?,
        credentials: kw.credentials(fn_name, sp)?,
        record_type: kw.record_type(fn_name, sp)?,
        name: kw.want_str("name", fn_name, sp)?,
        value: kw.want_str("value", fn_name, sp)?,
        ttl: kw.opt_int("ttl").unwrap_or(300),
        proxied: kw.opt_bool("proxied").unwrap_or(false),
        action,
    };
    let transport = UreqTransport::new(ctx);
    let outcome = dns_change(&transport, &req).map_err(|e| EvalError::native_fn(fn_name, e, sp))?;
    Ok(outcome.into_value())
}

// ─────────────────────────────────────────────────────────────────────
// Typed request / response model
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Cloudflare,
    Route53,
    Hetzner,
    Gcp,
}

impl Provider {
    fn from_keyword(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cloudflare" => Some(Self::Cloudflare),
            "route53" => Some(Self::Route53),
            "hetzner" => Some(Self::Hetzner),
            "gcp" => Some(Self::Gcp),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Cloudflare => "cloudflare",
            Self::Route53 => "route53",
            Self::Hetzner => "hetzner",
            Self::Gcp => "gcp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordType {
    A,
    Aaaa,
    Cname,
    Txt,
}

impl RecordType {
    /// Liberal parse — the two DnsRule schemas in the fleet spell the
    /// keyword differently (`:A`/`:a`, `:AAAA`/`:a-a-a-a`,
    /// `:CNAME`/`:c-n-a-m-e`, `:TXT`/`:t-x-t`). Accept all of them.
    fn from_keyword(s: &str) -> Option<Self> {
        let norm: String = s.chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase();
        match norm.as_str() {
            "a" => Some(Self::A),
            "aaaa" => Some(Self::Aaaa),
            "cname" => Some(Self::Cname),
            "txt" => Some(Self::Txt),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Cname => "CNAME",
            Self::Txt => "TXT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Upsert,
    Delete,
}

impl Action {
    fn fn_name(self) -> &'static str {
        match self {
            Self::Upsert => FN_UPSERT,
            Self::Delete => FN_DELETE,
        }
    }
    /// Route53 ChangeBatch action verb.
    fn route53_verb(self) -> &'static str {
        match self {
            Self::Upsert => "UPSERT",
            Self::Delete => "DELETE",
        }
    }
}

/// Provider-agnostic credentials, parsed from the `:credentials` map a
/// computeunit gets from `(k8s/read-secret …)`.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub map: HashMap<String, String>,
}

impl Credentials {
    fn get(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|k| self.map.get(*k).map(String::as_str))
    }
    fn cloudflare_token(&self) -> Option<&str> {
        self.get(&["api-token", "api_token", "token", "CLOUDFLARE_API_TOKEN"])
    }
    fn aws_access_key(&self) -> Option<&str> {
        self.get(&["access-key-id", "access_key_id", "aws-access-key-id", "AWS_ACCESS_KEY_ID"])
    }
    fn aws_secret_key(&self) -> Option<&str> {
        self.get(&["secret-access-key", "secret_access_key", "aws-secret-access-key", "AWS_SECRET_ACCESS_KEY"])
    }
    fn aws_session_token(&self) -> Option<&str> {
        self.get(&["session-token", "session_token", "aws-session-token", "AWS_SESSION_TOKEN"])
    }
    fn aws_region(&self) -> &str {
        // Route53 is a global service; SigV4 still requires a region and
        // AWS canonicalizes Route53 to us-east-1.
        self.get(&["region", "aws-region", "AWS_REGION"]).unwrap_or("us-east-1")
    }
}

pub struct ChangeRequest {
    pub provider: Provider,
    pub zone_id: String,
    pub credentials: Credentials,
    pub record_type: RecordType,
    pub name: String,
    pub value: String,
    pub ttl: i64,
    pub proxied: bool,
    pub action: Action,
}

/// Result returned to Lisp as a map.
#[derive(Debug)]
pub struct Outcome {
    provider: Provider,
    action: Action,
    name: String,
    record_type: RecordType,
}

impl Outcome {
    fn into_value(self) -> Value {
        map_value(&[
            ("status", Value::Str(Arc::from("ok"))),
            ("provider", Value::Str(Arc::from(self.provider.as_str()))),
            ("action", Value::Str(Arc::from(self.action.route53_verb().to_ascii_lowercase()))),
            ("name", Value::Str(Arc::from(self.name))),
            ("type", Value::Str(Arc::from(self.record_type.as_str()))),
        ])
    }
}

/// A single record returned by `dns/list`.
#[derive(Debug)]
pub struct Record {
    name: String,
    record_type: String,
    value: String,
    ttl: Option<i64>,
}

impl Record {
    fn into_value(self) -> Value {
        let mut fields = vec![
            ("name", Value::Str(Arc::from(self.name))),
            ("type", Value::Str(Arc::from(self.record_type))),
            ("value", Value::Str(Arc::from(self.value))),
        ];
        if let Some(ttl) = self.ttl {
            fields.push(("ttl", Value::Int(ttl)));
        }
        map_value(&fields)
    }
}

// ─────────────────────────────────────────────────────────────────────
// Transport seam (the Environment trait)
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The single egress point every provider funnels through. Mockable in
/// tests; the production impl is [`UreqTransport`].
pub trait DnsTransport {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, String>;
}

/// Production transport — borrows a clone of the script's shared agent.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    fn new(ctx: &mut ScriptCtx) -> Self {
        Self { agent: ctx.http().clone() }
    }
}

impl DnsTransport for UreqTransport {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, String> {
        let ctx = || format!("{} {}", method_str(req.method), req.url);
        match req.method {
            Method::Get | Method::Delete => {
                let mut b = if req.method == Method::Get {
                    self.agent.get(&req.url)
                } else {
                    self.agent.delete(&req.url)
                };
                for (k, v) in &req.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                let resp = b.call().map_err(|e| format!("{}: {e}", ctx()))?;
                let status = resp.status().as_u16();
                let body = resp.into_body().read_to_string().map_err(|e| format!("{}: {e}", ctx()))?;
                Ok(HttpResponse { status, body })
            }
            Method::Post | Method::Put => {
                let mut b = if req.method == Method::Post {
                    self.agent.post(&req.url)
                } else {
                    self.agent.put(&req.url)
                };
                for (k, v) in &req.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                let body = req.body.clone().unwrap_or_default();
                let resp = b.send(body).map_err(|e| format!("{}: {e}", ctx()))?;
                let status = resp.status().as_u16();
                let body = resp.into_body().read_to_string().map_err(|e| format!("{}: {e}", ctx()))?;
                Ok(HttpResponse { status, body })
            }
        }
    }
}

fn method_str(m: Method) -> &'static str {
    match m {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Delete => "DELETE",
    }
}

// ─────────────────────────────────────────────────────────────────────
// Provider dispatch
// ─────────────────────────────────────────────────────────────────────

fn dns_change(t: &dyn DnsTransport, req: &ChangeRequest) -> Result<Outcome, String> {
    match req.provider {
        Provider::Route53 => route53::change(t, req, now_unix()),
        Provider::Cloudflare => cloudflare::change(t, req),
        other => Err(unimplemented(other)),
    }
}

fn dns_list(
    t: &dyn DnsTransport,
    provider: Provider,
    zone_id: &str,
    creds: &Credentials,
) -> Result<Vec<Record>, String> {
    match provider {
        Provider::Route53 => route53::list(t, zone_id, creds, now_unix()),
        Provider::Cloudflare => cloudflare::list(t, zone_id, creds),
        other => Err(unimplemented(other)),
    }
}

fn unimplemented(p: Provider) -> String {
    format!(
        "DNS provider '{}' is declared but not implemented — wire it in tatara-lisp-script/src/stdlib/dns.rs (no silent fallback)",
        p.as_str()
    )
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────
// Route53 provider
// ─────────────────────────────────────────────────────────────────────

mod route53 {
    use super::*;

    const API_VERSION: &str = "2013-04-01";
    const HOST: &str = "route53.amazonaws.com";
    const SERVICE: &str = "route53";

    pub(super) fn change(t: &dyn DnsTransport, req: &ChangeRequest, now: u64) -> Result<Outcome, String> {
        let zone = bare_zone_id(&req.zone_id);
        let body = ChangeBatch {
            action: req.action.route53_verb(),
            name: &req.name,
            rtype: req.record_type.as_str(),
            ttl: req.ttl.max(0),
            value: &formatted_value(req.record_type, &req.value),
        }
        .to_string();

        let path = format!("/{API_VERSION}/hostedzone/{zone}/rrset/");
        let url = format!("https://{HOST}{path}");
        let canonical_uri = uri_encode_path(&path);
        let signer = Sigv4 {
            method: "POST",
            host: HOST,
            canonical_uri: &canonical_uri,
            canonical_query: "",
            payload: body.as_bytes(),
            region: req.credentials.aws_region(),
            service: SERVICE,
            access_key: cred_required(req.credentials.aws_access_key(), "access-key-id")?,
            secret_key: cred_required(req.credentials.aws_secret_key(), "secret-access-key")?,
            session_token: req.credentials.aws_session_token(),
            amz: AmzDate::from_unix(now),
        };
        let mut headers = signer.signed_headers();
        headers.push(("content-type".into(), "application/xml".into()));

        let resp = t.send(&HttpRequest {
            method: Method::Post,
            url,
            headers,
            body: Some(body),
        })?;
        if !resp.is_success() {
            return Err(format!("route53 ChangeResourceRecordSets failed ({}): {}", resp.status, resp.body.trim()));
        }
        Ok(Outcome {
            provider: Provider::Route53,
            action: req.action,
            name: req.name.clone(),
            record_type: req.record_type,
        })
    }

    pub(super) fn list(t: &dyn DnsTransport, zone_id: &str, creds: &Credentials, now: u64) -> Result<Vec<Record>, String> {
        let zone = bare_zone_id(zone_id);
        let path = format!("/{API_VERSION}/hostedzone/{zone}/rrset");
        let url = format!("https://{HOST}{path}");
        let canonical_uri = uri_encode_path(&path);
        let signer = Sigv4 {
            method: "GET",
            host: HOST,
            canonical_uri: &canonical_uri,
            canonical_query: "",
            payload: b"",
            region: creds.aws_region(),
            service: SERVICE,
            access_key: cred_required(creds.aws_access_key(), "access-key-id")?,
            secret_key: cred_required(creds.aws_secret_key(), "secret-access-key")?,
            session_token: creds.aws_session_token(),
            amz: AmzDate::from_unix(now),
        };
        let resp = t.send(&HttpRequest {
            method: Method::Get,
            url,
            headers: signer.signed_headers(),
            body: None,
        })?;
        if !resp.is_success() {
            return Err(format!("route53 ListResourceRecordSets failed ({}): {}", resp.status, resp.body.trim()));
        }
        Ok(parse_rrsets(&resp.body))
    }

    /// Strip a leading `/hostedzone/` prefix operators sometimes paste in.
    fn bare_zone_id(z: &str) -> &str {
        z.trim_start_matches("/hostedzone/").trim_start_matches('/')
    }

    /// Route53 wants TXT values double-quoted on the wire.
    pub(super) fn formatted_value(rtype: RecordType, value: &str) -> String {
        if rtype == RecordType::Txt && !(value.starts_with('"') && value.ends_with('"')) {
            format!("\"{}\"", value.replace('"', "\\\""))
        } else {
            value.to_string()
        }
    }

    /// Minimal, dependency-free scan of the ListResourceRecordSets XML —
    /// enough for drift detection. Pulls Name/Type/TTL + the first Value
    /// from each `<ResourceRecordSet>`.
    fn parse_rrsets(xml: &str) -> Vec<Record> {
        let mut out = Vec::new();
        for block in split_between(xml, "<ResourceRecordSet>", "</ResourceRecordSet>") {
            let name = tag(block, "Name").unwrap_or_default();
            let rtype = tag(block, "Type").unwrap_or_default();
            if name.is_empty() || rtype.is_empty() {
                continue;
            }
            let ttl = tag(block, "TTL").and_then(|t| t.trim().parse::<i64>().ok());
            let value = tag(block, "Value").unwrap_or_default();
            out.push(Record { name: xml_unescape(&name), record_type: rtype, value: xml_unescape(&value), ttl });
        }
        out
    }

    fn split_between<'a>(s: &'a str, open: &str, close: &str) -> Vec<&'a str> {
        let mut blocks = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len()..];
            if let Some(end) = after.find(close) {
                blocks.push(&after[..end]);
                rest = &after[end + close.len()..];
            } else {
                break;
            }
        }
        blocks
    }

    fn tag(block: &str, name: &str) -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = block.find(&open)? + open.len();
        let end = block[start..].find(&close)? + start;
        Some(block[start..end].to_string())
    }
}

/// Typed Route53 `ChangeResourceRecordSets` request body. The `Display`
/// impl is the typed-emission surface — XML is never `format!()`-assembled
/// ad hoc; values are escaped here, in one place.
struct ChangeBatch<'a> {
    action: &'a str,
    name: &'a str,
    rtype: &'a str,
    ttl: i64,
    value: &'a str,
}

impl std::fmt::Display for ChangeBatch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
        write!(f, "<ChangeResourceRecordSetsRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">")?;
        write!(f, "<ChangeBatch><Changes><Change>")?;
        write!(f, "<Action>{}</Action>", self.action)?;
        write!(f, "<ResourceRecordSet>")?;
        write!(f, "<Name>{}</Name>", XmlText(self.name))?;
        write!(f, "<Type>{}</Type>", self.rtype)?;
        write!(f, "<TTL>{}</TTL>", self.ttl)?;
        write!(f, "<ResourceRecords><ResourceRecord><Value>{}</Value></ResourceRecord></ResourceRecords>", XmlText(self.value))?;
        write!(f, "</ResourceRecordSet>")?;
        write!(f, "</Change></Changes></ChangeBatch>")?;
        write!(f, "</ChangeResourceRecordSetsRequest>")
    }
}

/// XML-text-escaping newtype — `Display` emits the escaped form.
struct XmlText<'a>(&'a str);

impl std::fmt::Display for XmlText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for c in self.0.chars() {
            match c {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                '\'' => f.write_str("&apos;")?,
                other => f.write_str(other.encode_utf8(&mut [0; 4]))?,
            }
        }
        Ok(())
    }
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

// ─────────────────────────────────────────────────────────────────────
// Cloudflare provider
// ─────────────────────────────────────────────────────────────────────

mod cloudflare {
    use super::*;

    const API: &str = "https://api.cloudflare.com/client/v4";

    pub(super) fn change(t: &dyn DnsTransport, req: &ChangeRequest) -> Result<Outcome, String> {
        let token = cred_required(req.credentials.cloudflare_token(), "api-token")?;
        let existing = find_record(t, token, &req.zone_id, req.record_type, &req.name)?;
        match req.action {
            Action::Upsert => {
                let payload = serde_json::json!({
                    "type": req.record_type.as_str(),
                    "name": req.name,
                    "content": req.value,
                    "ttl": req.ttl.max(1),
                    "proxied": req.proxied,
                })
                .to_string();
                let (method, url) = match &existing {
                    Some(id) => (Method::Put, format!("{API}/zones/{}/dns_records/{id}", req.zone_id)),
                    None => (Method::Post, format!("{API}/zones/{}/dns_records", req.zone_id)),
                };
                let resp = t.send(&HttpRequest { method, url, headers: auth(token), body: Some(payload) })?;
                check_cf(&resp, "upsert dns_record")?;
            }
            Action::Delete => {
                let Some(id) = existing else {
                    // Already absent — deletes are idempotent.
                    return Ok(outcome(req));
                };
                let resp = t.send(&HttpRequest {
                    method: Method::Delete,
                    url: format!("{API}/zones/{}/dns_records/{id}", req.zone_id),
                    headers: auth(token),
                    body: None,
                })?;
                check_cf(&resp, "delete dns_record")?;
            }
        }
        Ok(outcome(req))
    }

    pub(super) fn list(t: &dyn DnsTransport, zone_id: &str, creds: &Credentials) -> Result<Vec<Record>, String> {
        let token = cred_required(creds.cloudflare_token(), "api-token")?;
        let resp = t.send(&HttpRequest {
            method: Method::Get,
            url: format!("{API}/zones/{zone_id}/dns_records?per_page=100"),
            headers: auth(token),
            body: None,
        })?;
        let json = check_cf(&resp, "list dns_records")?;
        let mut out = Vec::new();
        if let Some(arr) = json.get("result").and_then(|v| v.as_array()) {
            for r in arr {
                out.push(Record {
                    name: r.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    record_type: r.get("type").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    value: r.get("content").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    ttl: r.get("ttl").and_then(serde_json::Value::as_i64),
                });
            }
        }
        Ok(out)
    }

    fn find_record(
        t: &dyn DnsTransport,
        token: &str,
        zone_id: &str,
        rtype: RecordType,
        name: &str,
    ) -> Result<Option<String>, String> {
        let resp = t.send(&HttpRequest {
            method: Method::Get,
            url: format!("{API}/zones/{zone_id}/dns_records?type={}&name={name}", rtype.as_str()),
            headers: auth(token),
            body: None,
        })?;
        let json = check_cf(&resp, "lookup dns_record")?;
        Ok(json
            .get("result")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|r| r.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_string))
    }

    fn auth(token: &str) -> Vec<(String, String)> {
        vec![
            ("authorization".into(), format!("Bearer {token}")),
            ("content-type".into(), "application/json".into()),
        ]
    }

    /// Cloudflare answers 200 with `{"success":bool,"errors":[…]}`. Treat
    /// `success:false` as an error even on HTTP 200.
    fn check_cf(resp: &HttpResponse, what: &str) -> Result<serde_json::Value, String> {
        let json: serde_json::Value = serde_json::from_str(&resp.body)
            .map_err(|e| format!("cloudflare {what}: response not JSON ({e})"))?;
        if !resp.is_success() || json.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
            let errs = json.get("errors").map(ToString::to_string).unwrap_or_default();
            return Err(format!("cloudflare {what} failed ({}): {errs}", resp.status));
        }
        Ok(json)
    }

    fn outcome(req: &ChangeRequest) -> Outcome {
        Outcome {
            provider: Provider::Cloudflare,
            action: req.action,
            name: req.name.clone(),
            record_type: req.record_type,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// AWS Signature Version 4
// ─────────────────────────────────────────────────────────────────────

/// All inputs needed to sign one request. Pure — no IO — so it is unit
/// tested against AWS's published example vector.
pub struct Sigv4<'a> {
    pub method: &'a str,
    pub host: &'a str,
    /// Already URI-encoded absolute path.
    pub canonical_uri: &'a str,
    /// Already-canonical query string (sorted, encoded). Empty for none.
    pub canonical_query: &'a str,
    pub payload: &'a [u8],
    pub region: &'a str,
    pub service: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub session_token: Option<&'a str>,
    pub amz: AmzDate,
}

impl Sigv4<'_> {
    /// The `(name, value)` header pairs to attach to the outgoing request
    /// (`x-amz-date`, optional `x-amz-security-token`, `authorization`).
    /// The `host` header is not returned — the HTTP client sets it, and
    /// it is folded into the signature here using `self.host`.
    pub fn signed_headers(&self) -> Vec<(String, String)> {
        let (authorization, _) = self.compute();
        let mut headers = vec![("x-amz-date".to_string(), self.amz.iso.clone())];
        if let Some(tok) = self.session_token {
            headers.push(("x-amz-security-token".to_string(), tok.to_string()));
        }
        headers.push(("authorization".to_string(), authorization));
        headers
    }

    /// Returns `(authorization_header, hex_signature)`.
    fn compute(&self) -> (String, String) {
        let payload_hash = sha256_hex(self.payload);

        // Canonical + signed headers. host and x-amz-date are always
        // signed; x-amz-security-token too when present. Sorted by name.
        let mut signed: Vec<(String, String)> = vec![
            ("host".to_string(), self.host.to_string()),
            ("x-amz-date".to_string(), self.amz.iso.clone()),
        ];
        if let Some(tok) = self.session_token {
            signed.push(("x-amz-security-token".to_string(), tok.to_string()));
        }
        signed.sort_by(|a, b| a.0.cmp(&b.0));

        let canonical_headers: String =
            signed.iter().map(|(k, v)| format!("{}:{}\n", k, v.trim())).collect();
        let signed_headers = signed.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            self.method, self.canonical_uri, self.canonical_query, canonical_headers, signed_headers, payload_hash
        );

        let scope = format!("{}/{}/{}/aws4_request", self.amz.ymd, self.region, self.service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            self.amz.iso,
            scope,
            sha256_hex(canonical_request.as_bytes())
        );

        let signing_key = self.signing_key();
        let signature = hex::encode(hmac(&signing_key, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, scope, signed_headers, signature
        );
        (authorization, signature)
    }

    fn signing_key(&self) -> Vec<u8> {
        let k_date = hmac(format!("AWS4{}", self.secret_key).as_bytes(), self.amz.ymd.as_bytes());
        let k_region = hmac(&k_date, self.region.as_bytes());
        let k_service = hmac(&k_region, self.service.as_bytes());
        hmac(&k_service, b"aws4_request")
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts a key of any length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// AWS request date in the two forms SigV4 needs: `YYYYMMDD` (scope) and
/// `YYYYMMDDTHHMMSSZ` (x-amz-date). Built from a Unix timestamp with a
/// pure civil-date conversion so it's deterministic + test-vector-able.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmzDate {
    pub ymd: String,
    pub iso: String,
}

impl AmzDate {
    pub fn from_unix(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let rem = secs % 86_400;
        let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        let (y, mo, d) = civil_from_days(days);
        AmzDate {
            ymd: format!("{y:04}{mo:02}{d:02}"),
            iso: format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z"),
        }
    }
}

/// Howard Hinnant's days→civil algorithm (`z` = days since 1970-01-01).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// AWS path canonicalization: URI-encode every segment, keep `/`
/// unencoded. Unreserved chars (`A-Za-z0-9-._~`) pass through.
fn uri_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// Lisp arg helpers
// ─────────────────────────────────────────────────────────────────────

/// Parsed keyword arguments: alternating `:key value` pairs in `args`.
struct Kwargs<'a> {
    map: HashMap<String, &'a Value>,
}

impl<'a> Kwargs<'a> {
    fn parse(args: &'a [Value], fn_name: &str, sp: tatara_lisp::Span) -> Result<Self, EvalError> {
        if args.len() % 2 != 0 {
            return Err(EvalError::native_fn(
                fn_name,
                "expects keyword arguments — got an odd number of values",
                sp,
            ));
        }
        let mut map = HashMap::new();
        let mut i = 0;
        while i < args.len() {
            let key = match &args[i] {
                Value::Keyword(k) | Value::Symbol(k) | Value::Str(k) => k.as_ref().to_string(),
                other => {
                    return Err(EvalError::native_fn(
                        fn_name,
                        format!("argument {} must be a keyword like :provider, got {}", i + 1, other.type_name()),
                        sp,
                    ))
                }
            };
            map.insert(key, &args[i + 1]);
            i += 2;
        }
        Ok(Self { map })
    }

    fn provider(&self, fn_name: &str, sp: tatara_lisp::Span) -> Result<Provider, EvalError> {
        let v = self.want("provider", fn_name, sp)?;
        let s = as_word(v).ok_or_else(|| {
            EvalError::native_fn(fn_name, ":provider must be a keyword/string", sp)
        })?;
        Provider::from_keyword(s)
            .ok_or_else(|| EvalError::native_fn(fn_name, format!("unknown :provider '{s}'"), sp))
    }

    fn record_type(&self, fn_name: &str, sp: tatara_lisp::Span) -> Result<RecordType, EvalError> {
        let v = self.want("record-type", fn_name, sp)?;
        let s = as_word(v).ok_or_else(|| {
            EvalError::native_fn(fn_name, ":record-type must be a keyword/string", sp)
        })?;
        RecordType::from_keyword(s)
            .ok_or_else(|| EvalError::native_fn(fn_name, format!("unsupported :record-type '{s}'"), sp))
    }

    fn credentials(&self, fn_name: &str, sp: tatara_lisp::Span) -> Result<Credentials, EvalError> {
        let v = self.want("credentials", fn_name, sp)?;
        match v {
            Value::Map(m) => {
                let mut map = HashMap::new();
                for (k, val) in m.iter() {
                    if let (Value::Str(ks) | Value::Symbol(ks) | Value::Keyword(ks), Value::Str(vs)) =
                        (k.to_value(), val)
                    {
                        map.insert(ks.as_ref().to_string(), vs.as_ref().to_string());
                    }
                }
                Ok(Credentials { map })
            }
            Value::Nil => Ok(Credentials::default()),
            other => Err(EvalError::native_fn(
                fn_name,
                format!(":credentials must be a map, got {}", other.type_name()),
                sp,
            )),
        }
    }

    fn want(&self, key: &str, fn_name: &str, sp: tatara_lisp::Span) -> Result<&'a Value, EvalError> {
        self.map
            .get(key)
            .copied()
            .ok_or_else(|| EvalError::native_fn(fn_name, format!("missing required :{key}"), sp))
    }

    fn want_str(&self, key: &str, fn_name: &str, sp: tatara_lisp::Span) -> Result<String, EvalError> {
        match self.want(key, fn_name, sp)? {
            Value::Str(s) | Value::Symbol(s) | Value::Keyword(s) => Ok(s.as_ref().to_string()),
            other => Err(EvalError::native_fn(
                fn_name,
                format!(":{key} must be a string, got {}", other.type_name()),
                sp,
            )),
        }
    }

    fn opt_int(&self, key: &str) -> Option<i64> {
        match self.map.get(key)? {
            Value::Int(n) => Some(*n),
            Value::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    fn opt_bool(&self, key: &str) -> Option<bool> {
        match self.map.get(key)? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

fn as_word(v: &Value) -> Option<&str> {
    match v {
        Value::Keyword(s) | Value::Symbol(s) | Value::Str(s) => Some(s.as_ref()),
        _ => None,
    }
}

fn cred_required<'a>(v: Option<&'a str>, name: &str) -> Result<&'a str, String> {
    v.filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing credential '{name}' in :credentials map"))
}

fn map_value(fields: &[(&str, Value)]) -> Value {
    let mut m = HashMap::new();
    for (k, v) in fields {
        m.insert(MapKey::Keyword(Arc::from(*k)), v.clone());
    }
    Value::Map(Arc::new(m))
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ── AmzDate ──────────────────────────────────────────────────────

    #[test]
    fn amz_date_epoch() {
        let d = AmzDate::from_unix(0);
        assert_eq!(d.ymd, "19700101");
        assert_eq!(d.iso, "19700101T000000Z");
    }

    #[test]
    fn amz_date_aws_example_instant() {
        // 2015-08-30T12:36:00Z — the AWS SigV4 worked-example timestamp.
        let secs = 1_440_938_160;
        let d = AmzDate::from_unix(secs);
        assert_eq!(d.ymd, "20150830");
        assert_eq!(d.iso, "20150830T123600Z");
    }

    #[test]
    fn amz_date_leap_day() {
        // 2020-02-29T23:59:59Z
        let d = AmzDate::from_unix(1_583_020_799);
        assert_eq!(d.iso, "20200229T235959Z");
    }

    // ── SigV4 gold vectors ───────────────────────────────────────────
    //
    // AWS Signature Version 4 Test Suite "get-vanilla": GET / against
    // example.amazonaws.com, service "service", region us-east-1,
    // 20150830T123600Z, empty body, minimal signed set host;x-amz-date.
    // The expected signature is the published suite value, independently
    // re-derived with a Python stdlib (hashlib+hmac) reference impl.
    #[test]
    fn sigv4_matches_aws_get_vanilla_vector() {
        let sig = Sigv4 {
            method: "GET",
            host: "example.amazonaws.com",
            canonical_uri: "/",
            canonical_query: "",
            payload: b"",
            region: "us-east-1",
            service: "service",
            access_key: "AKIDEXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            session_token: None,
            amz: AmzDate { ymd: "20150830".into(), iso: "20150830T123600Z".into() },
        };
        let (authz, signature) = sig.compute();
        assert_eq!(signature, "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31");
        assert_eq!(
            authz,
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    /// Pins the signer for the real Route53 shape: POST with an XML body
    /// against route53.amazonaws.com, service route53. Expected signature
    /// independently computed with the Python stdlib reference.
    #[test]
    fn sigv4_route53_post_vector() {
        let sig = Sigv4 {
            method: "POST",
            host: "route53.amazonaws.com",
            canonical_uri: "/2013-04-01/hostedzone/Z123/rrset/",
            canonical_query: "",
            payload: b"<x/>",
            region: "us-east-1",
            service: "route53",
            access_key: "AKIDEXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            session_token: None,
            amz: AmzDate { ymd: "20150830".into(), iso: "20150830T123600Z".into() },
        };
        let (_authz, signature) = sig.compute();
        assert_eq!(signature, "27c5bd71813cc981dab5360ffe92bb03008c96db0ece56c2a3b9acea5e19922a");
    }

    #[test]
    fn sigv4_session_token_is_signed() {
        let sig = Sigv4 {
            method: "POST",
            host: "route53.amazonaws.com",
            canonical_uri: "/2013-04-01/hostedzone/Z123/rrset/",
            canonical_query: "",
            payload: b"<x/>",
            region: "us-east-1",
            service: "route53",
            access_key: "AKID",
            secret_key: "SECRET",
            session_token: Some("FwoTOKEN=="),
            amz: AmzDate { ymd: "20260524".into(), iso: "20260524T000000Z".into() },
        };
        let headers = sig.signed_headers();
        assert!(headers.iter().any(|(k, v)| k == "x-amz-security-token" && v == "FwoTOKEN=="));
        let authz = headers.iter().find(|(k, _)| k == "authorization").unwrap();
        assert!(authz.1.contains("SignedHeaders=host;x-amz-date;x-amz-security-token"));
    }

    // ── Route53 typed XML ────────────────────────────────────────────

    #[test]
    fn change_batch_xml_upsert() {
        let xml = ChangeBatch {
            action: "UPSERT",
            name: "akeyless-saas.ab12cd34.pleme-dev.use1.quero.lol",
            rtype: "CNAME",
            ttl: 60,
            value: "tunnel.cfargotunnel.com",
        }
        .to_string();
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<Action>UPSERT</Action>"));
        assert!(xml.contains("<Name>akeyless-saas.ab12cd34.pleme-dev.use1.quero.lol</Name>"));
        assert!(xml.contains("<Type>CNAME</Type>"));
        assert!(xml.contains("<TTL>60</TTL>"));
        assert!(xml.contains("<Value>tunnel.cfargotunnel.com</Value>"));
    }

    #[test]
    fn change_batch_xml_escapes_values() {
        let xml = ChangeBatch { action: "UPSERT", name: "a&b", rtype: "TXT", ttl: 300, value: "x<y>\"z\"" }
            .to_string();
        assert!(xml.contains("<Name>a&amp;b</Name>"));
        assert!(xml.contains("&lt;y&gt;"));
        assert!(xml.contains("&quot;z&quot;"));
    }

    // ── MockTransport ────────────────────────────────────────────────

    #[derive(Default)]
    struct MockTransport {
        responses: RefCell<Vec<HttpResponse>>,
        seen: RefCell<Vec<(Method, String, Vec<(String, String)>, Option<String>)>>,
    }
    impl MockTransport {
        fn with(responses: Vec<(u16, &str)>) -> Self {
            Self {
                responses: RefCell::new(
                    responses.into_iter().rev().map(|(s, b)| HttpResponse { status: s, body: b.to_string() }).collect(),
                ),
                seen: RefCell::new(Vec::new()),
            }
        }
    }
    impl DnsTransport for MockTransport {
        fn send(&self, req: &HttpRequest) -> Result<HttpResponse, String> {
            self.seen
                .borrow_mut()
                .push((req.method, req.url.clone(), req.headers.clone(), req.body.clone()));
            self.responses.borrow_mut().pop().ok_or_else(|| "mock: no response queued".to_string())
        }
    }

    fn route53_creds() -> Credentials {
        let mut map = HashMap::new();
        map.insert("access-key-id".to_string(), "AKIDEXAMPLE".to_string());
        map.insert("secret-access-key".to_string(), "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string());
        Credentials { map }
    }

    #[test]
    fn route53_upsert_signs_and_posts() {
        let t = MockTransport::with(vec![(200, "<ChangeResourceRecordSetsResponse/>")]);
        let req = ChangeRequest {
            provider: Provider::Route53,
            zone_id: "/hostedzone/Z3M3LMPEXAMPLE".into(),
            credentials: route53_creds(),
            record_type: RecordType::Cname,
            name: "x.pleme-dev.use1.quero.lol".into(),
            value: "tunnel.cfargotunnel.com".into(),
            ttl: 60,
            proxied: false,
            action: Action::Upsert,
        };
        let outcome = route53::change(&t, &req, 1_440_938_160).expect("upsert ok");
        assert_eq!(outcome.provider, Provider::Route53);

        let seen = t.seen.borrow();
        assert_eq!(seen.len(), 1);
        let (method, url, headers, body) = &seen[0];
        assert_eq!(*method, Method::Post);
        // Leading /hostedzone/ stripped from the operator-pasted zone id.
        assert_eq!(url, "https://route53.amazonaws.com/2013-04-01/hostedzone/Z3M3LMPEXAMPLE/rrset/");
        assert!(headers.iter().any(|(k, v)| k == "authorization" && v.starts_with("AWS4-HMAC-SHA256 ")));
        assert!(headers.iter().any(|(k, _)| k == "x-amz-date"));
        assert!(body.as_ref().unwrap().contains("<Action>UPSERT</Action>"));
    }

    #[test]
    fn route53_upsert_surfaces_api_error() {
        let t = MockTransport::with(vec![(403, "<ErrorResponse><Error><Code>AccessDenied</Code></Error></ErrorResponse>")]);
        let req = ChangeRequest {
            provider: Provider::Route53,
            zone_id: "Z1".into(),
            credentials: route53_creds(),
            record_type: RecordType::A,
            name: "x".into(),
            value: "1.2.3.4".into(),
            ttl: 60,
            proxied: false,
            action: Action::Upsert,
        };
        let err = route53::change(&t, &req, 0).unwrap_err();
        assert!(err.contains("403"), "expected status in error: {err}");
        assert!(err.contains("AccessDenied"), "expected body in error: {err}");
    }

    #[test]
    fn route53_missing_creds_is_typed_error() {
        let t = MockTransport::with(vec![]);
        let req = ChangeRequest {
            provider: Provider::Route53,
            zone_id: "Z1".into(),
            credentials: Credentials::default(),
            record_type: RecordType::A,
            name: "x".into(),
            value: "1.2.3.4".into(),
            ttl: 60,
            proxied: false,
            action: Action::Upsert,
        };
        let err = route53::change(&t, &req, 0).unwrap_err();
        assert!(err.contains("access-key-id"), "{err}");
    }

    #[test]
    fn cloudflare_upsert_creates_when_absent() {
        // 1st call: lookup → empty result. 2nd: create → success.
        let t = MockTransport::with(vec![
            (200, r#"{"success":true,"errors":[],"result":[]}"#),
            (200, r#"{"success":true,"errors":[],"result":{"id":"new"}}"#),
        ]);
        let mut map = HashMap::new();
        map.insert("api-token".to_string(), "cf-token".to_string());
        let req = ChangeRequest {
            provider: Provider::Cloudflare,
            zone_id: "zone1".into(),
            credentials: Credentials { map },
            record_type: RecordType::Cname,
            name: "x.quero.lol".into(),
            value: "target.example".into(),
            ttl: 60,
            proxied: false,
            action: Action::Upsert,
        };
        cloudflare::change(&t, &req).expect("cf upsert ok");
        let seen = t.seen.borrow();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, Method::Get); // lookup
        assert_eq!(seen[1].0, Method::Post); // create (no existing id)
        assert!(seen[0].2.iter().any(|(k, v)| k == "authorization" && v == "Bearer cf-token"));
    }

    #[test]
    fn cloudflare_upsert_updates_when_present() {
        let t = MockTransport::with(vec![
            (200, r#"{"success":true,"errors":[],"result":[{"id":"abc123"}]}"#),
            (200, r#"{"success":true,"errors":[],"result":{"id":"abc123"}}"#),
        ]);
        let mut map = HashMap::new();
        map.insert("token".to_string(), "cf-token".to_string());
        let req = ChangeRequest {
            provider: Provider::Cloudflare,
            zone_id: "zone1".into(),
            credentials: Credentials { map },
            record_type: RecordType::A,
            name: "x.quero.lol".into(),
            value: "1.2.3.4".into(),
            ttl: 120,
            proxied: true,
            action: Action::Upsert,
        };
        cloudflare::change(&t, &req).expect("cf update ok");
        let seen = t.seen.borrow();
        assert_eq!(seen[1].0, Method::Put); // existing id → PUT
        assert!(seen[1].1.ends_with("/dns_records/abc123"));
    }

    // ── parsing + dispatch ───────────────────────────────────────────

    #[test]
    fn record_type_accepts_both_keyword_dialects() {
        for s in ["A", "a"] {
            assert_eq!(RecordType::from_keyword(s), Some(RecordType::A));
        }
        for s in ["AAAA", "aaaa", "a-a-a-a"] {
            assert_eq!(RecordType::from_keyword(s), Some(RecordType::Aaaa));
        }
        for s in ["CNAME", "cname", "c-n-a-m-e"] {
            assert_eq!(RecordType::from_keyword(s), Some(RecordType::Cname));
        }
        for s in ["TXT", "t-x-t"] {
            assert_eq!(RecordType::from_keyword(s), Some(RecordType::Txt));
        }
        assert_eq!(RecordType::from_keyword("mx"), None);
    }

    #[test]
    fn unimplemented_providers_error_not_silent() {
        let t = MockTransport::with(vec![]);
        for p in [Provider::Hetzner, Provider::Gcp] {
            let req = ChangeRequest {
                provider: p,
                zone_id: "z".into(),
                credentials: Credentials::default(),
                record_type: RecordType::A,
                name: "x".into(),
                value: "1.2.3.4".into(),
                ttl: 60,
                proxied: false,
                action: Action::Upsert,
            };
            let err = dns_change(&t, &req).unwrap_err();
            assert!(err.contains("not implemented"), "{err}");
        }
    }

    #[test]
    fn txt_values_get_route53_quoting() {
        let xml = ChangeBatch {
            action: "UPSERT",
            name: "_acme.quero.lol",
            rtype: "TXT",
            ttl: 60,
            value: &route53::formatted_value(RecordType::Txt, "token123"),
        }
        .to_string();
        assert!(xml.contains("<Value>&quot;token123&quot;</Value>"), "{xml}");
    }

    #[test]
    fn route53_list_parses_rrsets() {
        let xml = r#"<ListResourceRecordSetsResponse>
          <ResourceRecordSets>
            <ResourceRecordSet><Name>a.quero.lol.</Name><Type>A</Type><TTL>60</TTL>
              <ResourceRecords><ResourceRecord><Value>1.2.3.4</Value></ResourceRecord></ResourceRecords>
            </ResourceRecordSet>
            <ResourceRecordSet><Name>cn.quero.lol.</Name><Type>CNAME</Type><TTL>300</TTL>
              <ResourceRecords><ResourceRecord><Value>tgt.example</Value></ResourceRecord></ResourceRecords>
            </ResourceRecordSet>
          </ResourceRecordSets>
        </ListResourceRecordSetsResponse>"#;
        let t = MockTransport::with(vec![(200, xml)]);
        let recs = route53::list(&t, "Z1", &route53_creds(), 0).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].name, "a.quero.lol.");
        assert_eq!(recs[0].record_type, "A");
        assert_eq!(recs[0].value, "1.2.3.4");
        assert_eq!(recs[0].ttl, Some(60));
        assert_eq!(recs[1].record_type, "CNAME");
    }
}
