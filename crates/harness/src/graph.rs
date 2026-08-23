//! A harness is a module graph, not a single file.
//!
//! The graph owns every module a harness may reach — its own entry module,
//! its relative imports, and any `https:` dependency that survived the
//! hardcoded allowlist in [`crate::loader`]. The isolate is booted with only
//! this graph on the loader's allowlist, so "import control is the sandbox"
//! (`abstract.md` §7) still holds: a module not in the graph is a module the
//! harness cannot name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use url::Url;

/// A harness's module graph, keyed by the URL deno_graph resolves to.
///
/// The main entry is served at `harness:/main.js` regardless of the physical
/// layout; relative imports resolve against it into further `harness:` or
/// `https:` URLs. Anything the harness cannot name through this graph is
/// refused by the loader.
#[derive(Debug, Clone)]
pub struct Harness {
    /// resolved URL -> source (JS or TS; the loader transpiles TS).
    modules: HashMap<String, String>,
    /// The entry the bootstrap imports: always `harness:/main.js`.
    pub main: String,
}

impl Harness {
    /// A single-file harness — the degenerate graph. Every caller that used
    /// to hand `HarnessRun` a bare string keeps working through this.
    pub fn single(source: String) -> Self {
        Self {
            modules: HashMap::from([(crate::loader::HARNESS_SPECIFIER.to_string(), source)]),
            main: crate::loader::HARNESS_SPECIFIER.to_string(),
        }
    }

    /// A bundled harness, embedded at compile time so no filesystem read ever
    /// happens at runtime. Equivalent to `from_dir` against the crate's
    /// `harnesses/<name>/` directory.
    pub fn bundled(dir: &include_dir::Dir<'_>) -> Self {
        let mut modules = HashMap::new();
        for file in dir.files() {
            let spec = spec_for_rel(file.path());
            let source = file.contents_utf8().unwrap_or_default().to_string();
            modules.insert(spec, source);
        }
        let main = crate::loader::HARNESS_SPECIFIER.to_string();
        Self { modules, main }
    }

    /// A harness built from a live directory tree. The entry is `main.js` or
    /// `main.ts` at the tree's root.
    ///
    /// This is the live-directory seam: it re-introduces a filesystem read
    /// (from an allowlisted root, not an arbitrary path). Callers that must
    /// keep harness code fully embedded should use [`Self::bundled`].
    pub fn from_dir(root: &Path) -> Result<Self, BuildError> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(BuildError::NotDirectory(root));
        }
        let base =
            Url::from_directory_path(&root).map_err(|()| BuildError::NotDirectory(root.clone()))?;
        let mut modules = HashMap::new();
        let mut main = None;
        let mut files = Vec::new();
        walk_files(&root, &mut files)?;
        for path in files {
            let source = std::fs::read_to_string(&path)?;
            let rel = path.strip_prefix(&root).expect("walker stays under root");
            let rel = rel.to_string_lossy();
            let spec = base
                .join(&rel)
                .map_err(|_| BuildError::BadPath(path.clone()))?
                .to_string();
            if is_entry_rel(&rel) {
                main = Some(spec.clone());
            }
            modules.insert(spec, source);
        }
        let Some(main) = main else {
            return Err(BuildError::NoEntry);
        };
        Ok(Self { modules, main })
    }

    /// The source for a resolved specifier, if it is in the graph.
    pub(crate) fn source_for(&self, spec: &str) -> Option<&str> {
        self.modules.get(spec).map(String::as_str)
    }
}

/// Is this a relative path the tree's entry point?
fn is_entry_rel(rel: &str) -> bool {
    rel == "main.js" || rel == "main.ts"
}

/// The specifier for a bundled harness's file, keyed off its relative path
/// (which `include_dir` returns relative to the directory it was given).
fn spec_for_rel(rel: &Path) -> String {
    if is_entry_rel(&rel.to_string_lossy()) {
        return crate::loader::HARNESS_SPECIFIER.to_string();
    }
    // Bundled harnesses use `harness:/` namespace URLs for their relative
    // files; the loader resolves against this the same way it resolves
    // against `file:`/`https:` roots.
    let rel = rel.to_string_lossy();
    format!("harness:///{rel}")
}

/// Collects every file path under `root`, recursively.
fn walk_files(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("harness directory is not readable: {0}")]
    Io(#[from] std::io::Error),
    #[error("harness root is not a directory: {0}")]
    NotDirectory(std::path::PathBuf),
    #[error("could not form a module URL for {0}")]
    BadPath(std::path::PathBuf),
    #[error("harness directory has no `main.js` or `main.ts` entry")]
    NoEntry,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_file_graph_serves_its_one_module() {
        let g = Harness::single("export default 1;".to_string());
        assert_eq!(
            g.source_for(crate::loader::HARNESS_SPECIFIER),
            Some("export default 1;")
        );
    }
}
