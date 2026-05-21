use crate::git::{self, Commit, FileEntry, LocalBranch, RemoteBranch, Stash};
use crate::tree::{self, Row};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread;
use std::time::Instant;

#[derive(Debug)]
pub struct StatusSnapshot {
    pub entries: Vec<FileEntry>,
    pub branch: String,
    pub upstream: Option<String>,
}

#[derive(Debug, Default)]
pub struct OpDone {
    pub toast: Option<String>,
    pub status: Option<StatusSnapshot>,
    pub log: Option<Vec<Commit>>,
    pub stashes: Option<Vec<Stash>>,
    pub branches: Option<BranchesSnapshot>,
}

#[derive(Debug)]
pub struct BranchesSnapshot {
    pub locals: Vec<LocalBranch>,
    pub remotes: Vec<RemoteBranch>,
}

pub struct Pending {
    pub label: String,
    pub started: Instant,
    pub rx: Receiver<Result<OpDone, String>>,
}

fn gather_status() -> Result<StatusSnapshot> {
    let mut entries = git::status()?;
    let _ = git::annotate_lfs(&mut entries);
    let branch = git::current_branch().unwrap_or_else(|_| "(detached)".into());
    let upstream = git::upstream().unwrap_or(None);
    Ok(StatusSnapshot { entries, branch, upstream })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Status,
    Stash,
    Log,
    Branches,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchFocus {
    Local,
    Remote,
}

impl Default for BranchFocus {
    fn default() -> Self {
        BranchFocus::Local
    }
}

#[derive(Debug, Default)]
pub struct BranchesState {
    pub locals: Vec<LocalBranch>,
    pub remotes: Vec<RemoteBranch>,
    pub local_cursor: usize,
    pub remote_cursor: usize,
    pub focus: BranchFocus,
    pub loaded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffView {
    Hidden,
    Split,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFocus {
    List,
    Details,
}

#[derive(Debug, Default)]
pub struct LogState {
    pub commits: Vec<Commit>,
    pub cursor: usize,
    pub focus: LogFocus,
    pub details_text: String,
    pub details_scroll: u16,
    pub loaded: bool,
}

impl Default for LogFocus {
    fn default() -> Self {
        LogFocus::List
    }
}

const LOG_LIMIT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashFocus {
    List,
    Details,
}

impl Default for StashFocus {
    fn default() -> Self {
        StashFocus::List
    }
}

#[derive(Debug, Default)]
pub struct StashState {
    pub stashes: Vec<Stash>,
    pub cursor: usize,
    pub focus: StashFocus,
    pub details_text: String,
    pub details_scroll: u16,
    pub loaded: bool,
}

#[derive(Debug, Clone)]
pub struct CommitInput {
    pub subject: String,
    pub amend: bool,
}

impl CommitInput {
    pub fn new() -> Self {
        Self {
            subject: String::new(),
            amend: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PushDialog {
    Confirm { remote: String, branch: String },
    Pick { remotes: Vec<String>, cursor: usize, branch: String },
    Running,
}

#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub kind: ConfirmKind,
}

#[derive(Debug, Clone)]
pub enum ConfirmKind {
    Restore { path: String, is_dir: bool },
    DeleteUntracked { path: String },
    CommitDespiteLfs { pending: CommitInput },
    StashPop { reference: String },
    StashDrop { reference: String },
    BranchDelete { name: String, force: bool },
}

#[derive(Debug, Clone)]
pub struct NewBranchInput {
    pub name: String,
}

impl NewBranchInput {
    pub fn new() -> Self {
        Self { name: String::new() }
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    None,
    Commit(CommitInput),
    Push(PushDialog),
    Confirm(ConfirmDialog),
    NewBranch(NewBranchInput),
}

pub struct App {
    pub tab: Tab,
    pub entries: Vec<FileEntry>,
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub collapsed: HashSet<String>,
    pub focus: Focus,
    pub diff_view: DiffView,
    pub diff_text: String,
    pub diff_scroll: u16,
    pub log: LogState,
    pub stash: StashState,
    pub branches: BranchesState,
    pub modal: Modal,
    pub status_line: Option<(String, Instant)>,
    pub branch: String,
    pub upstream: Option<String>,
    pub pending: Option<Pending>,
    pub quit: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        let mut app = App {
            tab: Tab::Status,
            entries: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            collapsed: HashSet::new(),
            focus: Focus::Files,
            diff_view: DiffView::Hidden,
            diff_text: String::new(),
            diff_scroll: 0,
            log: LogState::default(),
            stash: StashState::default(),
            branches: BranchesState::default(),
            modal: Modal::None,
            status_line: None,
            branch: String::new(),
            upstream: None,
            pending: None,
            quit: false,
        };
        app.refresh()?;
        Ok(app)
    }

    pub fn start<F>(&mut self, label: impl Into<String>, f: F)
    where
        F: FnOnce() -> Result<OpDone> + Send + 'static,
    {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = channel();
        thread::spawn(move || {
            let r = f().map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.pending = Some(Pending {
            label: label.into(),
            started: Instant::now(),
            rx,
        });
    }

    pub fn poll(&mut self) {
        let Some(p) = self.pending.as_ref() else { return };
        match p.rx.try_recv() {
            Ok(Ok(done)) => {
                self.pending = None;
                self.apply_op(done);
            }
            Ok(Err(msg)) => {
                self.pending = None;
                self.toast(format!("error: {}", msg));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.toast("worker disconnected");
            }
        }
    }

    fn apply_op(&mut self, done: OpDone) {
        if done.status.is_some() {
            if matches!(self.modal, Modal::Commit(_)) {
                self.modal = Modal::None;
            }
        }
        if let Some(snap) = done.status {
            let prev = self.rows.get(self.cursor).map(|r| r.path.clone());
            self.entries = snap.entries;
            self.branch = snap.branch;
            self.upstream = snap.upstream;
            self.rebuild_rows();
            if let Some(p) = prev {
                if let Some(i) = self.rows.iter().position(|r| r.path == p) {
                    self.cursor = i;
                } else if self.cursor >= self.rows.len() {
                    self.cursor = self.rows.len().saturating_sub(1);
                }
            }
            self.refresh_diff();
            self.log.loaded = false;
            self.branches.loaded = false;
        }
        if let Some(commits) = done.log {
            self.log.commits = commits;
            self.log.loaded = true;
            if self.log.cursor >= self.log.commits.len() {
                self.log.cursor = self.log.commits.len().saturating_sub(1);
            }
            self.refresh_log_details();
        }
        if let Some(stashes) = done.stashes {
            self.stash.stashes = stashes;
            self.stash.loaded = true;
            if self.stash.cursor >= self.stash.stashes.len() {
                self.stash.cursor = self.stash.stashes.len().saturating_sub(1);
            }
            self.refresh_stash_details();
        }
        if let Some(snap) = done.branches {
            self.branches.locals = snap.locals;
            self.branches.remotes = snap.remotes;
            self.branches.loaded = true;
            if self.branches.local_cursor >= self.branches.locals.len() {
                self.branches.local_cursor = self.branches.locals.len().saturating_sub(1);
            }
            if self.branches.remote_cursor >= self.branches.remotes.len() {
                self.branches.remote_cursor = self.branches.remotes.len().saturating_sub(1);
            }
        }
        if let Some(t) = done.toast {
            self.toast(t);
        }
    }

    pub fn refresh(&mut self) -> Result<()> {
        let prev_path = self.rows.get(self.cursor).map(|r| r.path.clone());
        let mut entries = git::status()?;
        if let Err(e) = git::annotate_lfs(&mut entries) {
            self.toast(format!("LFS annotate failed: {}", e));
        }
        self.entries = entries;
        self.rebuild_rows();
        if let Some(p) = prev_path {
            if let Some(i) = self.rows.iter().position(|r| r.path == p) {
                self.cursor = i;
            } else {
                self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
            }
        } else {
            self.cursor = 0;
        }
        self.branch = git::current_branch().unwrap_or_else(|_| "(detached)".into());
        self.upstream = git::upstream().unwrap_or(None);
        self.refresh_diff();
        self.log.loaded = false;
        self.branches.loaded = false;
        Ok(())
    }

    fn rebuild_rows(&mut self) {
        self.rows = tree::build_rows(&self.entries, &self.collapsed);
    }

    pub fn refresh_diff(&mut self) {
        self.diff_scroll = 0;
        self.diff_text = match self.rows.get(self.cursor) {
            Some(r) if !r.is_dir => match r.entry_index.and_then(|i| self.entries.get(i)) {
                Some(e) => self.compute_diff(e),
                None => String::new(),
            },
            Some(r) => {
                let n = r.agg.staged + r.agg.unstaged + r.agg.untracked + r.agg.conflict;
                format!("(folder {} — {} changed file(s))", r.path, n)
            }
            None => String::new(),
        };
    }

    fn compute_diff(&self, e: &FileEntry) -> String {
        use crate::git::Stage;
        if e.index == Stage::Untracked {
            return git::diff_untracked(&e.path).unwrap_or_else(|err| format!("error: {}", err));
        }
        if e.worktree != Stage::Unmodified {
            match git::diff_worktree(&e.path) {
                Ok(s) if !s.trim().is_empty() => return s,
                _ => {}
            }
        }
        if e.index != Stage::Unmodified {
            return git::diff_cached(&e.path).unwrap_or_else(|err| format!("error: {}", err));
        }
        String::new()
    }

    pub fn toast<S: Into<String>>(&mut self, msg: S) {
        self.status_line = Some((msg.into(), Instant::now()));
    }

    pub fn staged_counts(&self) -> (usize, usize) {
        let mut git_n = 0;
        let mut lfs_n = 0;
        for e in &self.entries {
            let staged = matches!(e.index,
                crate::git::Stage::Added
                    | crate::git::Stage::Modified
                    | crate::git::Stage::Deleted
                    | crate::git::Stage::Renamed
                    | crate::git::Stage::Copied
                    | crate::git::Stage::TypeChange);
            if staged {
                if e.lfs_tracked { lfs_n += 1; } else { git_n += 1; }
            }
        }
        (git_n, lfs_n)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.pending.is_some() {
            if key.code == KeyCode::Char('q')
                && !matches!(self.modal, Modal::Commit(_) | Modal::NewBranch(_))
            {
                self.quit = true;
            }
            return Ok(());
        }
        match &self.modal {
            Modal::None => {
                match (key.code, self.tab) {
                    (KeyCode::Char('1'), _) => {
                        self.switch_tab(Tab::Status);
                        return Ok(());
                    }
                    (KeyCode::Char('2'), _) => {
                        self.switch_tab(Tab::Stash);
                        return Ok(());
                    }
                    (KeyCode::Char('3'), _) => {
                        self.switch_tab(Tab::Log);
                        return Ok(());
                    }
                    (KeyCode::Char('4'), _) => {
                        self.switch_tab(Tab::Branches);
                        return Ok(());
                    }
                    _ => {}
                }
                match self.tab {
                    Tab::Status => self.handle_main(key)?,
                    Tab::Stash => self.handle_stash(key)?,
                    Tab::Log => self.handle_log(key)?,
                    Tab::Branches => self.handle_branches(key)?,
                }
            }
            Modal::Commit(_) => self.handle_commit(key)?,
            Modal::Push(_) => self.handle_push_modal(key)?,
            Modal::Confirm(_) => self.handle_confirm(key)?,
            Modal::NewBranch(_) => self.handle_new_branch(key)?,
        }
        Ok(())
    }

    fn handle_main(&mut self, key: KeyEvent) -> Result<()> {
        match self.focus {
            Focus::Files => self.handle_files(key)?,
            Focus::Diff => self.handle_diff(key)?,
        }
        Ok(())
    }

    pub fn refresh_log(&mut self) {
        self.start("fetching log", || {
            let commits = git::log(LOG_LIMIT).unwrap_or_default();
            Ok(OpDone {
                log: Some(commits),
                ..Default::default()
            })
        });
    }

    fn switch_tab(&mut self, new_tab: Tab) {
        if self.tab == new_tab {
            return;
        }
        self.tab = new_tab;
        match new_tab {
            Tab::Status => {
                self.cursor = 0;
                self.diff_view = DiffView::Hidden;
                self.focus = Focus::Files;
                self.rebuild_rows();
                self.refresh_diff();
            }
            Tab::Stash => {
                if !self.stash.loaded {
                    self.refresh_stash();
                }
            }
            Tab::Log => {
                if !self.log.loaded {
                    self.refresh_log();
                }
            }
            Tab::Branches => {
                if !self.branches.loaded {
                    self.refresh_branches();
                }
            }
        }
    }

    pub fn refresh_branches(&mut self) {
        self.start("fetching branches", || {
            let locals = git::local_branches().unwrap_or_default();
            let remotes = git::remote_branches().unwrap_or_default();
            Ok(OpDone {
                branches: Some(BranchesSnapshot { locals, remotes }),
                ..Default::default()
            })
        });
    }

    fn handle_branches(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                self.branches.focus = match self.branches.focus {
                    BranchFocus::Local => BranchFocus::Remote,
                    BranchFocus::Remote => BranchFocus::Local,
                };
            }
            (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                self.branches.focus = BranchFocus::Local;
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                self.branches.focus = BranchFocus::Remote;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => self.branch_cursor_up(),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => self.branch_cursor_down(),
            (KeyCode::Home, _) => self.branch_cursor_home(),
            (KeyCode::End, _) => self.branch_cursor_end(),
            (KeyCode::Enter, _) => self.start_checkout(),
            (KeyCode::Char('d'), _) => self.start_branch_delete(false),
            (KeyCode::Char('D'), _) => self.start_branch_delete(true),
            (KeyCode::Char('n'), _) => {
                self.modal = Modal::NewBranch(NewBranchInput::new());
            }
            (KeyCode::Char('r'), _) => self.refresh_branches(),
            _ => {}
        }
        Ok(())
    }

    fn branch_cursor_up(&mut self) {
        match self.branches.focus {
            BranchFocus::Local => {
                if self.branches.local_cursor > 0 {
                    self.branches.local_cursor -= 1;
                }
            }
            BranchFocus::Remote => {
                if self.branches.remote_cursor > 0 {
                    self.branches.remote_cursor -= 1;
                }
            }
        }
    }

    fn branch_cursor_down(&mut self) {
        match self.branches.focus {
            BranchFocus::Local => {
                if self.branches.local_cursor + 1 < self.branches.locals.len() {
                    self.branches.local_cursor += 1;
                }
            }
            BranchFocus::Remote => {
                if self.branches.remote_cursor + 1 < self.branches.remotes.len() {
                    self.branches.remote_cursor += 1;
                }
            }
        }
    }

    fn branch_cursor_home(&mut self) {
        match self.branches.focus {
            BranchFocus::Local => self.branches.local_cursor = 0,
            BranchFocus::Remote => self.branches.remote_cursor = 0,
        }
    }

    fn branch_cursor_end(&mut self) {
        match self.branches.focus {
            BranchFocus::Local => {
                self.branches.local_cursor = self.branches.locals.len().saturating_sub(1);
            }
            BranchFocus::Remote => {
                self.branches.remote_cursor = self.branches.remotes.len().saturating_sub(1);
            }
        }
    }

    fn start_checkout(&mut self) {
        let target = match self.branches.focus {
            BranchFocus::Local => self
                .branches
                .locals
                .get(self.branches.local_cursor)
                .map(|b| b.name.clone()),
            BranchFocus::Remote => self
                .branches
                .remotes
                .get(self.branches.remote_cursor)
                .map(|b| b.name.clone()),
        };
        let Some(name) = target else { return };
        let n = name.clone();
        let toast_msg = format!("checked out {}", name);
        self.start(format!("checking out {}", n), move || {
            git::checkout_branch(&n)?;
            let snap = gather_status()?;
            let locals = git::local_branches().unwrap_or_default();
            let remotes = git::remote_branches().unwrap_or_default();
            Ok(OpDone {
                toast: Some(toast_msg),
                status: Some(snap),
                branches: Some(BranchesSnapshot { locals, remotes }),
                ..Default::default()
            })
        });
    }

    fn start_branch_delete(&mut self, force: bool) {
        if self.branches.focus != BranchFocus::Local {
            self.toast("delete only for local branches");
            return;
        }
        let Some(b) = self.branches.locals.get(self.branches.local_cursor).cloned() else { return };
        if b.is_head {
            self.toast("cannot delete current branch");
            return;
        }
        let title = if force { "Force-delete branch?" } else { "Delete branch?" };
        let message = if force {
            format!("Force-delete '{}' (git branch -D)? Unmerged work will be lost. [y/N]", b.name)
        } else {
            format!("Delete branch '{}'? [y/N]", b.name)
        };
        self.modal = Modal::Confirm(ConfirmDialog {
            title: title.into(),
            message,
            kind: ConfirmKind::BranchDelete { name: b.name, force },
        });
    }

    fn handle_new_branch(&mut self, key: KeyEvent) -> Result<()> {
        let Modal::NewBranch(mut ni) = self.modal.clone() else { return Ok(()) };
        match key.code {
            KeyCode::Esc => {
                self.modal = Modal::None;
                return Ok(());
            }
            KeyCode::Enter => {
                let name = ni.name.trim().to_string();
                if name.is_empty() {
                    self.toast("branch name required");
                    self.modal = Modal::NewBranch(ni);
                    return Ok(());
                }
                self.modal = Modal::None;
                let n = name.clone();
                let toast_msg = format!("created {}", name);
                self.start(format!("creating {}", n), move || {
                    git::branch_create(&n)?;
                    let locals = git::local_branches().unwrap_or_default();
                    let remotes = git::remote_branches().unwrap_or_default();
                    Ok(OpDone {
                        toast: Some(toast_msg),
                        branches: Some(BranchesSnapshot { locals, remotes }),
                        ..Default::default()
                    })
                });
                return Ok(());
            }
            KeyCode::Backspace => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    pop_word(&mut ni.name);
                } else {
                    ni.name.pop();
                }
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    ni.name.push(c);
                }
            }
            _ => {}
        }
        self.modal = Modal::NewBranch(ni);
        Ok(())
    }

    pub fn refresh_stash(&mut self) {
        self.start("fetching stash list", || {
            let stashes = git::stash_list().unwrap_or_default();
            Ok(OpDone {
                stashes: Some(stashes),
                ..Default::default()
            })
        });
    }

    fn refresh_stash_details(&mut self) {
        self.stash.details_scroll = 0;
        self.stash.details_text = match self.stash.stashes.get(self.stash.cursor) {
            Some(s) => git::stash_show(&s.reference)
                .unwrap_or_else(|e| format!("error: {}", e)),
            None => String::new(),
        };
    }

    fn handle_stash(&mut self, key: KeyEvent) -> Result<()> {
        match self.stash.focus {
            StashFocus::List => self.handle_stash_list(key),
            StashFocus::Details => self.handle_stash_details(key),
        }
    }

    fn handle_stash_list(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                if self.stash.cursor > 0 {
                    self.stash.cursor -= 1;
                    self.refresh_stash_details();
                }
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                if self.stash.cursor + 1 < self.stash.stashes.len() {
                    self.stash.cursor += 1;
                    self.refresh_stash_details();
                }
            }
            (KeyCode::Home, _) => {
                if !self.stash.stashes.is_empty() {
                    self.stash.cursor = 0;
                    self.refresh_stash_details();
                }
            }
            (KeyCode::End, _) => {
                if !self.stash.stashes.is_empty() {
                    self.stash.cursor = self.stash.stashes.len() - 1;
                    self.refresh_stash_details();
                }
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) | (KeyCode::Tab, _) | (KeyCode::Enter, _) => {
                if !self.stash.stashes.is_empty() {
                    self.stash.focus = StashFocus::Details;
                }
            }
            (KeyCode::Char('a'), _) => self.start_stash_apply(),
            (KeyCode::Char('p'), _) => self.start_stash_pop(),
            (KeyCode::Char('d'), _) => self.start_stash_drop(),
            (KeyCode::Char('r'), _) => self.refresh_stash(),
            _ => {}
        }
        Ok(())
    }

    fn handle_stash_details(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Esc, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) | (KeyCode::BackTab, _) | (KeyCode::Tab, _) => {
                self.stash.focus = StashFocus::List;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.stash.details_scroll = self.stash.details_scroll.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                self.stash.details_scroll = self.stash.details_scroll.saturating_add(1);
            }
            (KeyCode::PageUp, _) => {
                self.stash.details_scroll = self.stash.details_scroll.saturating_sub(10);
            }
            (KeyCode::PageDown, _) => {
                self.stash.details_scroll = self.stash.details_scroll.saturating_add(10);
            }
            (KeyCode::Home, _) => {
                self.stash.details_scroll = 0;
            }
            (KeyCode::End, _) => {
                let lines = self.stash.details_text.lines().count() as u16;
                self.stash.details_scroll = lines.saturating_sub(1);
            }
            (KeyCode::Char('r'), _) => self.refresh_stash(),
            _ => {}
        }
        Ok(())
    }

    fn start_stash_apply(&mut self) {
        let Some(s) = self.stash.stashes.get(self.stash.cursor).cloned() else { return };
        let reference = s.reference.clone();
        let toast_msg = format!("applied {}", s.reference);
        self.start(format!("applying {}", reference), move || {
            git::stash_apply(&reference)?;
            let snap = gather_status()?;
            let stashes = git::stash_list().unwrap_or_default();
            Ok(OpDone {
                toast: Some(toast_msg),
                status: Some(snap),
                stashes: Some(stashes),
                ..Default::default()
            })
        });
    }

    fn start_stash_pop(&mut self) {
        let Some(s) = self.stash.stashes.get(self.stash.cursor).cloned() else { return };
        let message = format!(
            "Pop {} into working tree? Stash will be removed. [y/N]",
            s.reference
        );
        self.modal = Modal::Confirm(ConfirmDialog {
            title: "Pop stash?".into(),
            message,
            kind: ConfirmKind::StashPop { reference: s.reference },
        });
    }

    fn start_stash_drop(&mut self) {
        let Some(s) = self.stash.stashes.get(self.stash.cursor).cloned() else { return };
        let message = format!("Permanently drop {}? [y/N]", s.reference);
        self.modal = Modal::Confirm(ConfirmDialog {
            title: "Drop stash?".into(),
            message,
            kind: ConfirmKind::StashDrop { reference: s.reference },
        });
    }

    fn refresh_log_details(&mut self) {
        self.log.details_scroll = 0;
        self.log.details_text = match self.log.commits.get(self.log.cursor) {
            Some(c) => git::show_commit(&c.sha).unwrap_or_else(|e| format!("error: {}", e)),
            None => String::new(),
        };
    }

    fn handle_log(&mut self, key: KeyEvent) -> Result<()> {
        match self.log.focus {
            LogFocus::List => self.handle_log_list(key),
            LogFocus::Details => self.handle_log_details(key),
        }
    }

    fn handle_log_list(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                if self.log.cursor > 0 {
                    self.log.cursor -= 1;
                    self.refresh_log_details();
                }
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                if self.log.cursor + 1 < self.log.commits.len() {
                    self.log.cursor += 1;
                    self.refresh_log_details();
                }
            }
            (KeyCode::Home, _) => {
                if !self.log.commits.is_empty() {
                    self.log.cursor = 0;
                    self.refresh_log_details();
                }
            }
            (KeyCode::End, _) => {
                if !self.log.commits.is_empty() {
                    self.log.cursor = self.log.commits.len() - 1;
                    self.refresh_log_details();
                }
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) | (KeyCode::Tab, _) | (KeyCode::Enter, _) => {
                if !self.log.commits.is_empty() {
                    self.log.focus = LogFocus::Details;
                }
            }
            (KeyCode::Char('r'), _) => {
                self.refresh_log();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_log_details(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Esc, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) | (KeyCode::BackTab, _) | (KeyCode::Tab, _) => {
                self.log.focus = LogFocus::List;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.log.details_scroll = self.log.details_scroll.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                self.log.details_scroll = self.log.details_scroll.saturating_add(1);
            }
            (KeyCode::PageUp, _) => {
                self.log.details_scroll = self.log.details_scroll.saturating_sub(10);
            }
            (KeyCode::PageDown, _) => {
                self.log.details_scroll = self.log.details_scroll.saturating_add(10);
            }
            (KeyCode::Home, _) => {
                self.log.details_scroll = 0;
            }
            (KeyCode::End, _) => {
                let lines = self.log.details_text.lines().count() as u16;
                self.log.details_scroll = lines.saturating_sub(1);
            }
            (KeyCode::Char('r'), _) => {
                self.refresh_log();
            }
            _ => {}
        }
        Ok(())
    }

    fn current(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    fn handle_files(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.refresh_diff();
                }
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                if self.cursor + 1 < self.rows.len() {
                    self.cursor += 1;
                    self.refresh_diff();
                }
            }
            (KeyCode::Home, _) => {
                if !self.rows.is_empty() {
                    self.cursor = 0;
                    self.refresh_diff();
                }
            }
            (KeyCode::End, _) => {
                if !self.rows.is_empty() {
                    self.cursor = self.rows.len() - 1;
                    self.refresh_diff();
                }
            }
            (KeyCode::Tab, _) => {
                if !self.rows.is_empty() {
                    self.open_diff();
                }
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) => self.right_action(),
            (KeyCode::Left, _) | (KeyCode::Char('h'), _) => self.left_action(),
            (KeyCode::Char(' '), _) => self.enter_action(),
            (KeyCode::Enter, _) => self.toggle_stage(),
            (KeyCode::Char('a'), _) => {
                self.start("staging all", || {
                    git::stage_all()?;
                    let snap = gather_status()?;
                    Ok(OpDone {
                        toast: Some("staged all".into()),
                        status: Some(snap),
                        ..Default::default()
                    })
                });
            }
            (KeyCode::Char('u'), _) => self.unstage_current(),
            (KeyCode::Char('d'), _) => self.start_discard(),
            (KeyCode::Char('c'), _) => self.modal = Modal::Commit(CommitInput::new()),
            (KeyCode::Char('P'), _) => self.start_push()?,
            (KeyCode::Char('r'), _) => self.start_refresh(),
            _ => {}
        }
        Ok(())
    }

    fn start_refresh(&mut self) {
        self.start("refreshing", || {
            let snap = gather_status()?;
            Ok(OpDone {
                toast: Some("refreshed".into()),
                status: Some(snap),
                ..Default::default()
            })
        });
    }

    fn right_action(&mut self) {
        let Some(r) = self.current() else { return };
        if r.is_dir {
            let p = r.path.clone();
            self.collapsed.remove(&p);
            self.rebuild_rows();
            self.clamp_cursor();
        } else {
            self.open_diff();
        }
    }

    fn show_diff_panel(&mut self) {
        if self.diff_view == DiffView::Hidden {
            self.diff_view = DiffView::Split;
        }
    }

    fn open_diff(&mut self) {
        self.show_diff_panel();
        self.focus = Focus::Diff;
    }

    fn close_diff(&mut self) {
        self.diff_view = DiffView::Hidden;
        self.focus = Focus::Files;
    }

    fn left_action(&mut self) {
        let Some(r) = self.current() else { return };
        if r.is_dir && r.expanded {
            let p = r.path.clone();
            self.collapsed.insert(p);
            self.rebuild_rows();
            self.clamp_cursor();
        } else {
            let parent = parent_path(&r.path);
            if !parent.is_empty() {
                if let Some(i) = self.rows.iter().position(|x| x.path == parent) {
                    self.cursor = i;
                    self.refresh_diff();
                }
            }
        }
    }

    fn enter_action(&mut self) {
        let Some(r) = self.current() else { return };
        if r.is_dir {
            let p = r.path.clone();
            if r.expanded {
                self.collapsed.insert(p);
            } else {
                self.collapsed.remove(&p);
            }
            self.rebuild_rows();
            self.clamp_cursor();
        } else if self.diff_view == DiffView::Hidden {
            self.show_diff_panel();
        } else {
            self.close_diff();
        }
    }

    fn clamp_cursor(&mut self) {
        if self.rows.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len() - 1;
        }
        self.refresh_diff();
    }

    fn handle_diff(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Esc, _) | (KeyCode::Left, _) => self.close_diff(),
            (KeyCode::BackTab, _) | (KeyCode::Tab, _) => self.close_diff(),
            (KeyCode::Enter, _) => {
                self.diff_view = match self.diff_view {
                    DiffView::Full => DiffView::Split,
                    _ => DiffView::Full,
                };
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.diff_scroll = self.diff_scroll.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                self.diff_scroll = self.diff_scroll.saturating_add(1);
            }
            (KeyCode::PageUp, _) => {
                self.diff_scroll = self.diff_scroll.saturating_sub(10);
            }
            (KeyCode::PageDown, _) => {
                self.diff_scroll = self.diff_scroll.saturating_add(10);
            }
            (KeyCode::Home, _) => {
                self.diff_scroll = 0;
            }
            (KeyCode::End, _) => {
                let lines = self.diff_text.lines().count() as u16;
                self.diff_scroll = lines.saturating_sub(1);
            }
            (KeyCode::Char('r'), _) => self.start_refresh(),
            _ => {}
        }
        Ok(())
    }

    fn toggle_stage(&mut self) {
        let Some(r) = self.current().cloned() else { return };
        if r.agg.has_unstaged_or_untracked() {
            let path = r.path.clone();
            let toast_msg = format!("staged {}", path);
            self.start(format!("staging {}", path), move || {
                git::stage(&path)?;
                let snap = gather_status()?;
                Ok(OpDone {
                    toast: Some(toast_msg),
                    status: Some(snap),
                    ..Default::default()
                })
            });
        } else if r.agg.has_staged() {
            let path = r.path.clone();
            let toast_msg = format!("unstaged {}", path);
            self.start(format!("unstaging {}", path), move || {
                git::unstage(&path)?;
                let snap = gather_status()?;
                Ok(OpDone {
                    toast: Some(toast_msg),
                    status: Some(snap),
                    ..Default::default()
                })
            });
        }
    }

    fn unstage_current(&mut self) {
        let Some(r) = self.current().cloned() else { return };
        if r.agg.has_staged() {
            let path = r.path.clone();
            let toast_msg = format!("unstaged {}", path);
            self.start(format!("unstaging {}", path), move || {
                git::unstage(&path)?;
                let snap = gather_status()?;
                Ok(OpDone {
                    toast: Some(toast_msg),
                    status: Some(snap),
                    ..Default::default()
                })
            });
        }
    }

    fn start_discard(&mut self) {
        let Some(r) = self.current().cloned() else { return };
        if r.is_dir {
            if r.agg.unstaged == 0 && r.agg.staged == 0 && r.agg.untracked == 0 {
                self.toast("nothing to discard");
                return;
            }
            self.modal = Modal::Confirm(ConfirmDialog {
                title: "Discard changes in folder?".into(),
                message: format!(
                    "Restore tracked files under '{}' to HEAD/index?\nUntracked files in this folder remain. [y/N]",
                    r.path
                ),
                kind: ConfirmKind::Restore { path: r.path, is_dir: true },
            });
            return;
        }
        if r.agg.untracked > 0 {
            self.modal = Modal::Confirm(ConfirmDialog {
                title: "Delete untracked file?".into(),
                message: format!("Permanently delete {} from disk? [y/N]", r.path),
                kind: ConfirmKind::DeleteUntracked { path: r.path },
            });
        } else if r.agg.unstaged > 0 {
            self.modal = Modal::Confirm(ConfirmDialog {
                title: "Discard changes?".into(),
                message: format!(
                    "Restore {} from index/HEAD? Working tree changes will be lost. [y/N]",
                    r.path
                ),
                kind: ConfirmKind::Restore { path: r.path, is_dir: false },
            });
        } else {
            self.toast("nothing to discard");
        }
    }

    fn handle_confirm(&mut self, key: KeyEvent) -> Result<()> {
        let Modal::Confirm(c) = self.modal.clone() else { return Ok(()) };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.modal = Modal::None;
                match c.kind {
                    ConfirmKind::Restore { path, is_dir } => {
                        if is_dir {
                            let prefix = format!("{}/", path);
                            let mut restore_paths: Vec<String> = Vec::new();
                            let mut delete_paths: Vec<String> = Vec::new();
                            for e in &self.entries {
                                if e.path.starts_with(&prefix) {
                                    if e.index == crate::git::Stage::Untracked {
                                        delete_paths.push(e.path.clone());
                                    } else if e.worktree.is_changed() {
                                        restore_paths.push(e.path.clone());
                                    }
                                }
                            }
                            let toast_msg = format!("restored folder {}", path);
                            self.start(format!("restoring {}", path), move || {
                                for p in &restore_paths {
                                    git::restore_worktree(p)?;
                                }
                                for p in &delete_paths {
                                    git::delete_untracked(p)?;
                                }
                                let snap = gather_status()?;
                                Ok(OpDone {
                                    toast: Some(toast_msg),
                                    status: Some(snap),
                                    ..Default::default()
                                })
                            });
                        } else {
                            let p = path.clone();
                            let toast_msg = format!("restored {}", path);
                            self.start(format!("restoring {}", p), move || {
                                git::restore_worktree(&p)?;
                                let snap = gather_status()?;
                                Ok(OpDone {
                                    toast: Some(toast_msg),
                                    status: Some(snap),
                                    ..Default::default()
                                })
                            });
                        }
                    }
                    ConfirmKind::DeleteUntracked { path } => {
                        let p = path.clone();
                        let toast_msg = format!("deleted {}", path);
                        self.start(format!("deleting {}", p), move || {
                            git::delete_untracked(&p)?;
                            let snap = gather_status()?;
                            Ok(OpDone {
                                toast: Some(toast_msg),
                                status: Some(snap),
                                ..Default::default()
                            })
                        });
                    }
                    ConfirmKind::CommitDespiteLfs { pending } => {
                        self.do_commit_with(pending);
                    }
                    ConfirmKind::StashPop { reference } => {
                        let r = reference.clone();
                        let toast_msg = format!("popped {}", reference);
                        self.start(format!("popping {}", r), move || {
                            git::stash_pop(&r)?;
                            let snap = gather_status()?;
                            let stashes = git::stash_list().unwrap_or_default();
                            Ok(OpDone {
                                toast: Some(toast_msg),
                                status: Some(snap),
                                stashes: Some(stashes),
                                ..Default::default()
                            })
                        });
                    }
                    ConfirmKind::StashDrop { reference } => {
                        let r = reference.clone();
                        let toast_msg = format!("dropped {}", reference);
                        self.start(format!("dropping {}", r), move || {
                            git::stash_drop(&r)?;
                            let stashes = git::stash_list().unwrap_or_default();
                            Ok(OpDone {
                                toast: Some(toast_msg),
                                stashes: Some(stashes),
                                ..Default::default()
                            })
                        });
                    }
                    ConfirmKind::BranchDelete { name, force } => {
                        let n = name.clone();
                        let toast_msg = format!("deleted {}", name);
                        self.start(format!("deleting {}", n), move || {
                            git::branch_delete(&n, force)?;
                            let locals = git::local_branches().unwrap_or_default();
                            let remotes = git::remote_branches().unwrap_or_default();
                            Ok(OpDone {
                                toast: Some(toast_msg),
                                branches: Some(BranchesSnapshot { locals, remotes }),
                                ..Default::default()
                            })
                        });
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.modal = Modal::None;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_commit(&mut self, key: KeyEvent) -> Result<()> {
        let Modal::Commit(mut ci) = self.modal.clone() else { return Ok(()) };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Esc, _) => {
                self.modal = Modal::None;
                return Ok(());
            }
            (KeyCode::Char('a'), true) => {
                ci.amend = !ci.amend;
                if ci.amend && ci.subject.is_empty() {
                    ci.subject = git::last_commit_subject().unwrap_or_default();
                }
            }
            (KeyCode::Enter, _) | (KeyCode::Char('s'), true) => {
                self.modal = Modal::Commit(ci);
                self.submit_commit()?;
                return Ok(());
            }
            (KeyCode::Backspace, true) => {
                pop_word(&mut ci.subject);
            }
            (KeyCode::Backspace, _) => {
                ci.subject.pop();
            }
            (KeyCode::Char(c), false) => {
                ci.subject.push(c);
            }
            _ => {}
        }
        self.modal = Modal::Commit(ci);
        Ok(())
    }

    fn submit_commit(&mut self) -> Result<()> {
        let Modal::Commit(ci) = self.modal.clone() else { return Ok(()) };
        if ci.subject.trim().is_empty() {
            self.toast("commit subject required");
            return Ok(());
        }
        let offending: Vec<String> = self
            .entries
            .iter()
            .filter(|e| {
                use crate::git::Stage;
                let staged = matches!(e.index,
                    Stage::Added | Stage::Modified | Stage::Renamed | Stage::Copied | Stage::TypeChange);
                staged && e.lfs_tracked && e.lfs_pointer_ok == Some(false)
            })
            .map(|e| e.path.clone())
            .collect();
        if !offending.is_empty() {
            let msg = format!(
                "Files match LFS pattern but staged as raw blobs:\n  {}\nCommit anyway? [y/N]",
                offending.join("\n  ")
            );
            self.modal = Modal::Confirm(ConfirmDialog {
                title: "LFS pointer mismatch".into(),
                message: msg,
                kind: ConfirmKind::CommitDespiteLfs { pending: ci },
            });
            return Ok(());
        }
        self.do_commit_with(ci);
        Ok(())
    }

    fn do_commit_with(&mut self, ci: CommitInput) {
        self.modal = Modal::Commit(ci.clone());
        let subject = ci.subject;
        let amend = ci.amend;
        self.start("committing", move || {
            let sha = git::commit(&subject, "", amend)?;
            let snap = gather_status()?;
            Ok(OpDone {
                toast: Some(format!("committed {}", sha)),
                status: Some(snap),
                ..Default::default()
            })
        });
    }

    fn start_push(&mut self) -> Result<()> {
        let branch = match git::current_branch() {
            Ok(b) => b,
            Err(e) => {
                self.toast(format!("no branch: {}", e));
                return Ok(());
            }
        };
        if let Some(up) = git::upstream()? {
            let remote = up.split('/').next().unwrap_or("origin").to_string();
            self.modal = Modal::Push(PushDialog::Running);
            self.do_push(&remote, &branch, false);
            return Ok(());
        }
        let remotes = git::remotes()?;
        if remotes.is_empty() {
            self.toast("no remotes configured");
            return Ok(());
        }
        if remotes.iter().any(|r| r == "origin") {
            self.modal = Modal::Push(PushDialog::Confirm {
                remote: "origin".into(),
                branch,
            });
        } else {
            self.modal = Modal::Push(PushDialog::Pick {
                remotes,
                cursor: 0,
                branch,
            });
        }
        Ok(())
    }

    fn handle_push_modal(&mut self, key: KeyEvent) -> Result<()> {
        let dlg = match &self.modal {
            Modal::Push(d) => d.clone(),
            _ => return Ok(()),
        };
        match dlg {
            PushDialog::Confirm { remote, branch } => match key.code {
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.modal = Modal::None;
                }
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.modal = Modal::Push(PushDialog::Running);
                    self.do_push(&remote, &branch, true);
                }
                _ => {}
            },
            PushDialog::Pick { mut remotes, mut cursor, branch } => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Up | KeyCode::Char('k') => {
                    if cursor > 0 {
                        cursor -= 1;
                    }
                    self.modal = Modal::Push(PushDialog::Pick { remotes, cursor, branch });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if cursor + 1 < remotes.len() {
                        cursor += 1;
                    }
                    self.modal = Modal::Push(PushDialog::Pick { remotes, cursor, branch });
                }
                KeyCode::Enter => {
                    let remote = remotes.remove(cursor);
                    self.modal = Modal::Push(PushDialog::Confirm { remote, branch });
                }
                _ => {}
            },
            PushDialog::Running => {}
        }
        Ok(())
    }

    fn do_push(&mut self, remote: &str, branch: &str, set_upstream: bool) {
        let rm = remote.to_string();
        let br = branch.to_string();
        self.modal = Modal::None;
        self.start(format!("pushing {} → {}", br, rm), move || {
            let msg = git::push(&rm, &br, set_upstream)?;
            let snap = gather_status()?;
            let first = msg.lines().next().unwrap_or("").to_string();
            Ok(OpDone {
                toast: Some(format!("pushed: {}", first)),
                status: Some(snap),
                ..Default::default()
            })
        });
    }
}

fn parent_path(p: &str) -> String {
    match p.rfind('/') {
        Some(i) => p[..i].to_string(),
        None => String::new(),
    }
}

fn pop_word(s: &mut String) {
    while matches!(s.chars().last(), Some(c) if c.is_whitespace()) {
        s.pop();
    }
    while matches!(s.chars().last(), Some(c) if !c.is_whitespace()) {
        s.pop();
    }
}
