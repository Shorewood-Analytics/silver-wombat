//! An in-memory GitHub that speaks the same REST calls as the real one.
//!
//! It backs the unit tests and the app's demo mode, where nothing leaves the
//! browser. Objects get real git object ids, so "same content, same sha"
//! holds exactly as it does on GitHub.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use base64::Engine as _;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::github::{MODE_FILE, MODE_TREE, TreeEntry};
use crate::http::{Http, LocalBoxFuture, Method, Request, Response};

#[derive(Clone)]
struct FakeCommit {
    tree: String,
    parents: Vec<String>,
    message: String,
    date: String,
}

#[derive(Default)]
struct FakeRepo {
    default_branch: String,
    refs: BTreeMap<String, String>,
    blobs: HashMap<String, Vec<u8>>,
    trees: HashMap<String, Vec<TreeEntry>>,
    commits: HashMap<String, FakeCommit>,
}

enum Leaf {
    Set { mode: String, sha: String },
    Delete,
}

#[derive(Default)]
struct State {
    login: String,
    repos: BTreeMap<String, FakeRepo>,
    token_valid: bool,
    offline: bool,
    fail_next: Option<(u16, String)>,
    clock: u64,
}

/// The fake server. Share it with `Rc` and hand it to [`crate::github::GitHub`].
pub struct FakeGitHub {
    state: RefCell<State>,
    log: RefCell<Vec<String>>,
}

type Reply = Result<Value, (u16, String)>;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The git object id of `body` stored as an object of type `kind`.
#[must_use]
pub fn object_id(kind: &str, body: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("{kind} {}\0", body.len()).as_bytes());
    hasher.update(body);
    hex(&hasher.finalize())
}

