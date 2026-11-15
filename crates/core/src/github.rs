//! A small typed client for the GitHub REST calls the IDE makes.
//!
//! Every endpoint used here was checked against the live API on 2026-11-15
//! (version header `2022-11-28`). Writes go through the Git Data API so that
//! one checkpoint is one commit no matter how many files it touches.

use std::fmt::Write as _;
use std::rc::Rc;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;

use crate::error::{Error, Result};
use crate::http::{Http, Method, Request, Response};

/// Git file modes as GitHub reports them.
pub const MODE_FILE: &str = "100644";
pub const MODE_EXECUTABLE: &str = "100755";
pub const MODE_SYMLINK: &str = "120000";
pub const MODE_TREE: &str = "040000";
pub const MODE_SUBMODULE: &str = "160000";

/// The signed-in account.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub login: String,
}

/// A repository the token can see.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Repo {
    pub full_name: String,
    pub default_branch: String,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub description: Option<String>,
}

/// One entry of a git tree. `path` is a single name unless the tree was
/// fetched recursively.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub sha: String,
    #[serde(default)]
    pub size: Option<u64>,
}

/// A git tree as returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tree {
    pub sha: String,
    pub tree: Vec<TreeEntry>,
    /// GitHub cut the listing short. Only possible for recursive listings.
    #[serde(default)]
    pub truncated: bool,
}

/// A git commit object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub tree: String,
    pub parents: Vec<String>,
    pub message: String,
}

/// One line of history, for `git log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub date: String,
}

/// A change to make when writing a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeChange {
    /// Create or replace a text file.
    Write {
        path: String,
        mode: String,
        content: String,
    },
    /// Point a path at an existing blob (used to move files without re-uploading).
    Link {
        path: String,
        mode: String,
        sha: String,
    },
    /// Remove a file.
    Delete { path: String },
}

#[derive(Deserialize)]
struct ShaOnly {
    sha: String,
}

#[derive(Deserialize)]
struct RefBody {
    object: ShaOnly,
}

#[derive(Deserialize)]
struct CommitBody {
    sha: String,
    tree: ShaOnly,
    #[serde(default)]
    parents: Vec<ShaOnly>,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct BlobBody {
    content: String,
    encoding: String,
}

#[derive(Deserialize)]
struct BranchBody {
    name: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct LogAuthor {
    #[serde(default)]
    name: String,
    #[serde(default)]
    date: String,
}

#[derive(Deserialize)]
struct LogCommit {
    #[serde(default)]
    message: String,
    author: Option<LogAuthor>,
}

#[derive(Deserialize)]
struct LogBody {
    sha: String,
    commit: LogCommit,
}

/// The GitHub client. Cheap to clone.
#[derive(Clone)]
pub struct GitHub {
    http: Rc<dyn Http>,
}

impl GitHub {
    /// Wraps a transport.
    #[must_use]
    pub fn new(http: Rc<dyn Http>) -> Self {
        Self { http }
    }

    async fn call(&self, method: Method, path: String, body: Option<String>) -> Result<Response> {
        let label = path.clone();
        let response = self
            .http
            .send(Request { method, path, body })
            .await
            .map_err(Error::Network)?;
        if (200..300).contains(&response.status) {
            return Ok(response);
        }
        let message = serde_json::from_str::<ErrorBody>(&response.body)
            .map(|body| body.message)
            .unwrap_or_default();
        Err(match response.status {
            401 => Error::Unauthorized,
            403 | 429 if response.rate_limit_remaining == Some(0) => Error::RateLimited {
                reset_epoch: response.rate_limit_reset,
            },
            403 => Error::Forbidden(message),
            404 => Error::NotFound(label),
            409 | 422 => Error::Rejected(message),
            status => Error::Api { status, message },
        })
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: String) -> Result<T> {
        let response = self.call(Method::Get, path, None).await?;
        decode(&response.body)
    }

    async fn send<T: for<'de> Deserialize<'de>>(
        &self,
        method: Method,
        path: String,
        body: &serde_json::Value,
    ) -> Result<T> {
        let response = self.call(method, path, Some(body.to_string())).await?;
        decode(&response.body)
    }

    /// Who the token belongs to. Also the cheapest way to check a token works.
    pub async fn user(&self) -> Result<User> {
        self.get("/user".to_owned()).await
    }

    /// Repositories the token can reach, most recently pushed first.
    pub async fn repos(&self) -> Result<Vec<Repo>> {
        self.get("/user/repos?sort=pushed&per_page=100".to_owned())
            .await
    }

    /// One repository by `owner/name`.
    pub async fn repo(&self, repo: &str) -> Result<Repo> {
        self.get(format!("/repos/{}", encode(repo))).await
    }

    /// Branch names, up to 300.
    pub async fn branches(&self, repo: &str) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for page in 1..=3 {
            let batch: Vec<BranchBody> = self
                .get(format!(
                    "/repos/{}/branches?per_page=100&page={page}",
                    encode(repo)
                ))
                .await?;
            let last = batch.len() < 100;
            names.extend(batch.into_iter().map(|branch| branch.name));
            if last {
                break;
            }
        }
        Ok(names)
    }

