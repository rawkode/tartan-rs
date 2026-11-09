//! Reading a configuration package and what it needs from a repository at one commit.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;

use crate::ConfigError;

/// A directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's name.
    pub name: String,
    /// Whether it is a directory.
    pub is_dir: bool,
}

/// A repository at one commit, as the loader reads it. Paths are relative to the root, with
/// `/` separators; the root is `""`.
#[async_trait]
pub trait ConfigSource: Send + Sync {
    /// The entries of directory `dir`, or `None` if there is no such directory.
    async fn list(&self, dir: &str) -> anyhow::Result<Option<Vec<DirEntry>>>;

    /// The contents of file `path`, or `None` if there is no such file.
    async fn read(&self, path: &str) -> anyhow::Result<Option<Vec<u8>>>;
}

/// How much a configuration may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Files read.
    pub files: usize,
    /// Directories listed.
    pub dirs: usize,
    /// Bytes of CUE read.
    pub bytes: usize,
}

/// The limits [`load`] applies.
pub const LIMITS: Limits = Limits {
    files: 500,
    dirs: 64,
    bytes: 4 << 20,
};

/// The files of a configuration: path to contents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigFiles {
    files: BTreeMap<String, String>,
}

impl ConfigFiles {
    /// Each file and its contents, by path.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.files.iter()
    }

    /// Whether there is a file at `path`.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Whether a root file declares `package <name>`.
    pub fn has_root_package(&self, name: &str) -> bool {
        self.files
            .iter()
            .filter(|(path, _)| !path.contains('/'))
            .any(|(_, contents)| package_of(contents).as_deref() == Some(name))
    }
}

impl FromIterator<(String, String)> for ConfigFiles {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(files: I) -> Self {
        Self {
            files: files.into_iter().collect(),
        }
    }
}

/// Reads the root's `*.cue` files, the repository's `cue.mod/`, the directories of the
/// module's packages that those files import (transitively), and the files they embed with
/// `@embed(file=...)` or `@embed(glob=...)`, within `limits`. Embedded files must be UTF-8 text
/// (JSON, YAML, text), the formats a configuration embeds.
pub async fn load(source: &dyn ConfigSource, limits: Limits) -> Result<ConfigFiles, ConfigError> {
    let mut loader = Loader {
        source,
        limits,
        files: BTreeMap::new(),
        dirs: 0,
        bytes: 0,
    };
    let Some(root) = loader.list("").await? else {
        return Ok(ConfigFiles::default());
    };
    loader.read_cue_files("", &root).await?;
    if root
        .iter()
        .any(|entry| entry.is_dir && entry.name == "cue.mod")
    {
        loader.read_tree("cue.mod").await?;
    }
    let module = loader
        .files
        .get("cue.mod/module.cue")
        .and_then(|text| module_of(text));

    if let Some(module) = module {
        let mut visited: BTreeSet<String> = BTreeSet::from([String::new()]);
        let mut pending: VecDeque<String> = loader.imported_dirs(&module, &visited);
        while let Some(dir) = pending.pop_front() {
            if !visited.insert(dir.clone()) {
                continue;
            }
            if let Some(entries) = loader.list(&dir).await? {
                loader.read_cue_files(&dir, &entries).await?;
            }
            pending.extend(loader.imported_dirs(&module, &visited));
        }
    }
    loader.read_embedded().await?;
    Ok(ConfigFiles {
        files: loader.files,
    })
}

struct Loader<'a> {
    source: &'a dyn ConfigSource,
    limits: Limits,
    files: BTreeMap<String, String>,
    dirs: usize,
    bytes: usize,
}

