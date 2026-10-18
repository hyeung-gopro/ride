use std::fs;
use std::path::{Path, PathBuf};

/// One entry in the project tree. Directory children are loaded lazily on
/// first expand, same as the IntelliJ VFS only materializing a directory
/// listing once something actually asks for it.
pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<FileNode>>,
}

impl FileNode {
    fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let is_dir = path.is_dir();
        FileNode {
            path,
            name,
            is_dir,
            expanded: false,
            children: None,
        }
    }

    fn load_children(&mut self) {
        if self.children.is_some() || !self.is_dir {
            return;
        }
        let mut entries: Vec<FileNode> = fs::read_dir(&self.path)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| FileNode::new(e.path())).collect())
            .unwrap_or_default();
        entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        self.children = Some(entries);
    }

    fn expand(&mut self) {
        if self.is_dir && !self.expanded {
            self.load_children();
            self.expanded = true;
        }
    }

    fn collapse(&mut self) {
        self.expanded = false;
    }
}

/// The Project View equivalent: a root directory rendered as a tree, with
/// a path-based selection cursor (robust to the row list reshuffling as
/// nodes expand/collapse, unlike a plain row index).
pub struct FileTree {
    pub root: FileNode,
    pub selected_path: PathBuf,
}

impl FileTree {
    pub fn new(root_path: PathBuf) -> Self {
        let mut root = FileNode::new(root_path.clone());
        root.is_dir = true;
        root.expand();
        FileTree {
            root,
            selected_path: root_path,
        }
    }

    pub fn visible_rows(&self) -> Vec<(usize, &FileNode)> {
        let mut rows = Vec::new();
        Self::collect(&self.root, 0, &mut rows);
        rows
    }

    fn collect<'a>(node: &'a FileNode, depth: usize, rows: &mut Vec<(usize, &'a FileNode)>) {
        rows.push((depth, node));
        if node.is_dir && node.expanded {
            if let Some(children) = &node.children {
                for c in children {
                    Self::collect(c, depth + 1, rows);
                }
            }
        }
    }

    fn find<'a>(node: &'a FileNode, path: &Path) -> Option<&'a FileNode> {
        if node.path == path {
            return Some(node);
        }
        node.children
            .as_ref()
            .and_then(|children| children.iter().find_map(|c| Self::find(c, path)))
    }

    fn find_mut<'a>(node: &'a mut FileNode, path: &Path) -> Option<&'a mut FileNode> {
        if node.path == path {
            return Some(node);
        }
        node.children
            .as_mut()
            .and_then(|children| children.iter_mut().find_map(|c| Self::find_mut(c, path)))
    }

    pub fn selected_node(&self) -> Option<&FileNode> {
        Self::find(&self.root, &self.selected_path)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let rows = self.visible_rows();
        if rows.is_empty() {
            return;
        }
        let cur = rows
            .iter()
            .position(|(_, n)| n.path == self.selected_path)
            .unwrap_or(0) as isize;
        let new = (cur + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.selected_path = rows[new].1.path.clone();
    }

    /// Selects the row at `index`, if one exists (used for mouse clicks).
    pub fn select_row(&mut self, index: usize) {
        if let Some((_, node)) = self.visible_rows().get(index) {
            self.selected_path = node.path.clone();
        }
    }

    pub fn toggle_selected(&mut self) {
        let path = self.selected_path.clone();
        if let Some(node) = Self::find_mut(&mut self.root, &path) {
            if node.expanded {
                node.collapse();
            } else {
                node.expand();
            }
        }
    }

    pub fn expand_selected(&mut self) {
        let path = self.selected_path.clone();
        if let Some(node) = Self::find_mut(&mut self.root, &path) {
            node.expand();
        }
    }

    pub fn collapse_selected(&mut self) {
        let path = self.selected_path.clone();
        if let Some(node) = Self::find_mut(&mut self.root, &path) {
            node.collapse();
        }
    }

    /// Creates an empty file named `name` inside `dir` on disk, then
    /// re-loads that directory's children so it shows up, expanded, and
    /// selects it. Mirrors IntelliJ's "New File" action from the Project
    /// view.
    pub fn create_file(&mut self, dir: &Path, name: &str) -> std::io::Result<PathBuf> {
        let path = dir.join(name);
        fs::File::create(&path)?;
        if let Some(node) = Self::find_mut(&mut self.root, dir) {
            node.children = None;
            node.expand();
        }
        self.selected_path = path.clone();
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn sandbox() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ride_tree_test_{}_{id}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("sub/b.txt"), "b").unwrap();
        dir
    }

    #[test]
    fn root_starts_expanded_with_children_loaded() {
        let dir = sandbox();
        let tree = FileTree::new(dir.clone());
        let rows = tree.visible_rows();
        // root + a.txt + sub (sub's own children not yet loaded/expanded)
        assert_eq!(rows.len(), 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expanding_a_dir_reveals_its_children() {
        let dir = sandbox();
        let mut tree = FileTree::new(dir.clone());
        tree.selected_path = dir.join("sub");
        tree.toggle_selected();
        let rows = tree.visible_rows();
        assert!(rows.iter().any(|(_, n)| n.path == dir.join("sub/b.txt")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn move_selection_clamps_at_bounds() {
        let dir = sandbox();
        let mut tree = FileTree::new(dir.clone());
        tree.move_selection(-5);
        assert_eq!(tree.selected_path, dir);
        tree.move_selection(100);
        let rows = tree.visible_rows();
        assert_eq!(tree.selected_path, rows.last().unwrap().1.path);
        fs::remove_dir_all(&dir).unwrap();
    }
}
