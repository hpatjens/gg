use crate::git::{FileEntry, Stage};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
}

/// Which half of the status view a tree represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Unstaged,
    Staged,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Agg {
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflict: u32,
    pub added: u32,
    pub modified: u32,
    pub deleted: u32,
}

impl Agg {
    pub fn merge(&mut self, o: &Agg) {
        self.staged += o.staged;
        self.unstaged += o.unstaged;
        self.untracked += o.untracked;
        self.conflict += o.conflict;
        self.added += o.added;
        self.modified += o.modified;
        self.deleted += o.deleted;
    }

    pub fn has_staged(&self) -> bool {
        self.staged > 0
    }

    pub fn has_unstaged_or_untracked(&self) -> bool {
        self.unstaged > 0 || self.untracked > 0
    }
}

#[derive(Debug, Clone)]
struct Node {
    name: String,
    path: String,
    kind: NodeKind,
    children: Vec<Node>,
    expanded: bool,
    agg: Agg,
    lfs_tracked: bool,
    lfs_pointer_warn: bool,
    entry_index: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub path: String,
    pub name: String,
    pub level: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub agg: Agg,
    pub lfs_tracked: bool,
    pub lfs_pointer_warn: bool,
    pub entry_index: Option<usize>,
}

pub fn build_rows(entries: &[FileEntry], collapsed: &HashSet<String>, side: Side) -> Vec<Row> {
    let mut root = Node {
        name: String::new(),
        path: String::new(),
        kind: NodeKind::Dir,
        children: vec![],
        expanded: true,
        agg: Agg::default(),
        lfs_tracked: false,
        lfs_pointer_warn: false,
        entry_index: None,
    };
    for (i, e) in entries.iter().enumerate() {
        let keep = match side {
            Side::Unstaged => e.in_unstaged(),
            Side::Staged => e.in_staged(),
        };
        if !keep {
            continue;
        }
        insert(&mut root, e, i, collapsed, side);
    }
    sort_rec(&mut root);
    aggregate(&mut root);
    for c in &mut root.children {
        coalesce(c, collapsed);
    }
    let mut rows = Vec::new();
    for c in &root.children {
        push_rows(c, 0, &mut rows);
    }
    rows
}

fn coalesce(n: &mut Node, collapsed: &HashSet<String>) {
    if matches!(n.kind, NodeKind::Dir) {
        while n.children.len() == 1 && matches!(n.children[0].kind, NodeKind::Dir) {
            let mut child = n.children.remove(0);
            n.name = format!("{}/{}", n.name, child.name);
            n.path = std::mem::take(&mut child.path);
            n.children = std::mem::take(&mut child.children);
        }
        n.expanded = !collapsed.contains(&n.path);
        for c in &mut n.children {
            coalesce(c, collapsed);
        }
    }
}

fn insert(parent: &mut Node, e: &FileEntry, idx: usize, collapsed: &HashSet<String>, side: Side) {
    let parts: Vec<&str> = e.path.split('/').collect();
    insert_parts(parent, &parts, 0, e, idx, "", collapsed, side);
}

fn insert_parts(
    parent: &mut Node,
    parts: &[&str],
    depth: usize,
    e: &FileEntry,
    idx: usize,
    prefix: &str,
    collapsed: &HashSet<String>,
    side: Side,
) {
    let name = parts[depth];
    let full = if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", prefix, name)
    };
    if depth + 1 == parts.len() {
        parent.children.push(Node {
            name: name.to_string(),
            path: full.clone(),
            kind: NodeKind::File,
            children: vec![],
            expanded: false,
            agg: file_agg(e, side),
            lfs_tracked: e.lfs_tracked,
            lfs_pointer_warn: e.lfs_tracked && e.lfs_pointer_ok == Some(false),
            entry_index: Some(idx),
        });
        return;
    }
    let pos = parent
        .children
        .iter()
        .position(|c| matches!(c.kind, NodeKind::Dir) && c.name == name);
    let cidx = match pos {
        Some(p) => p,
        None => {
            parent.children.push(Node {
                name: name.to_string(),
                path: full.clone(),
                kind: NodeKind::Dir,
                children: vec![],
                expanded: !collapsed.contains(&full),
                agg: Agg::default(),
                lfs_tracked: false,
                lfs_pointer_warn: false,
                entry_index: None,
            });
            parent.children.len() - 1
        }
    };
    insert_parts(&mut parent.children[cidx], parts, depth + 1, e, idx, &full, collapsed, side);
}

/// Status counts for one file, reflecting only the requested side. Callers only
/// pass entries that belong to `side` (see `in_unstaged`/`in_staged`).
fn file_agg(e: &FileEntry, side: Side) -> Agg {
    let mut a = Agg::default();
    match side {
        Side::Unstaged => {
            if e.index == Stage::Conflicted {
                a.conflict = 1;
                return a;
            }
            if e.index == Stage::Untracked {
                a.untracked = 1;
                return a;
            }
            a.unstaged = 1;
            match e.worktree {
                Stage::Added => a.added = 1,
                Stage::Modified => a.modified = 1,
                Stage::Deleted => a.deleted = 1,
                _ => {}
            }
        }
        Side::Staged => {
            a.staged = 1;
            match e.index {
                Stage::Added => a.added = 1,
                Stage::Modified => a.modified = 1,
                Stage::Deleted => a.deleted = 1,
                _ => {}
            }
        }
    }
    a
}

fn sort_rec(n: &mut Node) {
    n.children.sort_by(|a, b| {
        let da = matches!(a.kind, NodeKind::Dir);
        let db = matches!(b.kind, NodeKind::Dir);
        match (da, db) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });
    for c in &mut n.children {
        sort_rec(c);
    }
}

fn aggregate(n: &mut Node) -> Agg {
    if matches!(n.kind, NodeKind::File) {
        return n.agg;
    }
    let mut acc = Agg::default();
    let mut lfs = false;
    let mut warn = false;
    for c in &mut n.children {
        let sub = aggregate(c);
        acc.merge(&sub);
        if c.lfs_tracked {
            lfs = true;
        }
        if c.lfs_pointer_warn {
            warn = true;
        }
    }
    n.agg = acc;
    n.lfs_tracked = lfs;
    n.lfs_pointer_warn = warn;
    acc
}

fn push_rows(n: &Node, level: usize, out: &mut Vec<Row>) {
    out.push(Row {
        path: n.path.clone(),
        name: n.name.clone(),
        level,
        is_dir: matches!(n.kind, NodeKind::Dir),
        expanded: n.expanded,
        agg: n.agg,
        lfs_tracked: n.lfs_tracked,
        lfs_pointer_warn: n.lfs_pointer_warn,
        entry_index: n.entry_index,
    });
    if matches!(n.kind, NodeKind::Dir) && n.expanded {
        for c in &n.children {
            push_rows(c, level + 1, out);
        }
    }
}