impl Loader<'_> {
    async fn list(&mut self, dir: &str) -> Result<Option<Vec<DirEntry>>, ConfigError> {
        self.dirs += 1;
        if self.dirs > self.limits.dirs {
            return Err(too_large(format!("{} directories", self.limits.dirs)));
        }
        self.source
            .list(dir)
            .await
            .map_err(|error| ConfigError::Evaluator(format!("{error:#}")))
    }

    async fn read(&mut self, path: String) -> Result<(), ConfigError> {
        if self.files.len() >= self.limits.files {
            return Err(too_large(format!("{} files", self.limits.files)));
        }
        // A file that is missing is left out, so the evaluator reports it as `cue` would.
        let Some(bytes) = self
            .source
            .read(&path)
            .await
            .map_err(|error| ConfigError::Evaluator(format!("{error:#}")))?
        else {
            return Ok(());
        };
        self.bytes += bytes.len();
        if self.bytes > self.limits.bytes {
            return Err(too_large(format!("{} bytes of CUE", self.limits.bytes)));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| ConfigError::Invalid(format!("{path} is not UTF-8")))?;
        self.files.insert(path, text);
        Ok(())
    }

    async fn read_cue_files(&mut self, dir: &str, entries: &[DirEntry]) -> Result<(), ConfigError> {
        for entry in entries {
            if !entry.is_dir && entry.name.ends_with(".cue") {
                self.read(join(dir, &entry.name)).await?;
            }
        }
        Ok(())
    }

    /// Every file under `dir`, recursively.
    async fn read_tree(&mut self, dir: &str) -> Result<(), ConfigError> {
        let mut pending = VecDeque::from([dir.to_owned()]);
        while let Some(dir) = pending.pop_front() {
            let Some(entries) = self.list(&dir).await? else {
                continue;
            };
            for entry in entries {
                let path = join(&dir, &entry.name);
                if entry.is_dir {
                    pending.push_back(path);
                } else if entry.name.ends_with(".cue") {
                    self.read(path).await?;
                }
            }
        }
        Ok(())
    }

    /// Reads the files that loaded package files embed. Paths are relative to the embedding
    /// file's directory; one that leaves the repository is skipped, for the evaluator to refuse.
    async fn read_embedded(&mut self) -> Result<(), ConfigError> {
        let mut wanted: BTreeSet<Embed> = BTreeSet::new();
        for (path, text) in &self.files {
            if path.starts_with("cue.mod/") || !path.ends_with(".cue") {
                continue;
            }
            let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            for embed in embeds_of(text) {
                let (pattern, glob) = match &embed {
                    Embed::File(p) => (p.as_str(), false),
                    Embed::Glob(p) => (p.as_str(), true),
                };
                if let Some(resolved) = resolve(dir, pattern) {
                    wanted.insert(if glob {
                        Embed::Glob(resolved)
                    } else {
                        Embed::File(resolved)
                    });
                }
            }
        }
        for embed in wanted {
            match embed {
                Embed::File(path) => {
                    if !self.files.contains_key(&path) {
                        self.read(path).await?;
                    }
                }
                Embed::Glob(pattern) => {
                    for path in self.glob(&pattern).await? {
                        if !self.files.contains_key(&path) {
                            self.read(path).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The files matching `pattern`, a `/`-separated glob relative to the root whose elements
    /// may use `*`, `?` and `[...]`.
    async fn glob(&mut self, pattern: &str) -> Result<Vec<String>, ConfigError> {
        let elements: Vec<&str> = pattern.split('/').collect();
        let mut dirs = vec![String::new()];
        let mut found = Vec::new();
        for (index, element) in elements.iter().enumerate() {
            let last = index + 1 == elements.len();
            let matcher = glob::Pattern::new(element).ok();
            let mut next = Vec::new();
            for dir in &dirs {
                let Some(entries) = self.list(dir).await? else {
                    continue;
                };
                for entry in entries {
                    let matches = match &matcher {
                        Some(matcher) => matcher.matches(&entry.name),
                        None => entry.name == *element,
                    };
                    if !matches {
                        continue;
                    }
                    let path = join(dir, &entry.name);
                    match (last, entry.is_dir) {
                        (true, false) => found.push(path),
                        (false, true) => next.push(path),
                        _ => {}
                    }
                }
            }
            dirs = next;
        }
        Ok(found)
    }

    /// The module's directories that loaded files import and that are not `visited`.
    fn imported_dirs(&self, module: &str, visited: &BTreeSet<String>) -> VecDeque<String> {
        let prefix = format!("{module}/");
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for (path, text) in &self.files {
            if path.starts_with("cue.mod/") {
                continue;
            }
            for import in imports_of(text) {
                if let Some(dir) = import.strip_prefix(&prefix)
                    && !visited.contains(dir)
                {
                    dirs.insert(dir.to_owned());
                }
            }
        }
        dirs.into_iter().collect()
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn too_large(limit: String) -> ConfigError {
    ConfigError::Invalid(format!("the configuration needs more than {limit}"))
}

static PACKAGE_CLAUSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^package\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid regex"));
static MODULE_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*module:\s*"([^"@]+)"#).expect("valid regex"));
static SINGLE_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\s*import\s+(?:[A-Za-z_][A-Za-z0-9_]*\s+)?"([^"]+)""#).expect("valid regex")
});
static IMPORT_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?ms)^\s*import\s*\((.*?)\)").expect("valid regex"));
static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]+)""#).expect("valid regex"));

