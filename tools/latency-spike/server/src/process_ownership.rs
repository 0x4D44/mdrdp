//! Portable ownership rules for Windows process cleanup.

use std::path::Path;

/// Compare executable paths using Windows' case-insensitive, separator-insensitive
/// spelling rules. `canonicalize` may add the extended-path prefix while
/// `QueryFullProcessImageNameW` does not, so that prefix is deliberately ignored.
pub fn same_windows_executable(expected: &Path, actual: &Path) -> bool {
    fn normalized(path: &Path) -> String {
        let spelling = path.to_string_lossy().replace('/', "\\");
        let spelling = spelling
            .strip_prefix(r"\\?\")
            .or_else(|| spelling.strip_prefix(r"\??\"))
            .unwrap_or(&spelling);
        spelling.to_lowercase()
    }

    normalized(expected) == normalized(actual)
}

#[cfg(test)]
mod tests {
    use super::same_windows_executable;
    use std::path::Path;

    #[test]
    fn same_name_in_another_directory_is_not_owned() {
        assert!(!same_windows_executable(
            Path::new(r"C:\Program Files\rhydra\rhydra-server.exe"),
            Path::new(r"C:\Users\other\rhydra-server.exe"),
        ));
    }

    #[test]
    fn windows_path_spelling_does_not_change_ownership() {
        assert!(same_windows_executable(
            Path::new(r"\\?\C:\Program Files\Rhydra\rhydra-server.exe"),
            Path::new(r"c:/program files/rhydra/RHYDRA-SERVER.EXE"),
        ));
    }
}
