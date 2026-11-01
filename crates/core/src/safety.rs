//! Paths that are never cleanup candidates, whatever a recipe or a caller says.
//!
//! Matching is by name and location only, and errs toward protecting: a
//! `.env.example` is protected too. Comparison ignores case, because the default
//! filesystems on both platforms do.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedReason {
    /// Part of the operating system.
    System,
    /// Holds keys, tokens, or other secrets.
    Credentials,
    /// Repository history, which may hold unpushed work.
    GitMetadata,
    /// A filesystem root or the home folder itself.
    UserRoot,
    /// Content lives in the cloud, so removal frees nothing and may lose data.
    CloudPlaceholder,
}

const CREDENTIAL_DIRS: &[&str] = &[
    ".aws",
    ".azure",
    ".gnupg",
    ".kube",
    ".password-store",
    ".ssh",
    "gcloud",
    "keychains",
];

const CREDENTIAL_FILES: &[&str] = &[
    ".netrc",
    ".npmrc",
    ".pgpass",
    ".pypirc",
    "credentials",
    "credentials.json",
    "id_ed25519",
    "id_rsa",
];

const CREDENTIAL_SUFFIXES: &[&str] = &[".key", ".p12", ".pem", ".pfx"];

const MACOS_SYSTEM_ROOTS: &[&str] = &["/bin", "/etc", "/private/etc", "/sbin", "/system", "/usr"];

/// Homebrew and hand-installed software live here, not the OS.
const MACOS_SYSTEM_EXCEPTION: &str = "/usr/local";

const WINDOWS_SYSTEM_VARS: &[&str] = &[
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "SystemRoot",
];

/// Managed by Windows. The cleanup tools for these are the supported route.
const WINDOWS_SYSTEM_NAMES: &[&str] = &[
    "$recycle.bin",
    "hiberfil.sys",
    "pagefile.sys",
    "swapfile.sys",
    "system volume information",
];

/// Why `path` must never be touched, if it must not. Pass a resolved path:
/// a symlink or `..` here would dodge the location checks.
pub fn protected_reason(path: &Path, home: Option<&Path>) -> Option<ProtectedReason> {
    let normalized = normalize(path);
    let is_home = home.is_some_and(|home| normalize(home) == normalized);
    if is_home || is_filesystem_root(&normalized) {
        return Some(ProtectedReason::UserRoot);
    }

    let components: Vec<&str> = normalized.split('/').filter(|c| !c.is_empty()).collect();
    if components.contains(&".git") {
        return Some(ProtectedReason::GitMetadata);
    }
    let name = components.last().copied().unwrap_or_default();
    if components.iter().any(|c| CREDENTIAL_DIRS.contains(c)) || is_credential_file(name) {
        return Some(ProtectedReason::Credentials);
    }
    is_system_path(&normalized, &components).then_some(ProtectedReason::System)
}

fn normalize(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let text = text.replace('\\', "/").to_lowercase();
    text.trim_end_matches('/').to_owned()
}

/// `/` normalizes to empty, and `C:\` to `c:`.
fn is_filesystem_root(normalized: &str) -> bool {
    normalized.is_empty()
        || (normalized.len() == 2 && normalized.ends_with(':') && normalized.is_ascii())
}

fn is_credential_file(name: &str) -> bool {
    CREDENTIAL_FILES.contains(&name)
        || name == ".env"
        || name.starts_with(".env.")
        || CREDENTIAL_SUFFIXES.iter().any(|s| name.ends_with(s))
}

fn is_inside(normalized: &str, root: &str) -> bool {
    normalized
        .strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn is_system_path(normalized: &str, components: &[&str]) -> bool {
    if cfg!(windows) {
        let in_system_folder = WINDOWS_SYSTEM_VARS.iter().any(|var| {
            std::env::var_os(var)
                .is_some_and(|root| is_inside(normalized, &normalize(Path::new(&root))))
        });
        in_system_folder || components.iter().any(|c| WINDOWS_SYSTEM_NAMES.contains(c))
    } else {
        !is_inside(normalized, MACOS_SYSTEM_EXCEPTION)
            && MACOS_SYSTEM_ROOTS
                .iter()
                .any(|root| is_inside(normalized, root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(path: &str) -> Option<ProtectedReason> {
        protected_reason(Path::new(path), Some(Path::new("/Users/me")))
    }

    #[test]
    fn roots_and_home_are_protected() {
        assert_eq!(reason("/"), Some(ProtectedReason::UserRoot));
        assert_eq!(reason("/Users/me"), Some(ProtectedReason::UserRoot));
        assert_eq!(reason("/Users/me/"), Some(ProtectedReason::UserRoot));
        assert_eq!(reason(r"C:\"), Some(ProtectedReason::UserRoot));
        assert_eq!(reason("/Users/me/code"), None);
    }

    #[test]
    fn credentials_are_protected_by_folder_and_by_name() {
        for path in [
            "/Users/me/.ssh/config",
            "/Users/me/.aws",
            "/Users/me/Library/Keychains/login.keychain-db",
            "/Users/me/app/.env",
            "/Users/me/app/.env.local",
            "/Users/me/app/server.PEM",
            "/Users/me/app/tls/private.key",
            "/Users/me/.npmrc",
        ] {
            assert_eq!(reason(path), Some(ProtectedReason::Credentials), "{path}");
        }
    }

    #[test]
    fn similar_names_are_not_credentials() {
        for path in ["/Users/me/app/environment.ts", "/Users/me/app/monkey.txt"] {
            assert_eq!(reason(path), None, "{path}");
        }
    }

    #[test]
    fn git_metadata_is_protected_at_any_depth() {
        assert_eq!(
            reason("/Users/me/app/.git"),
            Some(ProtectedReason::GitMetadata)
        );
        assert_eq!(
            reason("/Users/me/app/.git/objects/pack"),
            Some(ProtectedReason::GitMetadata)
        );
        assert_eq!(reason("/Users/me/app/.github"), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn system_locations_are_protected_except_usr_local() {
        for path in [
            "/System/Library",
            "/usr/bin/git",
            "/bin",
            "/private/etc/hosts",
        ] {
            assert_eq!(reason(path), Some(ProtectedReason::System), "{path}");
        }
        assert_eq!(reason("/SYSTEM/Library"), Some(ProtectedReason::System));
        assert_eq!(reason("/usr/local/lib/thing"), None);
        assert_eq!(reason("/usrdata/thing"), None);
    }

    #[cfg(windows)]
    #[test]
    fn system_locations_are_protected_on_windows() {
        for path in [
            r"C:\Windows\System32",
            r"C:\Program Files\App",
            r"C:\pagefile.sys",
            r"D:\System Volume Information",
        ] {
            assert_eq!(reason(path), Some(ProtectedReason::System), "{path}");
        }
        assert_eq!(reason(r"C:\Users\me\code"), None);
    }
}
