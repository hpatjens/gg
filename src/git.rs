use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Ignored,
    TypeChange,
    Conflicted,
}

impl Stage {
    fn from_xy(c: char) -> Self {
        match c {
            '.' | ' ' => Stage::Unmodified,
            'M' => Stage::Modified,
            'A' => Stage::Added,
            'D' => Stage::Deleted,
            'R' => Stage::Renamed,
            'C' => Stage::Copied,
            'T' => Stage::TypeChange,
            'U' => Stage::Conflicted,
            '?' => Stage::Untracked,
            '!' => Stage::Ignored,
            _ => Stage::Unmodified,
        }
    }

    pub fn is_changed(self) -> bool {
        !matches!(self, Stage::Unmodified)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMode {
    Git,
    Lfs,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub index: Stage,
    pub worktree: Stage,
    pub lfs_tracked: bool,
    pub lfs_pointer_ok: Option<bool>,
    pub prev_storage: Option<StorageMode>,
    pub next_storage: Option<StorageMode>,
}

impl FileEntry {
    pub fn has_staged(&self) -> bool {
        self.index.is_changed() && self.index != Stage::Untracked
    }

    /// Belongs in the unstaged tree: untracked, conflicted, or has working-tree changes.
    pub fn in_unstaged(&self) -> bool {
        self.index == Stage::Untracked
            || self.index == Stage::Conflicted
            || (self.worktree.is_changed() && self.worktree != Stage::Untracked)
    }

    /// Belongs in the staged tree: has index changes (but is not merely untracked/conflicted).
    pub fn in_staged(&self) -> bool {
        self.index.is_changed()
            && self.index != Stage::Untracked
            && self.index != Stage::Conflicted
    }
}

fn run(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("spawning git {:?}", args))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        bail!("git {:?} failed: {}", args, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_raw(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("spawning git {:?}", args))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        bail!("git {:?} failed: {}", args, stderr.trim());
    }
    Ok(out.stdout)
}

fn run_try(args: &[&str]) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("spawning git {:?}", args))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

