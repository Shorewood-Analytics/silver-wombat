//! One repository branch as the person is editing it.
//!
//! The workspace is the branch's head commit on GitHub (the *base*) plus an
//! *overlay* of local changes. Reads go through the overlay; nothing is
//! written to GitHub until [`Workspace::checkpoint`] or [`Workspace::commit`].
//!
//! Every local change gets a revision number. Two watermarks say how safe a
//! change is: saved on this device, and checkpointed to GitHub. A change is
//! only ever reported as saved after the write that makes it so has succeeded.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fake::object_id;
use crate::github::{GitHub, MODE_FILE, MODE_SUBMODULE, MODE_SYMLINK, TreeChange, TreeEntry};
use crate::{diff, path};

/// Files larger than this are not opened as text.
pub const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
/// Fetched file contents kept in memory before the cache is emptied.
const BLOB_CACHE_BYTES: usize = 16 * 1024 * 1024;
/// Where checkpoints go: `autosave/<branch>`.
pub const AUTOSAVE_PREFIX: &str = "autosave/";

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Submodule,
}

/// How a path differs from the base commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

/// How safe a local change is. Ordered from least to most safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SaveState {
    /// Only in this tab's memory.
    NotSaved,
    /// Written to this device's storage.
    OnDevice,
    /// In a checkpoint commit on GitHub.
    OnGitHub,
}

impl SaveState {
    /// Words for the status line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotSaved => "Not yet saved",
            Self::OnDevice => "Saved on this device",
            Self::OnGitHub => "Checkpointed to GitHub",
        }
    }
}

/// One row of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub change: Option<ChangeKind>,
}

/// One local change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub state: SaveState,
}

/// What a file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    Text(String),
    /// Not valid UTF-8, or over [`MAX_TEXT_BYTES`].
    Binary {
        size: u64,
    },
}

/// A file found by a recursive listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub path: String,
    pub size: Option<u64>,
}

/// What `commit` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    pub sha: String,
    pub files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Draft {
    /// New text for a path. `base_sha` is the blob it replaces, if any.
    Write {
        content: String,
        base_sha: Option<String>,
        mode: String,
    },
    /// The path now holds an existing blob (a moved or copied file).
    Link {
        sha: String,
        base_sha: Option<String>,
        mode: String,
    },
    /// The path is removed.
    Delete { base_sha: String },
}

impl Draft {
    fn base_sha(&self) -> Option<&str> {
        match self {
            Self::Write { base_sha, .. } | Self::Link { base_sha, .. } => base_sha.as_deref(),
            Self::Delete { base_sha } => Some(base_sha),
        }
    }

    fn kind(&self) -> ChangeKind {
        match self {
            Self::Delete { .. } => ChangeKind::Deleted,
            Self::Write { base_sha: None, .. } | Self::Link { base_sha: None, .. } => {
                ChangeKind::Added
            }
            Self::Write { .. } | Self::Link { .. } => ChangeKind::Modified,
        }
    }
}

#[derive(Debug, Clone)]
struct DraftEntry {
    draft: Draft,
    revision: u64,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    repo: String,
    branch: String,
    drafts: BTreeMap<String, Draft>,
}

struct State {
    branch: String,
    head: String,
    root_tree: String,
    trees: HashMap<String, Rc<Vec<TreeEntry>>>,
    blobs: HashMap<String, Rc<Vec<u8>>>,
    blob_bytes: usize,
    overlay: BTreeMap<String, DraftEntry>,
    revision: u64,
    device_revision: u64,
    checkpoint_revision: u64,
    checkpoint: Option<(String, String)>,
}

/// A repository branch plus local changes. See the module docs.
pub struct Workspace {
    gh: GitHub,
    repo: String,
    state: RefCell<State>,
}

fn kind_of(entry: &TreeEntry) -> EntryKind {
    match (entry.kind.as_str(), entry.mode.as_str()) {
        ("tree", _) => EntryKind::Dir,
        ("commit", _) | (_, MODE_SUBMODULE) => EntryKind::Submodule,
        (_, MODE_SYMLINK) => EntryKind::Symlink,
        _ => EntryKind::File,
    }
}

impl Workspace {
    /// Opens `branch` of `repo` (`owner/name`) at its current head.
    pub async fn open(gh: GitHub, repo: &str, branch: &str) -> Result<Self> {
        let head = gh
            .branch_head(repo, branch)
            .await?
            .ok_or_else(|| Error::NotFound(format!("branch {branch}")))?;
        let commit = gh.commit(repo, &head).await?;
        Ok(Self {
            gh,
            repo: repo.to_owned(),
            state: RefCell::new(State {
                branch: branch.to_owned(),
                head,
                root_tree: commit.tree,
                trees: HashMap::new(),
                blobs: HashMap::new(),
                blob_bytes: 0,
                overlay: BTreeMap::new(),
                revision: 0,
                device_revision: 0,
                checkpoint_revision: 0,
                checkpoint: None,
            }),
        })
    }

    /// `owner/name`.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The branch being edited.
    #[must_use]
    pub fn branch(&self) -> String {
        self.state.borrow().branch.clone()
    }

