use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::document::Document;

/// A minimal stand-in for IntelliJ's Virtual File System: one API for
/// loading/saving file content as a `Document`. A real VFS also maintains
/// an app-level snapshot cache decoupled from disk and from any open
/// `Document`; that's future work (see README) once there's more than one
/// file open at a time.
pub struct VirtualFile {
    pub path: PathBuf,
}

impl VirtualFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        VirtualFile { path: path.into() }
    }

    pub fn load(&self) -> Result<Document> {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        Ok(Document::from_str(&text))
    }

    pub fn save(&self, doc: &mut Document) -> Result<()> {
        fs::write(&self.path, doc.text())?;
        doc.clear_modified();
        Ok(())
    }

    pub fn exists(path: &Path) -> bool {
        path.exists()
    }
}
