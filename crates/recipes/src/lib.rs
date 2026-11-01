//! Declarative recipe loader and matcher.
//!
//! A recipe says what a directory is, when to leave it alone, and how to explain
//! it. It cannot run anything: the action is a closed enum, `regenerate` hints are
//! text for humans, and unknown keys are load errors, so a `command` field fails
//! rather than being ignored.

use std::fs;
use std::path::{Path, PathBuf};

use gigsift_core::{Action, Category, Protection, RebuildCost, Risk};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

/// Version of the recipe file format, independent of the JSON contract.
pub const FORMAT_VERSION: u32 = 1;

/// Compiled into the binary, so an installed gigsift needs no data files.
const BUILTIN: &[(&str, &str)] = &[
    (
        "node-modules.toml",
        include_str!("../recipes/node-modules.toml"),
    ),
    (
        "python-venv.toml",
        include_str!("../recipes/python-venv.toml"),
    ),
    (
        "rust-target.toml",
        include_str!("../recipes/rust-target.toml"),
    ),
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub format_version: u32,
    pub id: String,
    pub name: String,
    pub platforms: Vec<Platform>,
    pub category: Category,
    pub action: Action,
    pub risk: Risk,
    pub rebuild_cost: RebuildCost,
    pub detect: Detect,
    pub protect: Protect,
    pub explain: Explain,
    pub regenerate: Vec<Regenerate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Macos,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Macos
        }
    }
}

/// A directory matches when a glob matches its path and each non-empty marker
/// list has at least one file present.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    pub paths: Vec<String>,
    /// Looked for beside the directory.
    #[serde(default)]
    pub parent_markers: Vec<String>,
    /// Looked for inside the directory.
    #[serde(default)]
    pub inner_markers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protect {
    pub when: Vec<Protection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Explain {
    pub reason: String,
    pub loses: String,
}

/// Picked by the first marker found beside the directory. The last entry has no
/// marker and is the fallback.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Regenerate {
    pub marker: Option<String>,
    /// Shown to a human. Never executed.
    pub hint: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RecipeError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid recipe {origin}: {source}")]
    Parse {
        origin: String,
        source: Box<toml::de::Error>,
    },
    #[error("invalid recipe {origin}: {problem}")]
    Invalid { origin: String, problem: String },
}

#[derive(Debug)]
struct Compiled {
    recipe: Recipe,
    paths: GlobSet,
}

#[derive(Debug, Default)]
pub struct RecipeSet {
    recipes: Vec<Compiled>,
}

/// A directory a recipe recognized.
#[derive(Debug, Clone, Copy)]
pub struct Match<'a> {
    pub recipe: &'a Recipe,
    pub regenerate: &'a str,
}

impl RecipeSet {
    /// The recipes shipped with gigsift. Tests prove these all load.
    pub fn builtin() -> Self {
        let mut set = Self::default();
        for (name, text) in BUILTIN {
            set.add(text, name).expect("built-in recipe is invalid");
        }
        set
    }

    /// Loads every `*.toml` in `dir`, in name order.
    pub fn load_dir(dir: &Path) -> Result<Self, RecipeError> {
        let read_error = |source| RecipeError::Read {
            path: dir.to_owned(),
            source,
        };
        let mut files = Vec::new();
        for entry in fs::read_dir(dir).map_err(read_error)? {
            let path = entry.map_err(read_error)?.path();
            if path.extension().is_some_and(|e| e == "toml") {
                files.push(path);
            }
        }
        files.sort();

        let mut set = Self::default();
        for path in files {
            let text = fs::read_to_string(&path).map_err(|source| RecipeError::Read {
                path: path.clone(),
                source,
            })?;
            set.add(&text, &path.display().to_string())?;
        }
        Ok(set)
    }