    /// The base commit.
    #[must_use]
    pub fn head(&self) -> String {
        self.state.borrow().head.clone()
    }

    /// The GitHub client this workspace uses.
    #[must_use]
    pub fn github(&self) -> &GitHub {
        &self.gh
    }

    /// The branch that receives checkpoints.
    #[must_use]
    pub fn autosave_branch(&self) -> String {
        format!("{AUTOSAVE_PREFIX}{}", self.state.borrow().branch)
    }

    async fn tree(&self, sha: &str) -> Result<Rc<Vec<TreeEntry>>> {
        if let Some(tree) = self.state.borrow().trees.get(sha) {
            return Ok(tree.clone());
        }
        let tree = Rc::new(self.gh.tree(&self.repo, sha, false).await?.tree);
        self.state
            .borrow_mut()
            .trees
            .insert(sha.to_owned(), tree.clone());
        Ok(tree)
    }

    async fn blob(&self, sha: &str) -> Result<Rc<Vec<u8>>> {
        if let Some(blob) = self.state.borrow().blobs.get(sha) {
            return Ok(blob.clone());
        }
        let blob = Rc::new(self.gh.blob(&self.repo, sha).await?);
        let mut state = self.state.borrow_mut();
        if state.blob_bytes + blob.len() > BLOB_CACHE_BYTES {
            state.blobs.clear();
            state.blob_bytes = 0;
        }
        state.blob_bytes += blob.len();
        state.blobs.insert(sha.to_owned(), blob.clone());
        Ok(blob)
    }

