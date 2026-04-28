//! URL grammar parsing — same shape as Nix flake refs.

use std::fmt;
use std::path::PathBuf;

use crate::ResolveError;

/// Typed source — every variant carries everything needed to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Local file path (`./foo.tlisp` or absolute).
    Local { path: PathBuf },

    /// `github:owner/repo/path?ref=<tag-or-commit>`
    GitHub {
        owner: String,
        repo: String,
        path: PathBuf,
        rev: Option<String>,
    },

    /// `gitlab:owner/repo/path?ref=<...>`
    GitLab {
        owner: String,
        repo: String,
        path: PathBuf,
        rev: Option<String>,
    },

    /// `codeberg:owner/repo/path?ref=<...>`
    Codeberg {
        owner: String,
        repo: String,
        path: PathBuf,
        rev: Option<String>,
    },

    /// `https://...` or `http://...` — optionally pinned via `#blake3=<hex>`.
    HttpDirect { url: String, blake3: Option<String> },
}

impl Source {
    /// Parse a URL string into a typed Source.
    pub fn parse(input: &str) -> Result<Self, ResolveError> {
        // Local file: starts with `.` `/` `~` or has no `:` outside path
        if input.starts_with('.')
            || input.starts_with('/')
            || input.starts_with('~')
            || (!input.contains(':') && !input.is_empty())
        {
            return Ok(Source::Local {
                path: PathBuf::from(input),
            });
        }

        // file:// scheme
        if let Some(rest) = input.strip_prefix("file://") {
            return Ok(Source::Local {
                path: PathBuf::from(rest),
            });
        }

        // http(s):// — possibly with #blake3= fragment
        if input.starts_with("http://") || input.starts_with("https://") {
            return parse_http(input);
        }

        // forge: github:, gitlab:, codeberg:
        if let Some(rest) = input.strip_prefix("github:") {
            return parse_forge(rest, ForgeKind::GitHub);
        }
        if let Some(rest) = input.strip_prefix("gitlab:") {
            return parse_forge(rest, ForgeKind::GitLab);
        }
        if let Some(rest) = input.strip_prefix("codeberg:") {
            return parse_forge(rest, ForgeKind::Codeberg);
        }

        Err(ResolveError::BadUrl(input.into()))
    }

    /// Cache key — a stable string that identifies this exact resolution
    /// target. Two URLs that resolve to the same bytes share a cache key.
    pub fn cache_key(&self) -> String {
        match self {
            Source::Local { path } => format!("local:{}", path.display()),
            Source::GitHub {
                owner,
                repo,
                path,
                rev,
            } => {
                format!(
                    "github:{owner}/{repo}/{}@{}",
                    path.display(),
                    rev.as_deref().unwrap_or("HEAD")
                )
            }
            Source::GitLab {
                owner,
                repo,
                path,
                rev,
            } => {
                format!(
                    "gitlab:{owner}/{repo}/{}@{}",
                    path.display(),
                    rev.as_deref().unwrap_or("HEAD")
                )
            }
            Source::Codeberg {
                owner,
                repo,
                path,
                rev,
            } => {
                format!(
                    "codeberg:{owner}/{repo}/{}@{}",
                    path.display(),
                    rev.as_deref().unwrap_or("HEAD")
                )
            }
            Source::HttpDirect { url, .. } => format!("http:{url}"),
        }
    }

    /// If the URL had a content-pin (`#blake3=<hex>`), return it.
    pub fn declared_blake3(&self) -> Option<&str> {
        match self {
            Source::HttpDirect { blake3, .. } => blake3.as_deref(),
            _ => None,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Local { path } => write!(f, "{}", path.display()),
            Source::GitHub {
                owner,
                repo,
                path,
                rev,
            } => {
                write!(f, "github:{owner}/{repo}/{}", path.display())?;
                if let Some(r) = rev {
                    write!(f, "?ref={r}")?;
                }
                Ok(())
            }
            Source::GitLab {
                owner,
                repo,
                path,
                rev,
            } => {
                write!(f, "gitlab:{owner}/{repo}/{}", path.display())?;
                if let Some(r) = rev {
                    write!(f, "?ref={r}")?;
                }
                Ok(())
            }
            Source::Codeberg {
                owner,
                repo,
                path,
                rev,
            } => {
                write!(f, "codeberg:{owner}/{repo}/{}", path.display())?;
                if let Some(r) = rev {
                    write!(f, "?ref={r}")?;
                }
                Ok(())
            }
            Source::HttpDirect { url, blake3 } => {
                write!(f, "{url}")?;
                if let Some(b) = blake3 {
                    write!(f, "#blake3={b}")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Copy, Clone)]
enum ForgeKind {
    GitHub,
    GitLab,
    Codeberg,
}

fn parse_forge(rest: &str, kind: ForgeKind) -> Result<Source, ResolveError> {
    // Split off ?query before path-walking.
    let (path_part, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };

    let parts: Vec<&str> = path_part.split('/').collect();
    if parts.len() < 3 {
        return Err(ResolveError::BadUrl(format!(
            "expected owner/repo/path/..., got {path_part:?}"
        )));
    }

    let owner = parts[0].to_string();
    let repo = parts[1].to_string();
    let path = PathBuf::from(parts[2..].join("/"));

    if owner.is_empty() || repo.is_empty() || path.as_os_str().is_empty() {
        return Err(ResolveError::BadUrl(format!(
            "owner/repo/path must all be non-empty in {path_part:?}"
        )));
    }

    let mut rev = None;
    if let Some(q) = query {
        for kv in q.split('&') {
            if let Some(v) = kv.strip_prefix("ref=") {
                rev = Some(v.to_string());
            }
        }
    }

    Ok(match kind {
        ForgeKind::GitHub => Source::GitHub {
            owner,
            repo,
            path,
            rev,
        },
        ForgeKind::GitLab => Source::GitLab {
            owner,
            repo,
            path,
            rev,
        },
        ForgeKind::Codeberg => Source::Codeberg {
            owner,
            repo,
            path,
            rev,
        },
    })
}

fn parse_http(input: &str) -> Result<Source, ResolveError> {
    // Split off #fragment.
    let (url, fragment) = match input.split_once('#') {
        Some((u, f)) => (u.to_string(), Some(f)),
        None => (input.to_string(), None),
    };

    let blake3 = fragment.and_then(|f| {
        f.split('&')
            .find_map(|kv| kv.strip_prefix("blake3=").map(str::to_string))
    });

    Ok(Source::HttpDirect { url, blake3 })
}