fn unhex(sha: &str) -> Vec<u8> {
    (0..sha.len() / 2)
        .filter_map(|i| u8::from_str_radix(sha.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

fn not_found() -> (u16, String) {
    (404, "Not Found".to_owned())
}

fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(code) = segment.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(code, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl FakeRepo {
    fn put_blob(&mut self, data: &[u8]) -> String {
        let sha = object_id("blob", data);
        self.blobs.insert(sha.clone(), data.to_vec());
        sha
    }

    fn put_tree(&mut self, mut entries: Vec<TreeEntry>) -> String {
        // Git orders entries as if directory names ended in a slash.
        let key = |entry: &TreeEntry| {
            let mut name = entry.path.clone().into_bytes();
            if entry.kind == "tree" {
                name.push(b'/');
            }
            name
        };
        entries.sort_by_key(key);
        let mut body = Vec::new();
        for entry in &entries {
            let mode = entry.mode.trim_start_matches('0');
            body.extend_from_slice(mode.as_bytes());
            body.push(b' ');
            body.extend_from_slice(entry.path.as_bytes());
            body.push(0);
            body.extend_from_slice(&unhex(&entry.sha));
        }
        let sha = object_id("tree", &body);
        self.trees.insert(sha.clone(), entries);
        sha
    }

    fn put_commit(&mut self, tree: &str, parents: &[String], message: &str, clock: u64) -> String {
        let mut body = format!("tree {tree}\n");
        for parent in parents {
            let _ = writeln!(body, "parent {parent}");
        }
        let stamp = 1_794_000_000 + clock;
        let _ = write!(
            body,
            "author Demo <demo@example.invalid> {stamp} +0000
committer Demo <demo@example.invalid> {stamp} +0000

{message}"
        );
        let sha = object_id("commit", body.as_bytes());
        self.commits.insert(
            sha.clone(),
            FakeCommit {
                tree: tree.to_owned(),
                parents: parents.to_vec(),
                message: message.to_owned(),
                date: format!("2026-11-15T12:{:02}:{:02}Z", (clock / 60) % 60, clock % 60),
            },
        );
        sha
    }

    /// Applies changes to a tree and returns the new tree's sha, or `None`
    /// when the result has no entries (git does not store empty directories).
    fn apply(
        &mut self,
        base: Option<&str>,
        changes: Vec<(Vec<String>, Leaf)>,
    ) -> Result<Option<String>, (u16, String)> {
        let mut entries = match base {
            Some(sha) => self
                .trees
                .get(sha)
                .cloned()
                .ok_or((422, format!("base tree {sha} does not exist")))?,
            None => Vec::new(),
        };
        let mut nested: BTreeMap<String, Vec<(Vec<String>, Leaf)>> = BTreeMap::new();
        for (mut parts, leaf) in changes {
            if parts.is_empty() {
                return Err((422, "empty path".to_owned()));
            }
            let name = parts.remove(0);
            if !parts.is_empty() {
                nested.entry(name).or_default().push((parts, leaf));
                continue;
            }
            let before = entries.len();
            entries.retain(|entry| entry.path != name);
            match leaf {
                Leaf::Set { mode, sha } => {
                    let size = self.blobs.get(&sha).map(|blob| blob.len() as u64);
                    if size.is_none() {
                        return Err((422, format!("blob {sha} does not exist")));
                    }
                    entries.push(TreeEntry {
                        path: name,
                        mode,
                        kind: "blob".to_owned(),
                        sha,
                        size,
                    });
                }
                Leaf::Delete if before == entries.len() => {
                    return Err((422, format!("cannot delete {name}: it does not exist")));
                }
                Leaf::Delete => {}
            }
        }
        for (name, sub_changes) in nested {
            let existing = entries
                .iter()
                .find(|entry| entry.path == name && entry.kind == "tree")
                .map(|entry| entry.sha.clone());
            entries.retain(|entry| entry.path != name);
            if let Some(sha) = self.apply(existing.as_deref(), sub_changes)? {
                entries.push(TreeEntry {
                    path: name,
                    mode: MODE_TREE.to_owned(),
                    kind: "tree".to_owned(),
                    sha,
                    size: None,
                });
            }
        }
        if entries.is_empty() {
            Ok(None)
        } else {
            Ok(Some(self.put_tree(entries)))
        }
    }

    fn apply_root(
        &mut self,
        base: Option<&str>,
        changes: Vec<(Vec<String>, Leaf)>,
    ) -> Result<String, (u16, String)> {
        Ok(match self.apply(base, changes)? {
            Some(sha) => sha,
            None => self.put_tree(Vec::new()),
        })
    }

    fn list_recursive(&self, sha: &str, prefix: &str, out: &mut Vec<TreeEntry>) {
        for entry in self.trees.get(sha).map_or(&[][..], Vec::as_slice) {
            let mut listed = entry.clone();
            listed.path = format!("{prefix}{}", entry.path);
            let is_tree = entry.kind == "tree";
            let child_prefix = format!("{}/", listed.path);
            out.push(listed);
            if is_tree {
                self.list_recursive(&entry.sha, &child_prefix, out);
            }
        }
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        let mut stack = vec![descendant.to_owned()];
        let mut seen = std::collections::HashSet::new();
        while let Some(sha) = stack.pop() {
            if sha == ancestor {
                return true;
            }
            if seen.insert(sha.clone())
                && let Some(commit) = self.commits.get(&sha)
            {
                stack.extend(commit.parents.iter().cloned());
            }
        }
        false
    }

    fn blob_at(&self, tree: &str, path: &str) -> Option<&Vec<u8>> {
        let mut current = tree.to_owned();
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            let entry = self.trees.get(&current)?.iter().find(|e| e.path == part)?;
            if parts.peek().is_none() {
                return self.blobs.get(&entry.sha);
            }
            current.clone_from(&entry.sha);
        }
        None
    }
}

impl FakeGitHub {
    /// An empty server whose only account is `login`.
    #[must_use]
    pub fn new(login: &str) -> Self {
        Self {
            state: RefCell::new(State {
                login: login.to_owned(),
                token_valid: true,
                ..State::default()
            }),
            log: RefCell::new(Vec::new()),
        }
    }

    /// A server with one small Python project, for the app's demo mode.
    #[must_use]
    pub fn demo() -> Self {
        let fake = Self::new("demo");
        fake.add_repo(
            "demo/hello-python",
            "main",
            &[
                ("README.md", "# hello-python\n\nA tiny project to try the editor on.\nNothing here is saved to GitHub.\n\nIn the terminal, try:\n\n    python -m app.main\n"),
                ("app/main.py", "from app.greet import greet\n\n\ndef main() -> None:\n    print(greet(\"world\"))\n\n\nif __name__ == \"__main__\":\n    main()\n"),
                ("app/greet.py", "def greet(name: str) -> str:\n    return f\"Hello, {name}\"\n"),
                ("app/__init__.py", ""),
                ("tests/test_greet.py", "from app.greet import greet\n\n\ndef test_greet() -> None:\n    assert greet(\"Ada\") == \"Hello, Ada\"\n"),
                ("pyproject.toml", "[project]\nname = \"hello-python\"\nversion = \"0.1.0\"\nrequires-python = \">=3.12\"\n"),
            ],
        );
        fake
    }

    /// Creates a repository with one commit containing `files`.
    ///
    /// # Panics
    /// Panics if a path is empty, which is a mistake in the calling test.
    pub fn add_repo(&self, full_name: &str, default_branch: &str, files: &[(&str, &str)]) {
        let mut state = self.state.borrow_mut();
        state.clock += 1;
        let clock = state.clock;
        let mut repo = FakeRepo {
            default_branch: default_branch.to_owned(),
            ..FakeRepo::default()
        };
        let changes = files
            .iter()
            .map(|(path, content)| {
                let sha = repo.put_blob(content.as_bytes());
                (
                    path.split('/').map(str::to_owned).collect(),
                    Leaf::Set {
                        mode: MODE_FILE.to_owned(),
                        sha,
                    },
                )
            })
            .collect();
        let tree = repo.apply_root(None, changes).expect("valid seed files");
        let commit = repo.put_commit(&tree, &[], "Initial commit", clock);
        repo.refs.insert(default_branch.to_owned(), commit);
        state.repos.insert(full_name.to_owned(), repo);
    }

    /// Commits straight to a branch, as if someone else pushed. `None`
    /// content deletes the file. Returns the new commit's sha.
    ///
    /// # Panics
    /// Panics if the repository or branch does not exist.
    pub fn push(
        &self,
        full_name: &str,
        branch: &str,
        message: &str,
        writes: &[(&str, Option<&str>)],
    ) -> String {
        let mut state = self.state.borrow_mut();
        state.clock += 1;
        let clock = state.clock;
        let repo = state.repos.get_mut(full_name).expect("repo exists");
        let head = repo.refs.get(branch).expect("branch exists").clone();
        let base = repo.commits[&head].tree.clone();
        let changes = writes
            .iter()
            .map(|(path, content)| {
                let leaf = match content {
                    Some(text) => Leaf::Set {
                        mode: MODE_FILE.to_owned(),
                        sha: repo.put_blob(text.as_bytes()),
                    },
                    None => Leaf::Delete,
                };
                (path.split('/').map(str::to_owned).collect(), leaf)
            })
            .collect();
        let tree = repo.apply_root(Some(&base), changes).expect("valid push");
        let commit = repo.put_commit(&tree, &[head], message, clock);
        repo.refs.insert(branch.to_owned(), commit.clone());
        commit
    }

    /// The text of a file on a branch, if it exists and is UTF-8.
    #[must_use]
    pub fn file(&self, full_name: &str, branch: &str, path: &str) -> Option<String> {
        let state = self.state.borrow();
        let repo = state.repos.get(full_name)?;
        let head = repo.refs.get(branch)?;
        let tree = &repo.commits.get(head)?.tree;
        String::from_utf8(repo.blob_at(tree, path)?.clone()).ok()
    }

    /// The commit a branch points at.
    #[must_use]
    pub fn head(&self, full_name: &str, branch: &str) -> Option<String> {
        let state = self.state.borrow();
        state.repos.get(full_name)?.refs.get(branch).cloned()
    }

    /// The message and parents of a commit.
    #[must_use]
    pub fn commit_info(&self, full_name: &str, sha: &str) -> Option<(String, Vec<String>)> {
        let state = self.state.borrow();
        let commit = state.repos.get(full_name)?.commits.get(sha)?;
        Some((commit.message.clone(), commit.parents.clone()))
    }

    /// Makes every request answer 401 (or work again).
    pub fn set_token_valid(&self, valid: bool) {
        self.state.borrow_mut().token_valid = valid;
    }

    /// Makes every request fail without an HTTP answer (or work again).
    pub fn set_offline(&self, offline: bool) {
        self.state.borrow_mut().offline = offline;
    }

    /// Makes the next request answer with this status and message.
    pub fn fail_next(&self, status: u16, message: &str) {
        self.state.borrow_mut().fail_next = Some((status, message.to_owned()));
    }

    /// Every request received so far, as `"METHOD /path"`.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    fn handle(&self, request: &Request) -> Reply {
        let mut state = self.state.borrow_mut();
        if let Some(failure) = state.fail_next.take() {
            return Err(failure);
        }
        if !state.token_valid {
            return Err((401, "Bad credentials".to_owned()));
        }
        let (path, query) = request
            .path
            .split_once('?')
            .unwrap_or((request.path.as_str(), ""));
        let segments: Vec<String> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(percent_decode)
            .collect();
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let body: Value = request
            .body
            .as_deref()
            .map_or(Ok(Value::Null), serde_json::from_str)
            .map_err(|error| (400, format!("Problems parsing JSON: {error}")))?;

        match (request.method, segments.as_slice()) {
            (Method::Get, ["user"]) => Ok(json!({"login": state.login})),
            (Method::Get, ["user", "repos"]) => Ok(Value::Array(
                state
                    .repos
                    .iter()
                    .map(|(name, repo)| repo_json(name, repo))
                    .collect(),
            )),
            (method, ["repos", owner, name, rest @ ..]) => {
                let full_name = format!("{owner}/{name}");
                state.clock += 1;
                let clock = state.clock;
                let repo = state.repos.get_mut(&full_name).ok_or_else(not_found)?;
                repo_route(method, &full_name, repo, rest, query, &body, clock)
            }
            _ => Err(not_found()),
        }
    }
}

fn repo_json(name: &str, repo: &FakeRepo) -> Value {
    json!({
        "full_name": name,
        "default_branch": repo.default_branch,
        "private": false,
        "description": "Demo repository. Nothing here is saved to GitHub.",
    })
}

fn str_field<'a>(body: &'a Value, key: &str) -> Result<&'a str, (u16, String)> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or((422, format!("Invalid request: {key} is required")))
}