    /// Parses and validates one recipe. `origin` names it in errors.
    pub fn add(&mut self, text: &str, origin: &str) -> Result<(), RecipeError> {
        let recipe: Recipe = toml::from_str(text).map_err(|source| RecipeError::Parse {
            origin: origin.to_owned(),
            source: Box::new(source),
        })?;
        let invalid = |problem: String| RecipeError::Invalid {
            origin: origin.to_owned(),
            problem,
        };
        validate(&recipe).map_err(invalid)?;
        if self.recipes.iter().any(|c| c.recipe.id == recipe.id) {
            return Err(invalid(format!("duplicate id `{}`", recipe.id)));
        }
        let paths = compile_globs(&recipe.detect.paths).map_err(invalid)?;
        self.recipes.push(Compiled { recipe, paths });
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    /// The recipe that recognizes `path`, if any. Only stats marker files.
    pub fn match_dir(&self, path: &Path, platform: Platform) -> Option<Match<'_>> {
        let compiled = self.recipes.iter().find(|c| {
            c.recipe.platforms.contains(&platform)
                && c.paths.is_match(path)
                && markers_present(&c.recipe.detect, path)
        })?;
        let recipe = &compiled.recipe;
        Some(Match {
            recipe,
            regenerate: regenerate_hint(recipe, path),
        })
    }

    /// The outermost recognized directory at or above `path`, so a file deep in
    /// `node_modules` belongs to that `node_modules`, matching what [`Self::find`]
    /// would report. Symlinks are not directories here.
    pub fn match_path(&self, path: &Path, platform: Platform) -> Option<(PathBuf, Match<'_>)> {
        path.ancestors()
            .filter(|dir| fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()))
            .filter_map(|dir| Some((dir.to_owned(), self.match_dir(dir, platform)?)))
            .last()
    }

    /// Walks `root` for matches without descending into one, so a package's own
    /// `node_modules` is part of its parent. Unreadable directories are skipped:
    /// this walk finds candidates and never reports totals.
    pub fn find(&self, root: &Path, platform: Platform) -> Vec<(PathBuf, Match<'_>)> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_owned()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let path = entry.path();
                match self.match_dir(&path, platform) {
                    Some(hit) => found.push((path, hit)),
                    None => pending.push(path),
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }
}

fn validate(recipe: &Recipe) -> Result<(), String> {
    if recipe.format_version != FORMAT_VERSION {
        return Err(format!(
            "format_version {} is not supported, expected {FORMAT_VERSION}",
            recipe.format_version
        ));
    }
    let id_ok = !recipe.id.is_empty()
        && recipe
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !id_ok {
        return Err(format!("id `{}` must be lowercase kebab-case", recipe.id));
    }
    if recipe.platforms.is_empty() {
        return Err("platforms must not be empty".to_owned());
    }
    let detect = &recipe.detect;
    if detect.paths.is_empty() {
        return Err("detect.paths must not be empty".to_owned());
    }
    if detect.parent_markers.is_empty() && detect.inner_markers.is_empty() {
        return Err("a name alone proves nothing: add parent_markers or inner_markers".to_owned());
    }
    match recipe.regenerate.split_last() {
        Some((last, rest))
            if last.marker.is_none() && rest.iter().all(|entry| entry.marker.is_some()) => {}
        _ => {
            return Err("regenerate must end with one entry that has no marker".to_owned());
        }
    }
    let regenerate_markers = recipe.regenerate.iter().filter_map(|e| e.marker.as_ref());
    for marker in detect
        .parent_markers
        .iter()
        .chain(&detect.inner_markers)
        .chain(regenerate_markers)
    {
        if !is_plain_file_name(marker) {
            return Err(format!("marker `{marker}` must be a plain file name"));
        }
    }
    Ok(())
}

/// Keeps markers beside the directory being judged, so a recipe can't probe elsewhere.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| e.to_string())?;
        builder.add(glob);
    }
    builder.build().map_err(|e| e.to_string())
}

fn markers_present(detect: &Detect, path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let any_in = |dir: &Path, markers: &[String]| {
        markers.is_empty() || markers.iter().any(|m| dir.join(m).is_file())
    };
    any_in(parent, &detect.parent_markers) && any_in(path, &detect.inner_markers)
}

fn regenerate_hint<'a>(recipe: &'a Recipe, path: &Path) -> &'a str {
    let parent = path.parent();
    recipe
        .regenerate
        .iter()
        .find(|entry| match (&entry.marker, parent) {
            (Some(marker), Some(parent)) => parent.join(marker).is_file(),
            (Some(_), None) => false,
            (None, _) => true,
        })
        .map_or("", |entry| entry.hint.as_str())
}