    /// The entry at `path` in the tree `root`, ignoring local changes.
    async fn entry_in(&self, root: &str, path: &str) -> Result<Option<TreeEntry>> {
        let mut current = root.to_owned();
        let mut parts = path.split('/').filter(|part| !part.is_empty()).peekable();
        while let Some(part) = parts.next() {
            let tree = self.tree(&current).await?;
            let Some(entry) = tree.iter().find(|entry| entry.path == part) else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(entry.clone()));
            }
            if entry.kind != "tree" {
                return Ok(None);
            }
            current.clone_from(&entry.sha);
        }
        Ok(None)
    }

    async fn base_entry(&self, path: &str) -> Result<Option<TreeEntry>> {
        let root = self.state.borrow().root_tree.clone();
        self.entry_in(&root, path).await
    }

    fn draft(&self, path: &str) -> Option<Draft> {
        self.state
            .borrow()
            .overlay
            .get(path)
            .map(|entry| entry.draft.clone())
    }

    fn has_drafts_within(&self, dir: &str) -> bool {
        self.state.borrow().overlay.iter().any(|(path, entry)| {
            path != dir
                && path::is_within(path, dir)
                && !matches!(entry.draft, Draft::Delete { .. })
        })
    }

    /// Whether every file that `dir` has on GitHub is deleted locally and
    /// nothing new was added, so the folder no longer exists here.
    async fn is_emptied(&self, dir: &str) -> Result<bool> {
        let has_deletes = self.state.borrow().overlay.iter().any(|(path, entry)| {
            path::is_within(path, dir) && matches!(entry.draft, Draft::Delete { .. })
        });
        Ok(has_deletes && self.files_under(dir).await?.is_empty())
    }

    fn set_draft(&self, path: &str, draft: Option<Draft>) {
        let mut state = self.state.borrow_mut();
        state.revision += 1;
        let revision = state.revision;
        match draft {
            Some(draft) => {
                state
                    .overlay
                    .insert(path.to_owned(), DraftEntry { draft, revision });
            }
            None => {
                state.overlay.remove(path);
            }
        }
    }

    /// What `path` is, with local changes applied. `None` if it does not exist.
    pub async fn kind(&self, path: &str) -> Result<Option<EntryKind>> {
        if path.is_empty() {
            return Ok(Some(EntryKind::Dir));
        }
        match self.draft(path) {
            Some(Draft::Delete { .. }) => return Ok(None),
            Some(_) => return Ok(Some(EntryKind::File)),
            None => {}
        }
        if self.has_drafts_within(path) {
            return Ok(Some(EntryKind::Dir));
        }
        let kind = self.base_entry(path).await?.as_ref().map(kind_of);
        if kind == Some(EntryKind::Dir) && self.is_emptied(path).await? {
            return Ok(None);
        }
        Ok(kind)
    }

    /// The contents of a directory, directories first, then by name.
    pub async fn list_dir(&self, dir: &str) -> Result<Vec<DirEntry>> {
        let mut rows: BTreeMap<String, DirEntry> = BTreeMap::new();
        let base = if dir.is_empty() {
            Some(self.state.borrow().root_tree.clone())
        } else {
            match self.base_entry(dir).await? {
                Some(entry) if entry.kind == "tree" => Some(entry.sha),
                Some(_) if !self.has_drafts_within(dir) => {
                    return Err(Error::Invalid(format!(
                        "{} is not a folder",
                        path::display(dir)
                    )));
                }
                _ => None,
            }
        };
        if let Some(sha) = &base {
            for entry in self.tree(sha).await?.iter() {
                rows.insert(
                    entry.path.clone(),
                    DirEntry {
                        name: entry.path.clone(),
                        kind: kind_of(entry),
                        size: entry.size,
                        change: None,
                    },
                );
            }
        }
        let mut found_overlay = false;
        for (full, entry) in &self.state.borrow().overlay {
            if full == dir || !path::is_within(full, dir) {
                continue;
            }
            let rest = if dir.is_empty() {
                full.as_str()
            } else {
                &full[dir.len() + 1..]
            };
            let deleted = matches!(entry.draft, Draft::Delete { .. });
            match rest.split_once('/') {
                Some((first, _)) if !deleted => {
                    found_overlay = true;
                    rows.entry(first.to_owned()).or_insert_with(|| DirEntry {
                        name: first.to_owned(),
                        kind: EntryKind::Dir,
                        size: None,
                        change: Some(ChangeKind::Added),
                    });
                }
                Some(_) => {}
                None if deleted => {
                    rows.remove(rest);
                }
                None => {
                    found_overlay = true;
                    let size = match &entry.draft {
                        Draft::Write { content, .. } => Some(content.len() as u64),
                        _ => None,
                    };
                    rows.insert(
                        rest.to_owned(),
                        DirEntry {
                            name: rest.to_owned(),
                            kind: EntryKind::File,
                            size,
                            change: Some(entry.draft.kind()),
                        },
                    );
                }
            }
        }
        if base.is_none() && !found_overlay {
            return Err(Error::NotFound(path::display(dir)));
        }
        // A folder whose files were all removed or moved away is gone too.
        let folders: Vec<String> = rows
            .values()
            .filter(|row| row.kind == EntryKind::Dir && row.change.is_none())
            .map(|row| row.name.clone())
            .collect();
        for name in folders {
            if self.is_emptied(&path::join(dir, &name)).await? {
                rows.remove(&name);
            }
        }
        let mut list: Vec<DirEntry> = rows.into_values().collect();
        list.sort_by(|a, b| {
            (a.kind != EntryKind::Dir, &a.name).cmp(&(b.kind != EntryKind::Dir, &b.name))
        });
        Ok(list)
    }

    /// Reads a file, with local changes applied.
    pub async fn read(&self, file: &str) -> Result<FileContent> {
        let (sha, size) = match self.draft(file) {
            Some(Draft::Write { content, .. }) => return Ok(FileContent::Text(content)),
            Some(Draft::Delete { .. }) => return Err(Error::NotFound(path::display(file))),
            Some(Draft::Link { sha, .. }) => (sha, None),
            None => {
                let entry = self
                    .base_entry(file)
                    .await?
                    .ok_or_else(|| Error::NotFound(path::display(file)))?;
                match kind_of(&entry) {
                    EntryKind::File | EntryKind::Symlink => (entry.sha, entry.size),
                    EntryKind::Dir => {
                        return Err(Error::Invalid(format!(
                            "{} is a folder",
                            path::display(file)
                        )));
                    }
                    EntryKind::Submodule => {
                        return Err(Error::Invalid(format!(
                            "{} is a submodule, not a file",
                            path::display(file)
                        )));
                    }
                }
            }
        };
        if let Some(size) = size
            && size > MAX_TEXT_BYTES
        {
            return Ok(FileContent::Binary { size });
        }
        let bytes = self.blob(&sha).await?;
        Ok(match std::str::from_utf8(&bytes) {
            Ok(text) => FileContent::Text(text.to_owned()),
            Err(_) => FileContent::Binary {
                size: bytes.len() as u64,
            },
        })
    }

    /// Reads a file that must be text.
    /// A file's bytes, whatever they are. [`Self::read`] is the way to get
    /// text; this is for showing a file that is not text, such as a picture.
    pub async fn read_bytes(&self, file: &str) -> Result<Rc<Vec<u8>>> {
        let sha = match self.draft(file) {
            Some(Draft::Write { content, .. }) => return Ok(Rc::new(content.into_bytes())),
            Some(Draft::Delete { .. }) => return Err(Error::NotFound(path::display(file))),
            Some(Draft::Link { sha, .. }) => sha,
            None => {
                let entry = self
                    .base_entry(file)
                    .await?
                    .ok_or_else(|| Error::NotFound(path::display(file)))?;
                match kind_of(&entry) {
                    EntryKind::File | EntryKind::Symlink => entry.sha,
                    EntryKind::Dir | EntryKind::Submodule => {
                        return Err(Error::Invalid(format!(
                            "{} is not a file",
                            path::display(file)
                        )));
                    }
                }
            }
        };
        self.blob(&sha).await
    }

    pub async fn read_text(&self, file: &str) -> Result<String> {
        match self.read(file).await? {
            FileContent::Text(text) => Ok(text),
            FileContent::Binary { size } => Err(Error::Invalid(format!(
                "{} is not text ({size} bytes)",
                path::display(file)
            ))),
        }
    }

    /// Sets the text of a file locally. Writing back the base text removes
    /// the change. Returns whether the file now differs from the base.
    pub async fn write(&self, file: &str, content: &str) -> Result<bool> {
        if file.is_empty()
            || file
                .split('/')
                .any(|part| part.is_empty() || part == ".git")
        {
            return Err(Error::Invalid(format!("{file:?} is not a valid file path")));
        }
        let (base_sha, mode) = match self.draft(file) {
            Some(Draft::Write { base_sha, mode, .. } | Draft::Link { base_sha, mode, .. }) => {
                (base_sha, mode)
            }
            Some(Draft::Delete { base_sha }) => {
                let mode = self
                    .base_entry(file)
                    .await?
                    .map_or_else(|| MODE_FILE.to_owned(), |entry| entry.mode);
                (Some(base_sha), mode)
            }
            None => {
                if self.has_drafts_within(file) {
                    return Err(Error::Invalid(format!(
                        "{} is a folder",
                        path::display(file)
                    )));
                }
                match self.base_entry(file).await? {
                    Some(entry) if kind_of(&entry) == EntryKind::File => {
                        (Some(entry.sha), entry.mode)
                    }
                    Some(_) => {
                        return Err(Error::Invalid(format!(
                            "{} is not a regular file",
                            path::display(file)
                        )));
                    }
                    None => (None, MODE_FILE.to_owned()),
                }
            }
        };
        // A file cannot live under a path that is itself a file.
        let mut ancestor = path::parent(file);
        while !ancestor.is_empty() {
            if matches!(self.kind(ancestor).await?, Some(kind) if kind != EntryKind::Dir) {
                return Err(Error::Invalid(format!(
                    "{} is a file, so nothing can be created inside it",
                    path::display(ancestor)
                )));
            }
            ancestor = path::parent(ancestor);
        }
        if base_sha.as_deref() == Some(object_id("blob", content.as_bytes()).as_str()) {
            self.set_draft(file, None);
            return Ok(false);
        }
        self.set_draft(
            file,
            Some(Draft::Write {
                content: content.to_owned(),
                base_sha,
                mode,
            }),
        );
        Ok(true)
    }

    /// Every file under `dir`, with local changes applied.
    pub async fn files_under(&self, dir: &str) -> Result<Vec<FileRef>> {
        let mut files: BTreeMap<String, Option<u64>> = BTreeMap::new();
        let base = if dir.is_empty() {
            Some(self.state.borrow().root_tree.clone())
        } else {
            self.base_entry(dir)
                .await?
                .filter(|entry| entry.kind == "tree")
                .map(|entry| entry.sha)
        };
        if let Some(sha) = base {
            let tree = self.gh.tree(&self.repo, &sha, true).await?;
            if tree.truncated {
                return Err(Error::Invalid(format!(
                    "{} has too many files to list in one go",
                    path::display(dir)
                )));
            }
            for entry in tree.tree {
                if matches!(kind_of(&entry), EntryKind::File | EntryKind::Symlink) {
                    files.insert(path::join(dir, &entry.path), entry.size);
                }
            }
        }
        for (full, entry) in &self.state.borrow().overlay {
            if !path::is_within(full, dir) || full == dir {
                continue;
            }
            match &entry.draft {
                Draft::Delete { .. } => {
                    files.remove(full);
                }
                Draft::Write { content, .. } => {
                    files.insert(full.clone(), Some(content.len() as u64));
                }
                Draft::Link { .. } => {
                    files.insert(full.clone(), None);
                }
            }
        }
        Ok(files
            .into_iter()
            .map(|(path, size)| FileRef { path, size })
            .collect())
    }

    async fn remove_file(&self, file: &str) -> Result<()> {
        match self.draft(file) {
            Some(Draft::Delete { .. }) => Err(Error::NotFound(path::display(file))),
            Some(draft) => {
                let replacement = draft.base_sha().map(|sha| Draft::Delete {
                    base_sha: sha.to_owned(),
                });
                self.set_draft(file, replacement);
                Ok(())
            }
            None => {
                let entry = self
                    .base_entry(file)
                    .await?
                    .ok_or_else(|| Error::NotFound(path::display(file)))?;
                self.set_draft(
                    file,
                    Some(Draft::Delete {
                        base_sha: entry.sha,
                    }),
                );
                Ok(())
            }
        }
    }

    /// Removes a file, or a folder and everything in it when `recursive`.
    /// Returns how many files were removed.
    pub async fn remove(&self, target: &str, recursive: bool) -> Result<usize> {
        match self.kind(target).await? {
            None => Err(Error::NotFound(path::display(target))),
            Some(EntryKind::Dir) if !recursive => Err(Error::Invalid(format!(
                "{} is a folder (use rm -r)",
                path::display(target)
            ))),
            Some(EntryKind::Dir) => {
                let files = self.files_under(target).await?;
                for file in &files {
                    self.remove_file(&file.path).await?;
                }
                Ok(files.len())
            }
            Some(EntryKind::Submodule) => Err(Error::Invalid(format!(
                "{} is a submodule; removing submodules is not supported",
                path::display(target)
            ))),
            Some(_) => {
                self.remove_file(target).await?;
                Ok(1)
            }
        }
    }

    async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        if let Some(Draft::Write { content, .. }) = self.draft(from) {
            self.write(to, &content).await?;
            return Ok(());
        }
        let (sha, mode) = match self.draft(from) {
            Some(Draft::Link { sha, mode, .. }) => (sha, mode),
            Some(_) => return Err(Error::NotFound(path::display(from))),
            None => {
                let entry = self
                    .base_entry(from)
                    .await?
                    .ok_or_else(|| Error::NotFound(path::display(from)))?;
                (entry.sha, entry.mode)
            }
        };
        // Reuse `write`'s path checks with a throwaway value, then point the
        // draft at the existing blob so binary files move without re-upload.
        self.write(to, "\u{0}placeholder").await?;
        let base_sha = self.draft(to).and_then(|d| d.base_sha().map(str::to_owned));
        if base_sha.as_deref() == Some(sha.as_str()) {
            self.set_draft(to, None);
        } else {
            self.set_draft(
                to,
                Some(Draft::Link {
                    sha,
                    base_sha,
                    mode,
                }),
            );
        }
        Ok(())
    }

    /// Copies a file, or a folder when `recursive`. Returns how many files.
    pub async fn copy(&self, from: &str, to: &str, recursive: bool) -> Result<usize> {
        match self.kind(from).await? {
            None => Err(Error::NotFound(path::display(from))),
            Some(EntryKind::Dir) if !recursive => Err(Error::Invalid(format!(
                "{} is a folder (use cp -r)",
                path::display(from)
            ))),
            Some(EntryKind::Dir) => {
                if path::is_within(to, from) {
                    return Err(Error::Invalid(
                        "cannot copy a folder into itself".to_owned(),
                    ));
                }
                let files = self.files_under(from).await?;
                for file in &files {
                    let rest = &file.path[from.len()..];
                    self.copy_file(&file.path, &format!("{to}{rest}")).await?;
                }
                Ok(files.len())
            }
            Some(EntryKind::Submodule) => Err(Error::Invalid(format!(
                "{} is a submodule; copying submodules is not supported",
                path::display(from)
            ))),
            Some(_) => {
                self.copy_file(from, to).await?;
                Ok(1)
            }
        }
    }

    /// Moves a file or folder. Returns how many files moved.
    pub async fn rename(&self, from: &str, to: &str) -> Result<usize> {
        if from == to {
            return Ok(0);
        }
        let count = self.copy(from, to, true).await?;
        self.remove(from, true).await?;
        Ok(count)
    }

    /// Every local change, by path.
    #[must_use]
    pub fn changes(&self) -> Vec<Change> {
        let state = self.state.borrow();
        state
            .overlay
            .iter()
            .map(|(path, entry)| Change {
                path: path.clone(),
                kind: entry.draft.kind(),
                state: state.save_state(entry.revision),
            })
            .collect()
    }

    /// The least safe state among all local changes, or `None` if there are
    /// no local changes.
    #[must_use]
    pub fn save_state(&self) -> Option<SaveState> {
        self.changes().into_iter().map(|change| change.state).min()
    }

    /// The save state of one file, or `None` if it has no local change.
    #[must_use]
    pub fn file_state(&self, file: &str) -> Option<SaveState> {
        let state = self.state.borrow();
        state
            .overlay
            .get(file)
            .map(|entry| state.save_state(entry.revision))
    }

    /// A unified diff of one changed file against the base: what the change
    /// was started from.
    pub async fn diff(&self, file: &str) -> Result<String> {
        let Some(draft) = self.draft(file) else {
            return Ok(String::new());
        };
        let base = draft.base_sha().map(str::to_owned);
        self.diff_from(file, base, &draft).await
    }

    /// A unified diff of one changed file against what GitHub has for it now
    /// (as of the last [`Self::refresh`]). For a file in conflict this is the
    /// difference that matters: their version to the local one.
    pub async fn diff_against_github(&self, file: &str) -> Result<String> {
        let Some(draft) = self.draft(file) else {
            return Ok(String::new());
        };
        let root = self.state.borrow().root_tree.clone();
        let theirs = self.entry_in(&root, file).await?.map(|entry| entry.sha);
        self.diff_from(file, theirs, &draft).await
    }

    async fn diff_from(&self, file: &str, old: Option<String>, draft: &Draft) -> Result<String> {
        let old = match old {
            Some(sha) => match std::str::from_utf8(&self.blob(&sha).await?) {
                Ok(text) => Some(text.to_owned()),
                Err(_) => return Ok(format!("Binary file {file} changed\n")),
            },
            None => None,
        };
        let new = match draft {
            Draft::Write { content, .. } => Some(content.clone()),
            Draft::Delete { .. } => None,
            Draft::Link { sha, .. } => match std::str::from_utf8(&self.blob(sha).await?) {
                Ok(text) => Some(text.to_owned()),
                Err(_) => return Ok(format!("Binary file {file} changed\n")),
            },
        };
        Ok(diff::between(file, old.as_deref(), new.as_deref()))
    }

    /// Every local change as one patch file that `git apply` accepts: a way
    /// to carry unsaved work out of the browser.
    pub async fn patch(&self) -> Result<String> {
        let mut patch = String::new();
        for change in self.changes() {
            patch.push_str(&self.diff(&change.path).await?);
        }
        Ok(patch)
    }

    /// Drops the local change to one file. Returns whether there was one.
    pub fn discard(&self, file: &str) -> bool {
        let had = self.state.borrow().overlay.contains_key(file);
        if had {
            self.set_draft(file, None);
        }
        had
    }

    /// The local changes as JSON for the device store, with the revision the
    /// snapshot covers. After the store confirms the write, pass that
    /// revision to [`Self::mark_on_device`].
    #[must_use]
    pub fn export_drafts(&self) -> (u64, String) {
        let state = self.state.borrow();
        let saved = Saved {
            version: 1,
            repo: self.repo.clone(),
            branch: state.branch.clone(),
            drafts: state
                .overlay
                .iter()
                .map(|(path, entry)| (path.clone(), entry.draft.clone()))
                .collect(),
        };
        let json = serde_json::to_string(&saved).unwrap_or_else(|_| "{}".to_owned());
        (state.revision, json)
    }

    /// Records that everything up to `revision` is in the device store.
    pub fn mark_on_device(&self, revision: u64) {
        let mut state = self.state.borrow_mut();
        state.device_revision = state.device_revision.max(revision);
    }

    /// The newest revision number, for change detection.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.state.borrow().revision
    }

    /// Restores local changes saved by [`Self::export_drafts`]. Returns how
    /// many were restored. Data for another repository or branch is refused.
    pub fn import_drafts(&self, json: &str) -> Result<usize> {
        let saved: Saved =
            serde_json::from_str(json).map_err(|error| Error::Decode(error.to_string()))?;
        let mut state = self.state.borrow_mut();
        if saved.version != 1 || saved.repo != self.repo || saved.branch != state.branch {
            return Err(Error::Invalid(format!(
                "saved changes are for {}@{}, not {}@{}",
                saved.repo, saved.branch, self.repo, state.branch
            )));
        }
        let count = saved.drafts.len();
        for (path, draft) in saved.drafts {
            state.revision += 1;
            let revision = state.revision;
            state.overlay.insert(path, DraftEntry { draft, revision });
        }
        // They came from the device store, so they are on the device.
        state.device_revision = state.revision;
        Ok(count)
    }

    /// Paths whose base on GitHub no longer matches what the local change
    /// was made against. Checked against the tree `root`.
    async fn conflicts_in(&self, root: &str) -> Result<Vec<String>> {
        let drafts: Vec<(String, Option<String>)> = self
            .state
            .borrow()
            .overlay
            .iter()
            .map(|(path, entry)| (path.clone(), entry.draft.base_sha().map(str::to_owned)))
            .collect();
        let mut conflicts = Vec::new();
        for (path, base_sha) in drafts {
            let current = self.entry_in(root, &path).await?.map(|entry| entry.sha);
            if current != base_sha {
                conflicts.push(path);
            }
        }
        Ok(conflicts)
    }

    /// Fetches the branch's latest head. Local changes are kept. Returns
    /// the paths that now conflict with what is on GitHub.
    pub async fn refresh(&self) -> Result<Vec<String>> {
        let branch = self.branch();
        let latest = self
            .gh
            .branch_head(&self.repo, &branch)
            .await?
            .ok_or_else(|| Error::NotFound(format!("branch {branch}")))?;
        if latest != self.head() {
            let commit = self.gh.commit(&self.repo, &latest).await?;
            let mut state = self.state.borrow_mut();
            state.head = latest;
            state.root_tree = commit.tree;
        }
        let root = self.state.borrow().root_tree.clone();
        self.conflicts_in(&root).await
    }

    /// The local changes that conflict with the branch as last fetched: files
    /// that have also changed on GitHub since the local change was started.
    /// Does not contact GitHub for the branch; call [`Self::refresh`] first
    /// to compare with its latest state.
    pub async fn conflicts(&self) -> Result<Vec<String>> {
        let root = self.state.borrow().root_tree.clone();
        self.conflicts_in(&root).await
    }

    /// Settles a conflict in favour of the local change: when committed, it
    /// will replace what GitHub now has for this file. The other way to
    /// settle one is [`Self::discard`], which takes GitHub's version.
    ///
    /// Until one of the two is chosen the conflict stays, however the file is
    /// edited: nothing of someone else's is replaced without that choice.
    /// Settles a conflict by combining the two sides: the changes made here
    /// and the changes on GitHub, both against the version they started
    /// from. Returns how many places changed on both sides in different
    /// ways; those are left in the file between marker lines (see
    /// [`diff::merge`]) for the person to settle before committing.
    pub async fn merge_theirs(&self, file: &str) -> Result<usize> {
        let not_text = || {
            Error::Invalid(format!(
                "{} can only be merged when both versions are text. Keep yours or use GitHub's.",
                path::display(file)
            ))
        };
        let Some(Draft::Write {
            content,
            base_sha,
            mode,
        }) = self.draft(file)
        else {
            return Err(not_text());
        };
        let root = self.state.borrow().root_tree.clone();
        let Some(theirs_sha) = self.entry_in(&root, file).await?.map(|entry| entry.sha) else {
            return Err(Error::Invalid(format!(
                "{} was deleted on GitHub, so there is nothing to merge with. Keep yours or use GitHub's.",
                path::display(file)
            )));
        };
        let text_of =
            |bytes: Rc<Vec<u8>>| String::from_utf8(bytes.to_vec()).map_err(|_| not_text());
        // A file new on both sides started from nothing.
        let base = match &base_sha {
            Some(sha) => text_of(self.blob(sha).await?)?,
            None => String::new(),
        };
        let theirs = text_of(self.blob(&theirs_sha).await?)?;
        let merged = diff::merge(&base, &content, &theirs).ok_or_else(|| {
            Error::Invalid(format!(
                "{} is too long to merge here. Keep yours or use GitHub's.",
                path::display(file)
            ))
        })?;
        self.set_draft(
            file,
            Some(Draft::Write {
                content: merged.text,
                base_sha: Some(theirs_sha),
                mode,
            }),
        );
        Ok(merged.conflicts)
    }

    /// Changed files that still hold a place a merge left for the person
    /// to settle.
    #[must_use]
    pub fn unsettled(&self) -> Vec<String> {
        let state = self.state.borrow();
        state
            .overlay
            .iter()
            .filter(|(_, entry)| {
                matches!(&entry.draft, Draft::Write { content, .. } if diff::has_marks(content))
            })
            .map(|(file, _)| file.clone())
            .collect()
    }

    pub async fn keep_mine(&self, file: &str) -> Result<()> {
        let draft = self
            .draft(file)
            .ok_or_else(|| Error::NotFound(path::display(file)))?;
        let root = self.state.borrow().root_tree.clone();
        let theirs = self.entry_in(&root, file).await?.map(|entry| entry.sha);
        let rebased = match (draft, theirs) {
            (Draft::Write { content, mode, .. }, base_sha) => Some(Draft::Write {
                content,
                base_sha,
                mode,
            }),
            (Draft::Link { sha, mode, .. }, base_sha) => Some(Draft::Link {
                sha,
                base_sha,
                mode,
            }),
            (Draft::Delete { .. }, Some(base_sha)) => Some(Draft::Delete { base_sha }),
            // It is already gone on GitHub: the deletion has nothing left to do.
            (Draft::Delete { .. }, None) => None,
        };
        self.set_draft(file, rebased);
        Ok(())
    }

    async fn tree_changes(
        &self,
        root: &str,
        drafts: &BTreeMap<String, DraftEntry>,
    ) -> Result<Vec<TreeChange>> {
        let mut changes = Vec::new();
        for (path, entry) in drafts {
            match &entry.draft {
                Draft::Write { content, mode, .. } => changes.push(TreeChange::Write {
                    path: path.clone(),
                    mode: mode.clone(),
                    content: content.clone(),
                }),
                Draft::Link { sha, mode, .. } => changes.push(TreeChange::Link {
                    path: path.clone(),
                    mode: mode.clone(),
                    sha: sha.clone(),
                }),
                // Deleting a path that is already gone is an API error.
                Draft::Delete { .. } => {
                    if self.entry_in(root, path).await?.is_some() {
                        changes.push(TreeChange::Delete { path: path.clone() });
                    }
                }
            }
        }
        Ok(changes)
    }

    /// Writes all local changes to GitHub as one commit on
    /// `autosave/<branch>`, leaving the branch itself alone. Returns the
    /// checkpoint commit, or `None` when there was nothing new to save.
    pub async fn checkpoint(&self) -> Result<Option<String>> {
        let (revision, drafts, root, head, already) = {
            let state = self.state.borrow();
            (
                state.revision,
                state.overlay.clone(),
                state.root_tree.clone(),
                state.head.clone(),
                state.checkpoint_revision,
            )
        };
        if drafts.is_empty() || already >= revision {
            return Ok(None);
        }
        let autosave = self.autosave_branch();
        let changes = self.tree_changes(&root, &drafts).await?;
        let tree = self.gh.create_tree(&self.repo, &root, &changes).await?;
        let known = self.state.borrow().checkpoint.clone();
        let tip = match known {
            Some(tip) => Some(tip),
            None => match self.gh.branch_head(&self.repo, &autosave).await? {
                Some(sha) => {
                    let commit = self.gh.commit(&self.repo, &sha).await?;
                    Some((sha, commit.tree))
                }
                None => None,
            },
        };
        let sha = match &tip {
            Some((sha, tip_tree)) if *tip_tree == tree => sha.clone(),
            _ => {
                let mut message = format!(
                    "Checkpoint: {} file{}\n",
                    drafts.len(),
                    if drafts.len() == 1 { "" } else { "s" }
                );
                for path in drafts.keys() {
                    message.push('\n');
                    message.push_str(path);
                }
                let parent = tip.as_ref().map_or(head, |(sha, _)| sha.clone());
                let commit = self
                    .gh
                    .create_commit(&self.repo, &message, &tree, &[parent])
                    .await?;
                if tip.is_some() {
                    self.gh
                        .advance_branch(&self.repo, &autosave, &commit)
                        .await?;
                } else {
                    self.gh
                        .create_branch(&self.repo, &autosave, &commit)
                        .await?;
                }
                commit
            }
        };
        let mut state = self.state.borrow_mut();
        state.checkpoint = Some((sha.clone(), tree));
        state.checkpoint_revision = state.checkpoint_revision.max(revision);
        Ok(Some(sha))
    }

    /// Commits all local changes to the branch as one commit.
    ///
    /// First fetches the branch's latest head. If any changed file differs on
    /// GitHub from what the local change was based on, nothing is written and
    /// [`Error::Conflict`] lists those files. The branch is only ever
    /// fast-forwarded.
    pub async fn commit(&self, message: &str) -> Result<CommitOutcome> {
        let message = message.trim();
        if message.is_empty() {
            return Err(Error::Invalid("Write a commit message first.".to_owned()));
        }
        let (revision, drafts) = {
            let state = self.state.borrow();
            (state.revision, state.overlay.clone())
        };
        if drafts.is_empty() {
            return Err(Error::Invalid("Nothing to commit.".to_owned()));
        }
        let unsettled = self.unsettled();
        if !unsettled.is_empty() {
            let files: Vec<String> = unsettled.iter().map(|file| path::display(file)).collect();
            return Err(Error::Invalid(format!(
                "Not committed: {} still has places marked {} where both sides changed the same lines. Edit them to what they should be, remove the marker lines, then commit.",
                files.join(", "),
                diff::MARK_START
            )));
        }
        let branch = self.branch();
        let latest = self
            .gh
            .branch_head(&self.repo, &branch)
            .await?
            .ok_or_else(|| Error::NotFound(format!("branch {branch}")))?;
        let latest_tree = if latest == self.head() {
            self.state.borrow().root_tree.clone()
        } else {
            self.gh.commit(&self.repo, &latest).await?.tree
        };
        let conflicts = self.conflicts_in(&latest_tree).await?;
        if !conflicts.is_empty() {
            return Err(Error::Conflict(conflicts));
        }
        let changes = self.tree_changes(&latest_tree, &drafts).await?;
        let tree = self
            .gh
            .create_tree(&self.repo, &latest_tree, &changes)
            .await?;
        if tree == latest_tree {
            return Err(Error::Invalid(
                "Nothing to commit: the files already match GitHub.".to_owned(),
            ));
        }
        let sha = self
            .gh
            .create_commit(&self.repo, message, &tree, &[latest])
            .await?;
        self.gh.advance_branch(&self.repo, &branch, &sha).await?;

        let mut state = self.state.borrow_mut();
        state.head.clone_from(&sha);
        state.root_tree = tree;
        let committed: BTreeSet<&String> = drafts.keys().collect();
        let mut kept = BTreeMap::new();
        for (path, mut entry) in std::mem::take(&mut state.overlay) {
            if !committed.contains(&path) {
                kept.insert(path, entry);
            } else if entry.revision > revision {
                // Edited again while the commit was in flight: keep the edit,
                // now based on what was just committed.
                let new_base = match &drafts[&path].draft {
                    Draft::Write { content, .. } => Some(object_id("blob", content.as_bytes())),
                    Draft::Link { sha, .. } => Some(sha.clone()),
                    Draft::Delete { .. } => None,
                };
                entry.draft = match (entry.draft, new_base) {
                    (Draft::Write { content, mode, .. }, base_sha) => Draft::Write {
                        content,
                        base_sha,
                        mode,
                    },
                    (Draft::Link { sha, mode, .. }, base_sha) => Draft::Link {
                        sha,
                        base_sha,
                        mode,
                    },
                    (Draft::Delete { .. }, Some(base_sha)) => Draft::Delete { base_sha },
                    (Draft::Delete { .. }, None) => continue,
                };
                kept.insert(path, entry);
            }
        }
        state.overlay = kept;
        state.revision += 1;
        Ok(CommitOutcome {
            sha,
            files: drafts.len(),
        })
    }

    /// Switches to another branch, or creates `name` at the current base
    /// when `create`. Switching with local changes is refused, so nothing is
    /// silently carried onto a different base.
    pub async fn switch_branch(&self, name: &str, create: bool) -> Result<()> {
        let name = name.trim();
        if name.is_empty() || name.contains(' ') || name.starts_with('-') {
            return Err(Error::Invalid(format!(
                "{name:?} is not a valid branch name"
            )));
        }
        if create {
            self.gh
                .create_branch(&self.repo, name, &self.head())
                .await?;
            let mut state = self.state.borrow_mut();
            name.clone_into(&mut state.branch);
            state.checkpoint = None;
            state.checkpoint_revision = 0;
            return Ok(());
        }
        if name == self.branch() {
            return Ok(());
        }
        if !self.state.borrow().overlay.is_empty() {
            return Err(Error::Invalid(
                "Commit or discard your changes before switching branches.".to_owned(),
            ));
        }
        let head = self
            .gh
            .branch_head(&self.repo, name)
            .await?
            .ok_or_else(|| Error::NotFound(format!("branch {name}")))?;
        let commit = self.gh.commit(&self.repo, &head).await?;
        let mut state = self.state.borrow_mut();
        name.clone_into(&mut state.branch);
        state.head = head;
        state.root_tree = commit.tree;
        state.checkpoint = None;
        state.checkpoint_revision = 0;
        Ok(())
    }
}

impl State {
    fn save_state(&self, revision: u64) -> SaveState {
        if revision <= self.checkpoint_revision {
            SaveState::OnGitHub
        } else if revision <= self.device_revision {
            SaveState::OnDevice
        } else {
            SaveState::NotSaved
        }
    }
}