pub fn repo_root() -> Result<PathBuf> {
    let s = run(&["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(s.trim()))
}

pub fn current_branch() -> Result<String> {
    let s = run(&["symbolic-ref", "--short", "HEAD"])?;
    Ok(s.trim().to_string())
}

pub fn upstream() -> Result<Option<String>> {
    match run_try(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])? {
        Some(s) => Ok(Some(s.trim().to_string())),
        None => Ok(None),
    }
}

pub fn remotes() -> Result<Vec<String>> {
    let s = run(&["remote"])?;
    Ok(s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

pub fn status() -> Result<Vec<FileEntry>> {
    let out = run_raw(&[
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
    ])?;
    parse_porcelain_v2(&out)
}

fn parse_porcelain_v2(data: &[u8]) -> Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let end = data[i..].iter().position(|&b| b == 0).map(|p| i + p).unwrap_or(data.len());
        let record = &data[i..end];
        if record.is_empty() {
            i = end + 1;
            continue;
        }
        let kind = record[0];
        let rec_str = std::str::from_utf8(record).context("non-utf8 status record")?;
        match kind {
            b'1' => {
                let parts: Vec<&str> = rec_str.splitn(9, ' ').collect();
                if parts.len() < 9 {
                    bail!("malformed v2 record: {}", rec_str);
                }
                let xy = parts[1];
                let mut chars = xy.chars();
                let x = chars.next().unwrap_or('.');
                let y = chars.next().unwrap_or('.');
                let path = parts[8].to_string();
                entries.push(FileEntry {
                    path,
                    index: Stage::from_xy(x),
                    worktree: Stage::from_xy(y),
                    lfs_tracked: false,
                    lfs_pointer_ok: None,
                    prev_storage: None,
                    next_storage: None,
                });
                i = end + 1;
            }
            b'2' => {
                let parts: Vec<&str> = rec_str.splitn(10, ' ').collect();
                if parts.len() < 10 {
                    bail!("malformed v2 rename record: {}", rec_str);
                }
                let xy = parts[1];
                let mut chars = xy.chars();
                let x = chars.next().unwrap_or('.');
                let y = chars.next().unwrap_or('.');
                let path = parts[9].to_string();
                let orig_end = data[end + 1..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| end + 1 + p)
                    .unwrap_or(data.len());
                entries.push(FileEntry {
                    path,
                    index: Stage::from_xy(x),
                    worktree: Stage::from_xy(y),
                    lfs_tracked: false,
                    lfs_pointer_ok: None,
                    prev_storage: None,
                    next_storage: None,
                });
                i = orig_end + 1;
            }
            b'?' => {
                let path = rec_str[2..].to_string();
                entries.push(FileEntry {
                    path,
                    index: Stage::Untracked,
                    worktree: Stage::Untracked,
                    lfs_tracked: false,
                    lfs_pointer_ok: None,
                    prev_storage: None,
                    next_storage: None,
                });
                i = end + 1;
            }
            b'!' => {
                i = end + 1;
            }
            b'u' => {
                let parts: Vec<&str> = rec_str.splitn(11, ' ').collect();
                if parts.len() < 11 {
                    bail!("malformed v2 unmerged record: {}", rec_str);
                }
                let path = parts[10].to_string();
                entries.push(FileEntry {
                    path,
                    index: Stage::Conflicted,
                    worktree: Stage::Conflicted,
                    lfs_tracked: false,
                    lfs_pointer_ok: None,
                    prev_storage: None,
                    next_storage: None,
                });
                i = end + 1;
            }
            b'#' => {
                i = end + 1;
            }
            _ => {
                i = end + 1;
            }
        }
    }
    Ok(entries)
}

pub fn attr_filter(paths: &[&str]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    if paths.is_empty() {
        return Ok(map);
    }
    let mut child = Command::new("git")
        .args(["check-attr", "-z", "--stdin", "filter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning git check-attr")?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| anyhow!("no stdin"))?;
        for p in paths {
            stdin.write_all(p.as_bytes())?;
            stdin.write_all(&[0])?;
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "git check-attr failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<&str> = s.split('\0').collect();
    let mut idx = 0;
    while idx + 2 < parts.len() {
        let path = parts[idx];
        let _attr = parts[idx + 1];
        let value = parts[idx + 2];
        if !path.is_empty() {
            map.insert(path.to_string(), value.to_string());
        }
        idx += 3;
    }
    Ok(map)
}

fn blob_is_lfs_pointer(spec: &str) -> Option<bool> {
    let out = Command::new("git").args(["show", spec]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let head = &out.stdout[..out.stdout.len().min(200)];
    let s = String::from_utf8_lossy(head);
    Some(s.starts_with("version https://git-lfs.github.com/spec/"))
}

fn storage_of(spec: &str) -> Option<StorageMode> {
    blob_is_lfs_pointer(spec).map(|p| if p { StorageMode::Lfs } else { StorageMode::Git })
}

pub fn annotate_lfs(entries: &mut [FileEntry]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    let attrs = attr_filter(&paths)?;
    for e in entries.iter_mut() {
        let filter = attrs.get(&e.path).cloned().unwrap_or_default();
        e.lfs_tracked = filter == "lfs";

        if e.index != Stage::Untracked {
            e.prev_storage = storage_of(&format!("HEAD:{}", e.path));
        }

        if e.has_staged() && e.index != Stage::Deleted {
            e.next_storage = storage_of(&format!(":{}", e.path));
            if e.lfs_tracked {
                e.lfs_pointer_ok = Some(e.next_storage == Some(StorageMode::Lfs));
            }
        }
    }
    Ok(())
}

pub fn diff_worktree(path: &str) -> Result<String> {
    let s = run(&["diff", "--", path])?;
    Ok(s)
}

pub fn diff_cached(path: &str) -> Result<String> {
    let s = run(&["diff", "--cached", "--", path])?;
    Ok(s)
}

pub fn diff_untracked(path: &str) -> Result<String> {
    let full = run(&["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(full.trim());
    let p = root.join(path);
    match std::fs::read_to_string(&p) {
        Ok(content) => {
            let mut out = String::new();
            out.push_str(&format!("--- /dev/null\n+++ b/{}\n", path));
            for line in content.lines() {
                out.push('+');
                out.push_str(line);
                out.push('\n');
            }
            Ok(out)
        }
        Err(_) => Ok(format!("(binary or unreadable: {})", path)),
    }
}

pub fn stage(path: &str) -> Result<()> {
    run(&["add", "-A", "--", path])?;
    Ok(())
}

pub fn stage_all() -> Result<()> {
    run(&["add", "-A"])?;
    Ok(())
}

pub fn unstage(path: &str) -> Result<()> {
    let head = run_try(&["rev-parse", "--verify", "HEAD"])?;
    if head.is_some() {
        run(&["restore", "--staged", "--", path])?;
    } else {
        run(&["rm", "--cached", "--", path])?;
    }
    Ok(())
}

pub fn restore_worktree(path: &str) -> Result<()> {
    run(&["restore", "--", path])?;
    Ok(())
}

pub fn delete_untracked(path: &str) -> Result<()> {
    let root = repo_root()?;
    let full = root.join(path);
    std::fs::remove_file(&full).with_context(|| format!("removing {}", path))?;
    Ok(())
}

pub fn commit(subject: &str, body: &str, amend: bool) -> Result<String> {
    let mut args: Vec<String> = vec!["commit".into()];
    if amend {
        args.push("--amend".into());
    }
    args.push("-m".into());
    args.push(subject.to_string());
    if !body.trim().is_empty() {
        args.push("-m".into());
        args.push(body.to_string());
    }
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let _ = run(&str_args)?;
    let sha = run(&["rev-parse", "--short", "HEAD"])?;
    Ok(sha.trim().to_string())
}

pub fn last_commit_subject() -> Result<String> {
    let s = run(&["log", "-1", "--pretty=%s"])?;
    Ok(s.trim().to_string())
}

#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

pub fn log(limit: usize) -> Result<Vec<Commit>> {
    let arg = format!("-n{}", limit);
    let out = run_try(&[
        "log",
        &arg,
        "--date=short",
        "--pretty=format:%H%x1f%h%x1f%an%x1f%ad%x1f%s",
    ])?;
    let s = out.unwrap_or_default();
    let mut commits = Vec::new();
    for line in s.lines() {
        let parts: Vec<&str> = line.splitn(5, '\x1f').collect();
        if parts.len() < 5 {
            continue;
        }
        commits.push(Commit {
            sha: parts[0].to_string(),
            short: parts[1].to_string(),
            author: parts[2].to_string(),
            date: parts[3].to_string(),
            subject: parts[4].to_string(),
        });
    }
    Ok(commits)
}

pub fn show_commit(sha: &str) -> Result<String> {
    let s = run(&["show", "--color=never", "--stat", "-p", sha])?;
    Ok(s)
}

#[derive(Debug, Clone)]
pub struct Stash {
    pub reference: String,
    pub subject: String,
}

pub fn stash_list() -> Result<Vec<Stash>> {
    let out = run_try(&["stash", "list", "--format=%gd%x1f%s"])?;
    let s = out.unwrap_or_default();
    let mut stashes = Vec::new();
    for line in s.lines() {
        let parts: Vec<&str> = line.splitn(2, '\x1f').collect();
        if parts.len() < 2 {
            continue;
        }
        stashes.push(Stash {
            reference: parts[0].to_string(),
            subject: parts[1].to_string(),
        });
    }
    Ok(stashes)
}

pub fn stash_show(reference: &str) -> Result<String> {
    let s = run(&["stash", "show", "--color=never", "-p", reference])?;
    Ok(s)
}

pub fn stash_apply(reference: &str) -> Result<()> {
    run(&["stash", "apply", reference])?;
    Ok(())
}

pub fn stash_pop(reference: &str) -> Result<()> {
    run(&["stash", "pop", reference])?;
    Ok(())
}

pub fn stash_drop(reference: &str) -> Result<()> {
    run(&["stash", "drop", reference])?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LocalBranch {
    pub name: String,
    pub is_head: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub gone: bool,
    pub short: String,
    pub subject: String,
}

#[derive(Debug, Clone)]
pub struct RemoteBranch {
    pub remote: String,
    pub name: String,
    pub short: String,
    pub subject: String,
}

pub fn local_branches() -> Result<Vec<LocalBranch>> {
    let fmt = "%(HEAD)\x1f%(refname:short)\x1f%(upstream:short)\x1f%(upstream:track)\x1f%(objectname:short)\x1f%(contents:subject)";
    let arg = format!("--format={}", fmt);
    let s = run(&["for-each-ref", "--sort=-committerdate", "refs/heads", &arg])?;
    let mut out = Vec::new();
    for line in s.lines() {
        let parts: Vec<&str> = line.splitn(6, '\x1f').collect();
        if parts.len() < 6 {
            continue;
        }
        let (ahead, behind, gone) = parse_track(parts[3]);
        out.push(LocalBranch {
            is_head: parts[0].trim() == "*",
            name: parts[1].to_string(),
            upstream: if parts[2].is_empty() { None } else { Some(parts[2].to_string()) },
            ahead,
            behind,
            gone,
            short: parts[4].to_string(),
            subject: parts[5].to_string(),
        });
    }
    Ok(out)
}

fn parse_track(s: &str) -> (usize, usize, bool) {
    let s = s.trim();
    if s.is_empty() {
        return (0, 0, false);
    }
    let inner = s.trim_start_matches('[').trim_end_matches(']');
    if inner.contains("gone") {
        return (0, 0, true);
    }
    let mut ahead = 0usize;
    let mut behind = 0usize;
    for part in inner.split(',') {
        let p = part.trim();
        if let Some(rest) = p.strip_prefix("ahead ") {
            ahead = rest.parse().unwrap_or(0);
        } else if let Some(rest) = p.strip_prefix("behind ") {
            behind = rest.parse().unwrap_or(0);
        }
    }
    (ahead, behind, false)
}

pub fn remote_branches() -> Result<Vec<RemoteBranch>> {
    let fmt = "%(refname:short)\x1f%(objectname:short)\x1f%(contents:subject)";
    let arg = format!("--format={}", fmt);
    let s = run(&["for-each-ref", "--sort=refname", "refs/remotes", &arg])?;
    let mut out = Vec::new();
    for line in s.lines() {
        let parts: Vec<&str> = line.splitn(3, '\x1f').collect();
        if parts.len() < 3 {
            continue;
        }
        let refname = parts[0];
        if refname.ends_with("/HEAD") {
            continue;
        }
        let (remote, name) = match refname.split_once('/') {
            Some((r, n)) => (r.to_string(), n.to_string()),
            None => continue,
        };
        out.push(RemoteBranch {
            remote,
            name,
            short: parts[1].to_string(),
            subject: parts[2].to_string(),
        });
    }
    Ok(out)
}

pub fn checkout_branch(name: &str) -> Result<()> {
    run(&["checkout", name])?;
    Ok(())
}

pub fn branch_create(name: &str) -> Result<()> {
    run(&["branch", name])?;
    Ok(())
}

pub fn branch_delete(name: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    run(&["branch", flag, name])?;
    Ok(())
}

pub fn push(remote: &str, branch: &str, set_upstream: bool) -> Result<String> {
    let mut args: Vec<&str> = vec!["push"];
    if set_upstream {
        args.push("-u");
    }
    args.push(remote);
    args.push(branch);
    let out = Command::new("git")
        .args(&args)
        .output()
        .context("spawning git push")?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        bail!("push failed: {}", stderr.trim());
    }
    let combined = if stderr.is_empty() { stdout } else { stderr };
    Ok(combined.trim().to_string())
}
