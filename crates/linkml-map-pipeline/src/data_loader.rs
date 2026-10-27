//! Table matching for the streaming driver.
//!
//! Port of the lookup half of upstream `loaders/data_loaders.py::DataLoader`:
//! which table a `class_derivation` reads, and which file holds it. Parsing the
//! files stays with `linkml-map-io`; this type only answers "is there data for
//! this table, and where".
//!
//! - **Single-file mode** (`base_path` is a file): the file holds exactly one
//!   table, named by `identifier` (`--source-type`) or, failing that, the file
//!   stem. [`DataLoader::contains`] is true only for that name.
//! - **Directory mode**: a table is the file whose name stem equals the table
//!   name, found by trying the extensions in [`DataLoader::EXTENSIONS`] in order.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

/// Resolves `populated_from` table names to data files.
#[derive(Debug, Clone)]
pub struct DataLoader {
    base_path: PathBuf,
    /// Single-file mode only: the source class the file's rows belong to.
    identifier: String,
}

impl DataLoader {
    /// Extensions searched for a table file, in order of preference.
    ///
    /// Upstream's `_find_file` tries `.tsv`, `.csv`, `.yaml`, `.yml`, `.json`.
    /// `.jsonl` is a Rust extension (upstream has no JSONL loader) and comes last,
    /// so it never shadows a table upstream would have found.
    pub const EXTENSIONS: [&'static str; 6] = ["tsv", "csv", "yaml", "yml", "json", "jsonl"];

    /// Open `base_path`, a data file or a directory of tables.
    ///
    /// `identifier` is single-file mode only: the source class the file's rows
    /// belong to, defaulting to the file stem (upstream `DataLoader.__init__`).
    ///
    /// # Errors
    /// `Path not found: …` when `base_path` does not exist, as upstream's
    /// `FileNotFoundError`.
    pub fn new(base_path: impl Into<PathBuf>, identifier: Option<&str>) -> Result<Self> {
        let base_path = base_path.into();
        if !base_path.exists() {
            anyhow::bail!("Path not found: {}", base_path.display());
        }
        let identifier = identifier.map(str::to_owned).unwrap_or_else(|| {
            base_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        Ok(Self {
            base_path,
            identifier,
        })
    }

    /// Whether the loader is configured for single-file mode.
    pub fn is_single_file(&self) -> bool {
        self.base_path.is_file()
    }

    /// Single-file mode: the table the file holds.
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Whether data exists for `identifier` (upstream `__contains__`).
    pub fn contains(&self, identifier: &str) -> bool {
        if self.is_single_file() {
            return identifier == self.identifier;
        }
        self.find_file(identifier).is_some()
    }

    /// The data file for `identifier`: the file itself in single-file mode when
    /// the name matches, else the directory file found by stem. `None` when
    /// there is none. Upstream `__getitem__` / `get_path`.
    pub fn find(&self, identifier: &str) -> Option<PathBuf> {
        if self.is_single_file() {
            return (identifier == self.identifier).then(|| self.base_path.clone());
        }
        self.find_file(identifier)
    }

    /// Upstream `_find_file`: `<dir>/<identifier>.<ext>` for the first extension
    /// that exists. Matching is case-sensitive on the extension, as upstream's.
    fn find_file(&self, identifier: &str) -> Option<PathBuf> {
        Self::EXTENSIONS
            .iter()
            .map(|ext| self.base_path.join(format!("{identifier}.{ext}")))
            .find(|path| path.is_file())
    }

    /// Sorted, de-duplicated stems of the supported files in the directory
    /// (upstream `get_available_identifiers`). Empty in single-file mode. The
    /// extension test is case-insensitive, as upstream's `suffix.lower()`.
    ///
    /// # Errors
    /// The directory cannot be read.
    pub fn get_available_identifiers(&self) -> Result<Vec<String>> {
        if self.is_single_file() {
            return Ok(Vec::new());
        }
        let mut names = BTreeSet::new();
        for entry in std::fs::read_dir(&self.base_path)
            .with_context(|| format!("reading source directory {}", self.base_path.display()))?
        {
            let path = entry
                .with_context(|| format!("reading entry in {}", self.base_path.display()))?
                .path();
            if is_supported_extension(&path)
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.insert(stem.to_owned());
            }
        }
        Ok(names.into_iter().collect())
    }

    /// What the loader has, for the "No input data" message: the single file's
    /// identifier, or the directory's available identifiers.
    ///
    /// # Errors
    /// The directory cannot be read.
    pub fn describe_available(&self) -> Result<Vec<String>> {
        if self.is_single_file() {
            Ok(vec![self.identifier.clone()])
        } else {
            self.get_available_identifiers()
        }
    }
}

fn is_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| DataLoader::EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for f in files {
            std::fs::write(tmp.path().join(f), "").unwrap();
        }
        tmp
    }

    /// `test_data_loaders.py` single-file mode: the identifier defaults to the
    /// stem, `--source-type` overrides it, and only that name is contained.
    #[test]
    fn single_file_identifier_is_the_stem_or_the_override() {
        let tmp = dir_with(&["people.tsv"]);
        let file = tmp.path().join("people.tsv");
        let loader = DataLoader::new(&file, None).unwrap();
        assert!(loader.is_single_file());
        assert!(loader.contains("people") && !loader.contains("Person"));
        assert_eq!(loader.find("people"), Some(file.clone()));
        assert_eq!(loader.find("Person"), None);
        assert_eq!(loader.describe_available().unwrap(), ["people"]);

        let loader = DataLoader::new(&file, Some("Person")).unwrap();
        assert!(loader.contains("Person") && !loader.contains("people"));
        assert_eq!(loader.find("Person"), Some(file));
    }

    /// `_find_file`: `.tsv` beats `.csv` beats `.yaml` …; unknown extensions and
    /// differently-cased extensions are not tables.
    #[test]
    fn directory_prefers_extensions_in_upstream_order() {
        let tmp = dir_with(&[
            "A.json", "A.yaml", "A.csv", "B.txt", "C.TSV", "D.jsonl", "D.json",
        ]);
        let loader = DataLoader::new(tmp.path(), None).unwrap();
        assert!(!loader.is_single_file());
        assert_eq!(loader.find("A"), Some(tmp.path().join("A.csv")));
        assert_eq!(loader.find("D"), Some(tmp.path().join("D.json")));
        assert!(!loader.contains("B"), "unsupported extension");
        assert!(!loader.contains("C"), "extension match is case-sensitive");
        assert!(!loader.contains("nope"));
    }

    /// `get_available_identifiers`: sorted, de-duplicated stems of supported
    /// files; the extension test ignores case (so `C.TSV` is listed, not found).
    #[test]
    fn available_identifiers_are_sorted_stems() {
        let tmp = dir_with(&["b.tsv", "a.csv", "a.json", "c.TSV", "x.txt"]);
        let loader = DataLoader::new(tmp.path(), None).unwrap();
        assert_eq!(loader.get_available_identifiers().unwrap(), ["a", "b", "c"]);
    }

    #[test]
    fn missing_path_is_reported_like_upstream() {
        let err = DataLoader::new("/definitely/not/here.tsv", None).unwrap_err();
        assert_eq!(err.to_string(), "Path not found: /definitely/not/here.tsv");
    }
}
