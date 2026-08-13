use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum DiscoverError {
    #[error("source directory does not exist: {0}")]
    Missing(String),
    #[error("failed to walk source directory: {0}")]
    Walk(#[from] ignore::Error),
    #[error("source path is not valid UTF-8: {0}")]
    NonUtf8(String),
}

pub(crate) fn sources(root: &Path) -> Result<Vec<PathBuf>, DiscoverError> {
    if !root.is_dir() {
        return Err(DiscoverError::Missing(root.display().to_string()));
    }

    let mut paths = Vec::new();
    for entry in WalkBuilder::new(root).standard_filters(true).build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
            continue;
        }
        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("rs" | "ts" | "tsx" | "mts" | "cts")
        ) {
            paths.push(entry.into_path());
        }
    }

    paths.sort_by(|left, right| {
        relative(root, left)
            .unwrap_or_default()
            .cmp(&relative(root, right).unwrap_or_default())
    });
    Ok(paths)
}

pub(crate) fn relative(root: &Path, path: &Path) -> Result<String, DiscoverError> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let text = relative
        .to_str()
        .ok_or_else(|| DiscoverError::NonUtf8(path.display().to_string()))?;
    Ok(text.replace('\\', "/"))
}