// One arm per REST route; splitting it would only scatter the route table.
#[allow(clippy::too_many_lines)]
fn repo_route(
    method: Method,
    full_name: &str,
    repo: &mut FakeRepo,
    rest: &[&str],
    query: &str,
    body: &Value,
    clock: u64,
) -> Reply {
    match (method, rest) {
        (Method::Get, []) => Ok(repo_json(full_name, repo)),
        (Method::Get, ["branches"]) => Ok(Value::Array(
            repo.refs.keys().map(|name| json!({"name": name})).collect(),
        )),
        (Method::Get, ["git", "ref", "heads", branch @ ..]) => {
            let branch = branch.join("/");
            let sha = repo.refs.get(&branch).ok_or_else(not_found)?;
            Ok(
                json!({"ref": format!("refs/heads/{branch}"), "object": {"sha": sha, "type": "commit"}}),
            )
        }
        (Method::Get, ["git", "commits", sha]) => {
            let commit = repo.commits.get(*sha).ok_or_else(not_found)?;
            Ok(json!({
                "sha": sha,
                "tree": {"sha": commit.tree},
                "parents": commit.parents.iter().map(|p| json!({"sha": p})).collect::<Vec<_>>(),
                "message": commit.message,
            }))
        }
        (Method::Get, ["git", "trees", sha]) => {
            if !repo.trees.contains_key(*sha) {
                return Err(not_found());
            }
            let mut entries = Vec::new();
            if query.contains("recursive=1") {
                repo.list_recursive(sha, "", &mut entries);
            } else {
                entries.clone_from(&repo.trees[*sha]);
            }
            let tree: Vec<Value> = entries
                .iter()
                .map(|entry| {
                    json!({"path": entry.path, "mode": entry.mode, "type": entry.kind, "sha": entry.sha, "size": entry.size})
                })
                .collect();
            Ok(json!({"sha": sha, "tree": tree, "truncated": false}))
        }
        (Method::Get, ["git", "blobs", sha]) => {
            let data = repo.blobs.get(*sha).ok_or_else(not_found)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(data);
            // GitHub wraps base64 at 60 columns; do the same so decoding is exercised.
            let wrapped: Vec<&str> = encoded
                .as_bytes()
                .chunks(60)
                .filter_map(|chunk| std::str::from_utf8(chunk).ok())
                .collect();
            Ok(
                json!({"sha": sha, "size": data.len(), "encoding": "base64", "content": wrapped.join("\n") + "\n"}),
            )
        }
        (Method::Post, ["git", "trees"]) => {
            let base = body.get("base_tree").and_then(Value::as_str);
            let entries = body
                .get("tree")
                .and_then(Value::as_array)
                .ok_or((422, "Invalid request: tree is required".to_owned()))?;
            let mut changes = Vec::new();
            for entry in entries {
                let path = str_field(entry, "path")?;
                let parts: Vec<String> = path.split('/').map(str::to_owned).collect();
                if parts.iter().any(String::is_empty) {
                    return Err((422, format!("tree.path {path:?} is malformed")));
                }
                let leaf = if let Some(content) = entry.get("content").and_then(Value::as_str) {
                    Leaf::Set {
                        mode: str_field(entry, "mode")?.to_owned(),
                        sha: repo.put_blob(content.as_bytes()),
                    }
                } else if let Some(sha) = entry.get("sha").and_then(Value::as_str) {
                    Leaf::Set {
                        mode: str_field(entry, "mode")?.to_owned(),
                        sha: sha.to_owned(),
                    }
                } else {
                    Leaf::Delete
                };
                changes.push((parts, leaf));
            }
            let sha = repo.apply_root(base, changes)?;
            Ok(json!({"sha": sha}))
        }
        (Method::Post, ["git", "commits"]) => {
            let tree = str_field(body, "tree")?;
            let message = str_field(body, "message")?;
            if !repo.trees.contains_key(tree) {
                return Err((422, "Tree SHA does not exist".to_owned()));
            }
            let parents: Vec<String> = body
                .get("parents")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if parents.iter().any(|p| !repo.commits.contains_key(p)) {
                return Err((422, "Parent SHA does not exist".to_owned()));
            }
            let sha = repo.put_commit(tree, &parents, message, clock);
            Ok(json!({"sha": sha}))
        }
        (Method::Patch, ["git", "refs", "heads", branch @ ..]) => {
            let branch = branch.join("/");
            let sha = str_field(body, "sha")?;
            let current = repo
                .refs
                .get(&branch)
                .ok_or((422, "Reference does not exist".to_owned()))?
                .clone();
            if !repo.commits.contains_key(sha) {
                return Err((422, "Object does not exist".to_owned()));
            }
            let force = body.get("force").and_then(Value::as_bool).unwrap_or(false);
            if !force && !repo.is_ancestor(&current, sha) {
                return Err((422, "Update is not a fast forward".to_owned()));
            }
            repo.refs.insert(branch.clone(), sha.to_owned());
            Ok(json!({"ref": format!("refs/heads/{branch}"), "object": {"sha": sha}}))
        }
        (Method::Post, ["git", "refs"]) => {
            let name = str_field(body, "ref")?;
            let sha = str_field(body, "sha")?;
            let branch = name
                .strip_prefix("refs/heads/")
                .ok_or((422, "Reference name must start with refs/heads/".to_owned()))?;
            if repo.refs.contains_key(branch) {
                return Err((422, "Reference already exists".to_owned()));
            }
            if !repo.commits.contains_key(sha) {
                return Err((422, "Object does not exist".to_owned()));
            }
            repo.refs.insert(branch.to_owned(), sha.to_owned());
            Ok(json!({"ref": name, "object": {"sha": sha}}))
        }
        (Method::Get, ["commits"]) => {
            let param = |key: &str| {
                query
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| percent_decode(value))
            };
            let branch = param("sha").unwrap_or_else(|| repo.default_branch.clone());
            let limit = param("per_page")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(30);
            let mut sha = repo.refs.get(&branch).cloned().ok_or_else(not_found)?;
            let mut out = Vec::new();
            while out.len() < limit {
                let Some(commit) = repo.commits.get(&sha) else {
                    break;
                };
                out.push(json!({"sha": sha, "commit": {"message": commit.message, "author": {"name": "Demo", "date": commit.date}}}));
                match commit.parents.first() {
                    Some(parent) => sha.clone_from(parent),
                    None => break,
                }
            }
            Ok(Value::Array(out))
        }
        _ => Err(not_found()),
    }
}

