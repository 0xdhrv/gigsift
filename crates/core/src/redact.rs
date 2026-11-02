//! Controls how much of a user's file paths leave the machine. Anything sent to an
//! AI tool may reach a remote model, and paths carry usernames and project names.

use std::path::Path;
use std::str::FromStr;

use serde_json::Value;

/// Keys that hold a path in the JSON contract.
const PATH_KEYS: &[&str] = &[
    "matched_path",
    "path",
    "root",
    "symlink_target",
    "trash_location",
];

/// What stands in for a path's folders under [`PathSharing::Hidden`].
const HIDDEN: &str = "<hidden>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSharing {
    /// Absolute paths, unchanged.
    Full,
    /// The home folder becomes `~`, which keeps the username out.
    Home,
    /// Only the final name of each path remains, like `<hidden>/node_modules`.
    Hidden,
}

impl FromStr for PathSharing {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "full" => Ok(Self::Full),
            "home" => Ok(Self::Home),
            "hidden" => Ok(Self::Hidden),
            other => Err(format!(
                "`{other}` is not a sharing level, expected full, home, or hidden"
            )),
        }
    }
}

/// Applies `sharing` throughout a serialized response. The home folder is replaced
/// in every string, not only path fields, since messages can quote paths too.
pub fn redact(value: &mut Value, sharing: PathSharing, home: Option<&Path>) {
    if sharing == PathSharing::Full {
        return;
    }
    let home = home.and_then(Path::to_str);
    walk(value, None, sharing, home);
}

fn walk(value: &mut Value, key: Option<&str>, sharing: PathSharing, home: Option<&str>) {
    match value {
        Value::String(text) => {
            let is_path = key.is_some_and(|key| PATH_KEYS.contains(&key));
            *text = if sharing == PathSharing::Hidden && is_path {
                hide(text)
            } else {
                home.map_or_else(|| text.clone(), |home| replace_home(text, home))
            };
        }
        Value::Array(items) => {
            for item in items {
                walk(item, key, sharing, home);
            }
        }
        Value::Object(map) => {
            for (name, item) in map.iter_mut() {
                walk(item, Some(name), sharing, home);
            }
        }
        _ => {}
    }
}

fn hide(path: &str) -> String {
    let name = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    format!("{HIDDEN}/{name}")
}

/// Only matches `home` at a folder boundary, so `/Users/ann` leaves `/Users/anna`.
fn replace_home(text: &str, home: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(home) {
        let end = start + home.len();
        let at_boundary = rest[end..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric() && next != '_' && next != '-');
        result.push_str(&rest[..start]);
        result.push_str(if at_boundary { "~" } else { home });
        rest = &rest[end..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const HOME: &str = "/Users/ann";

    fn sample() -> Value {
        json!({
            "root": "/Users/ann/code",
            "entries": [{ "path": "/Users/ann/code/app/node_modules", "logical_bytes": 5 }],
            "elsewhere": { "path": "/opt/tool/" },
            "warnings": [{ "message": "could not read /Users/ann/code/x" }],
        })
    }

    fn redacted(sharing: PathSharing) -> Value {
        let mut value = sample();
        redact(&mut value, sharing, Some(Path::new(HOME)));
        value
    }

    #[test]
    fn full_changes_nothing() {
        assert_eq!(redacted(PathSharing::Full), sample());
    }

    #[test]
    fn home_hides_the_username_everywhere() {
        let value = redacted(PathSharing::Home);
        assert_eq!(value["root"], "~/code");
        assert_eq!(value["entries"][0]["path"], "~/code/app/node_modules");
        assert_eq!(value["warnings"][0]["message"], "could not read ~/code/x");
        assert_eq!(value["elsewhere"]["path"], "/opt/tool/");
        assert!(!value.to_string().contains("ann"));
    }

    #[test]
    fn hidden_keeps_only_final_names() {
        let value = redacted(PathSharing::Hidden);
        assert_eq!(value["root"], "<hidden>/code");
        assert_eq!(value["entries"][0]["path"], "<hidden>/node_modules");
        assert_eq!(value["elsewhere"]["path"], "<hidden>/tool");
        assert!(!value.to_string().contains("ann"));
    }

    #[test]
    fn numbers_are_left_alone() {
        assert_eq!(
            redacted(PathSharing::Hidden)["entries"][0]["logical_bytes"],
            5
        );
    }

    #[test]
    fn a_longer_username_is_not_mistaken_for_home() {
        let mut value = json!({ "path": "/Users/anna/code" });
        redact(&mut value, PathSharing::Home, Some(Path::new(HOME)));
        assert_eq!(value["path"], "/Users/anna/code");
    }

    #[test]
    fn windows_paths_are_handled() {
        let mut value = json!({ "path": r"C:\Users\ann\code\app" });
        let home = Path::new(r"C:\Users\ann");
        redact(&mut value, PathSharing::Home, Some(home));
        assert_eq!(value["path"], r"~\code\app");

        let mut value = json!({ "path": r"D:\code\app" });
        redact(&mut value, PathSharing::Hidden, Some(home));
        assert_eq!(value["path"], "<hidden>/app");
    }

    #[test]
    fn levels_parse_from_text() {
        assert_eq!("home".parse(), Ok(PathSharing::Home));
        assert!("everything".parse::<PathSharing>().is_err());
    }
}