    /// The commit a branch points at, or `None` if the branch does not exist.
    pub async fn branch_head(&self, repo: &str, branch: &str) -> Result<Option<String>> {
        let path = format!("/repos/{}/git/ref/heads/{}", encode(repo), encode(branch));
        match self.get::<RefBody>(path).await {
            Ok(body) => Ok(Some(body.object.sha)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// A commit object.
    pub async fn commit(&self, repo: &str, sha: &str) -> Result<Commit> {
        let body: CommitBody = self
            .get(format!(
                "/repos/{}/git/commits/{}",
                encode(repo),
                encode(sha)
            ))
            .await?;
        Ok(Commit {
            sha: body.sha,
            tree: body.tree.sha,
            parents: body.parents.into_iter().map(|parent| parent.sha).collect(),
            message: body.message,
        })
    }

    /// One level of a tree, or the whole subtree when `recursive`.
    pub async fn tree(&self, repo: &str, sha: &str, recursive: bool) -> Result<Tree> {
        let suffix = if recursive { "?recursive=1" } else { "" };
        self.get(format!(
            "/repos/{}/git/trees/{}{suffix}",
            encode(repo),
            encode(sha)
        ))
        .await
    }

    /// The bytes of a blob.
    pub async fn blob(&self, repo: &str, sha: &str) -> Result<Vec<u8>> {
        let body: BlobBody = self
            .get(format!("/repos/{}/git/blobs/{}", encode(repo), encode(sha)))
            .await?;
        if body.encoding != "base64" {
            return Err(Error::Decode(format!(
                "blob {sha} has encoding {}",
                body.encoding
            )));
        }
        // GitHub wraps the base64 at 60 columns.
        let compact: String = body
            .content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(compact)
            .map_err(|error| Error::Decode(format!("blob {sha}: {error}")))
    }

    /// Writes a new tree made of `base_tree` plus `changes`. Returns its sha.
    pub async fn create_tree(
        &self,
        repo: &str,
        base_tree: &str,
        changes: &[TreeChange],
    ) -> Result<String> {
        let entries: Vec<serde_json::Value> = changes
            .iter()
            .map(|change| match change {
                TreeChange::Write {
                    path,
                    mode,
                    content,
                } => json!({"path": path, "mode": mode, "type": "blob", "content": content}),
                TreeChange::Link { path, mode, sha } => {
                    json!({"path": path, "mode": mode, "type": "blob", "sha": sha})
                }
                TreeChange::Delete { path } => {
                    json!({"path": path, "mode": MODE_FILE, "type": "blob", "sha": null})
                }
            })
            .collect();
        let body: ShaOnly = self
            .send(
                Method::Post,
                format!("/repos/{}/git/trees", encode(repo)),
                &json!({"base_tree": base_tree, "tree": entries}),
            )
            .await?;
        Ok(body.sha)
    }

    /// Writes a commit object. It is not on any branch until a ref points at it.
    pub async fn create_commit(
        &self,
        repo: &str,
        message: &str,
        tree: &str,
        parents: &[String],
    ) -> Result<String> {
        let body: ShaOnly = self
            .send(
                Method::Post,
                format!("/repos/{}/git/commits", encode(repo)),
                &json!({"message": message, "tree": tree, "parents": parents}),
            )
            .await?;
        Ok(body.sha)
    }

    /// Moves a branch forward to `sha`. Never forces: if the branch has moved
    /// so that this is not a fast-forward, GitHub refuses and this returns
    /// [`Error::Rejected`].
    pub async fn advance_branch(&self, repo: &str, branch: &str, sha: &str) -> Result<()> {
        let _: serde_json::Value = self
            .send(
                Method::Patch,
                format!("/repos/{}/git/refs/heads/{}", encode(repo), encode(branch)),
                &json!({"sha": sha, "force": false}),
            )
            .await?;
        Ok(())
    }

    /// Creates a branch at `sha`. Fails if it already exists.
    pub async fn create_branch(&self, repo: &str, branch: &str, sha: &str) -> Result<()> {
        let _: serde_json::Value = self
            .send(
                Method::Post,
                format!("/repos/{}/git/refs", encode(repo)),
                &json!({"ref": format!("refs/heads/{branch}"), "sha": sha}),
            )
            .await?;
        Ok(())
    }

    /// The newest `count` commits on a branch.
    pub async fn log(&self, repo: &str, branch: &str, count: usize) -> Result<Vec<LogEntry>> {
        let count = count.clamp(1, 100);
        let entries: Vec<LogBody> = self
            .get(format!(
                "/repos/{}/commits?sha={}&per_page={count}",
                encode(repo),
                encode_query(branch)
            ))
            .await?;
        Ok(entries
            .into_iter()
            .map(|entry| {
                let (author, date) = entry
                    .commit
                    .author
                    .map(|author| (author.name, author.date))
                    .unwrap_or_default();
                LogEntry {
                    sha: entry.sha,
                    message: entry.commit.message,
                    author,
                    date,
                }
            })
            .collect())
    }
}

fn decode<T: for<'de> Deserialize<'de>>(body: &str) -> Result<T> {
    serde_json::from_str(body).map_err(|error| Error::Decode(error.to_string()))
}

/// Percent-encodes a path that may contain slashes (repo names, branch names).
/// Slashes are kept; everything outside the unreserved set is escaped.
#[must_use]
pub fn encode(value: &str) -> String {
    escape(value, true)
}

/// Percent-encodes a query-string value, slashes included.
#[must_use]
pub fn encode_query(value: &str) -> String {
    escape(value, false)
}

fn escape(value: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && byte == b'/');
        if keep {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_paths_and_queries() {
        assert_eq!(encode("owner/repo"), "owner/repo");
        assert_eq!(encode("feature/a b#1"), "feature/a%20b%231");
        assert_eq!(encode_query("feature/x&y"), "feature%2Fx%26y");
        assert_eq!(encode("naïve"), "na%C3%AFve");
    }
}
