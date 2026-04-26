//! Source fetchers — one per [`crate::Source`] variant.

use std::time::Duration;

use crate::{parse::Source, ResolveError};

pub(crate) fn fetch(
    source: &Source,
    timeout: Duration,
    user_agent: &str,
) -> Result<Vec<u8>, ResolveError> {
    match source {
        Source::Local { path } => fetch_local(path),
        Source::GitHub { owner, repo, path, rev } => {
            let raw_url = format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/{}/{}",
                rev.as_deref().unwrap_or("HEAD"),
                path.display()
            );
            fetch_http(&raw_url, timeout, user_agent)
        }
        Source::GitLab { owner, repo, path, rev } => {
            // GitLab raw endpoint expects URL-encoded path.
            let raw_url = format!(
                "https://gitlab.com/{owner}/{repo}/-/raw/{}/{}",
                rev.as_deref().unwrap_or("main"),
                path.display()
            );
            fetch_http(&raw_url, timeout, user_agent)
        }
        Source::Codeberg { owner, repo, path, rev } => {
            let raw_url = format!(
                "https://codeberg.org/{owner}/{repo}/raw/branch/{}/{}",
                rev.as_deref().unwrap_or("main"),
                path.display()
            );
            fetch_http(&raw_url, timeout, user_agent)
        }
        Source::HttpDirect { url, .. } => fetch_http(url, timeout, user_agent),
    }
}

fn fetch_local(path: &std::path::Path) -> Result<Vec<u8>, ResolveError> {
    std::fs::read(path).map_err(|e| ResolveError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

fn fetch_http(url: &str, timeout: Duration, user_agent: &str) -> Result<Vec<u8>, ResolveError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .user_agent(user_agent)
        .build();

    let mut req = agent.get(url);

    // Optional GitHub auth: pass GH_TOKEN env var into Authorization header.
    if url.starts_with("https://raw.githubusercontent.com/") {
        if let Ok(token) = std::env::var("GH_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN")) {
            if !token.is_empty() {
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
        }
    }

    let response = req.call().map_err(|e| match e {
        ureq::Error::Status(status, _) => ResolveError::Http {
            url: url.into(),
            status,
        },
        ureq::Error::Transport(t) => ResolveError::Network {
            url: url.into(),
            source: Box::new(t),
        },
    })?;

    let mut buf = Vec::new();
    response.into_reader().read_to_end(&mut buf).map_err(|e| ResolveError::Io {
        path: url.into(),
        source: e,
    })?;
    Ok(buf)
}