impl Http for FakeGitHub {
    fn send(&self, request: Request) -> LocalBoxFuture<'_, Result<Response, String>> {
        Box::pin(async move {
            self.log
                .borrow_mut()
                .push(format!("{} {}", request.method.as_str(), request.path));
            if self.state.borrow().offline {
                return Err("offline".to_owned());
            }
            let (status, body) = match self.handle(&request) {
                Ok(value) => (200, value.to_string()),
                Err((status, message)) => (status, json!({"message": message}).to_string()),
            };
            Ok(Response {
                status,
                body,
                rate_limit_remaining: Some(4999),
                rate_limit_reset: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ids_match_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            object_id("blob", b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        // `git hash-object -t tree /dev/null`
        assert_eq!(
            object_id("tree", b""),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
    }

    #[test]
    fn tree_ids_match_git() {
        // A repository whose only file is `a.txt` containing "hello\n":
        // `git write-tree` prints this id.
        let mut repo = FakeRepo::default();
        let blob = repo.put_blob(b"hello\n");
        let tree = repo
            .apply_root(
                None,
                vec![(
                    vec!["a.txt".to_owned()],
                    Leaf::Set {
                        mode: MODE_FILE.to_owned(),
                        sha: blob,
                    },
                )],
            )
            .unwrap();
        assert_eq!(tree, "2e81171448eb9f2ee3821e3d447aa6b2fe3ddba1");
    }

    #[test]
    fn deleting_the_last_file_removes_the_directory() {
        let fake = FakeGitHub::new("t");
        fake.add_repo("t/r", "main", &[("a/b.txt", "x"), ("c.txt", "y")]);
        fake.push("t/r", "main", "rm", &[("a/b.txt", None)]);
        assert_eq!(fake.file("t/r", "main", "a/b.txt"), None);
        assert_eq!(fake.file("t/r", "main", "c.txt").as_deref(), Some("y"));
    }
}
