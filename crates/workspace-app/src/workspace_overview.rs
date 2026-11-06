//! Summaries based only on state already loaded in TWR; never scan project files.
use std::path::{Path, PathBuf};

/// Associate a buffer with the most specific known project root.
pub fn buffer_owner<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a Path> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .map(PathBuf::as_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chooses_nested_root_without_matching_sibling_prefixes() {
        let roots = vec![PathBuf::from("/demo"), PathBuf::from("/demo/nested")];
        assert_eq!(
            buffer_owner(Path::new("/demo/nested/a"), &roots),
            Some(Path::new("/demo/nested"))
        );
        assert_eq!(
            buffer_owner(Path::new("/demo/other"), &roots),
            Some(Path::new("/demo"))
        );
        assert_eq!(buffer_owner(Path::new("/demonstration/a"), &roots), None);
    }
}