/// The package a CUE file declares: the first clause that is not a comment or an attribute.
fn package_of(text: &str) -> Option<String> {
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") || line.starts_with('@') {
            continue;
        }
        return PACKAGE_CLAUSE
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned());
    }
    None
}

/// The module path `cue.mod/module.cue` declares, without a major version suffix.
fn module_of(text: &str) -> Option<String> {
    MODULE_FIELD
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
}

static EMBED_ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@embed\(([^)]*)\)").expect("valid regex"));
static EMBED_ARGUMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(file|glob)\s*=\s*(?:"([^"]*)"|([^,\s)]+))"#).expect("valid regex")
});

/// What an `@embed` attribute names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Embed {
    File(String),
    Glob(String),
}

/// The files and globs a CUE file's `@embed` attributes name, relative to its directory.
fn embeds_of(text: &str) -> Vec<Embed> {
    EMBED_ATTRIBUTE
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .flat_map(|args| EMBED_ARGUMENT.captures_iter(args.as_str()))
        .filter_map(|c| {
            let value = c.get(2).or_else(|| c.get(3))?.as_str().to_owned();
            Some(match c.get(1)?.as_str() {
                "file" => Embed::File(value),
                _ => Embed::Glob(value),
            })
        })
        .collect()
}

/// `path` relative to directory `dir`, as a path from the root; `None` when it is absolute or
/// leaves the root.
fn resolve(dir: &str, path: &str) -> Option<String> {
    if path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The import paths in a CUE file, without `:package` qualifiers.
fn imports_of(text: &str) -> Vec<String> {
    let mut paths: Vec<String> = SINGLE_IMPORT
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
        .collect();
    for block in IMPORT_BLOCK.captures_iter(text).filter_map(|c| c.get(1)) {
        paths.extend(
            QUOTED
                .captures_iter(block.as_str())
                .filter_map(|c| c.get(1))
                .map(|m| m.as_str().to_owned()),
        );
    }
    paths
        .into_iter()
        .map(|path| path.split(':').next().unwrap_or_default().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A repository in memory.
    struct Memory {
        files: BTreeMap<String, String>,
        reads: Mutex<Vec<String>>,
    }

    impl Memory {
        fn new(files: &[(&str, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
                    .collect(),
                reads: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ConfigSource for Memory {
        async fn list(&self, dir: &str) -> anyhow::Result<Option<Vec<DirEntry>>> {
            let prefix = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            let mut entries: BTreeMap<String, bool> = BTreeMap::new();
            for path in self.files.keys() {
                if let Some(rest) = path.strip_prefix(&prefix) {
                    match rest.split_once('/') {
                        Some((name, _)) => entries.insert(name.to_owned(), true),
                        None => entries.insert(rest.to_owned(), false),
                    };
                }
            }
            Ok((!entries.is_empty()).then(|| {
                entries
                    .into_iter()
                    .map(|(name, is_dir)| DirEntry { name, is_dir })
                    .collect()
            }))
        }

        async fn read(&self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
            self.reads
                .lock()
                .expect("never poisoned")
                .push(path.to_owned());
            Ok(self.files.get(path).map(|c| c.clone().into_bytes()))
        }
    }

    fn paths(files: &ConfigFiles) -> Vec<&str> {
        files.iter().map(|(p, _)| p.as_str()).collect()
    }

    #[tokio::test]
    async fn loads_the_root_the_module_and_what_is_imported() {
        let repo = Memory::new(&[
            ("cue.mod/module.cue", "module: \"example.com/acme@v0\"\n"),
            ("cue.mod/pkg/x/x.cue", "package x\n"),
            (
                "tartan.cue",
                "package tartan\nimport \"example.com/acme/config/fields\"\n",
            ),
            ("env.cue", "package cuenv\n"),
            ("README.md", "not cue"),
            (
                "config/fields/fields.cue",
                "package fields\nimport (\n\tb \"example.com/acme/config/base:base\"\n\t\"strings\"\n)\n",
            ),
            ("config/base/base.cue", "package base\n"),
            ("unrelated/other.cue", "package other\n"),
        ]);
        let files = load(&repo, LIMITS).await.expect("loads");
        assert_eq!(
            paths(&files),
            vec![
                "config/base/base.cue",
                "config/fields/fields.cue",
                "cue.mod/module.cue",
                "cue.mod/pkg/x/x.cue",
                "env.cue",
                "tartan.cue",
            ]
        );
        assert!(files.has_root_package("tartan"));
        assert!(
            !repo
                .reads
                .lock()
                .expect("never poisoned")
                .contains(&"unrelated/other.cue".to_owned())
        );
    }

    #[tokio::test]
    async fn loads_embedded_files() {
        let repo = Memory::new(&[
            ("cue.mod/module.cue", "module: \"example.com/acme@v0\"\n"),
            (
                "tartan.cue",
                "@extern(embed)\npackage tartan\na: _ @embed(file=\"config/a.json\")\nb: _ @embed(glob=config/*.yaml, type=yaml)\nc: _ @embed(file=\"../outside.json\")\nd: _ @embed(file=missing.json)\n",
            ),
            ("config/a.json", "{}"),
            ("config/b.yaml", "b: 1\n"),
            ("config/c.yaml", "c: 1\n"),
            ("config/nested/d.yaml", "d: 1\n"),
            ("config/e.json", "{}"),
        ]);
        let files = load(&repo, LIMITS).await.expect("loads");
        assert_eq!(
            paths(&files),
            vec![
                "config/a.json",
                "config/b.yaml",
                "config/c.yaml",
                "cue.mod/module.cue",
                "tartan.cue",
            ]
        );
        assert_eq!(resolve("config", "../a.json").as_deref(), Some("a.json"));
        assert_eq!(resolve("", "../a.json"), None);
        assert_eq!(resolve("", "/etc/passwd"), None);
    }

    #[tokio::test]
    async fn an_empty_repository_has_no_configuration() {
        let files = load(&Memory::new(&[]), LIMITS).await.expect("loads");
        assert!(!files.has_root_package("tartan"));
    }

    #[tokio::test]
    async fn limits_are_enforced() {
        let repo = Memory::new(&[
            ("a.cue", "package tartan\n"),
            ("b.cue", "package tartan\n"),
            ("c.cue", "package tartan\n"),
        ]);
        let few_files = Limits { files: 2, ..LIMITS };
        assert!(load(&repo, few_files).await.is_err());
        let few_bytes = Limits {
            bytes: 20,
            ..LIMITS
        };
        assert!(load(&repo, few_bytes).await.is_err());
        let no_dirs = Limits { dirs: 0, ..LIMITS };
        assert!(load(&repo, no_dirs).await.is_err());
        assert!(load(&repo, LIMITS).await.is_ok());
    }

    #[test]
    fn finds_packages_and_imports() {
        assert_eq!(
            package_of("// c\n@if(x)\npackage tartan\n").as_deref(),
            Some("tartan")
        );
        assert_eq!(package_of("a: 1\n"), None);
        assert_eq!(
            imports_of("import \"a/b\"\nimport x \"c/d:e\"\nimport (\n\t\"f\"\n\ty \"g/h\"\n)\n"),
            vec!["a/b", "c/d", "f", "g/h"]
        );
        assert_eq!(
            module_of("module: \"example.com/acme@v0\"\n").as_deref(),
            Some("example.com/acme")
        );
    }
}
