use crate::{ApalacheSpec, Error};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const BUILTINS: &[&str] = &[
    "Naturals",
    "Integers",
    "Reals",
    "Sequences",
    "FiniteSets",
    "TLC",
    "Bags",
    "Apalache",
];

fn source_error(message: impl Into<String>) -> Error {
    Error::SpecSource(message.into())
}

/// Tokenize identifiers and commas outside strings and TLA+ comments.
fn dependency_tokens(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut block_depth = 0usize;
    while i < bytes.len() {
        if block_depth > 0 {
            if bytes.get(i..i + 2) == Some(b"(*") {
                block_depth += 1;
                i += 2;
            } else if bytes.get(i..i + 2) == Some(b"*)") {
                block_depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"\\*") {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"(*") {
            block_depth = 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i] == b',' {
            tokens.push(",".to_string());
            i += 1;
            continue;
        }
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(source[start..i].to_string());
            continue;
        }
        i += 1;
    }
    tokens
}

fn import_names(source: &str) -> Vec<String> {
    let tokens = dependency_tokens(source);
    let mut names = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "EXTENDS" if i + 1 < tokens.len() && tokens[i + 1] != "," => {
                names.push(tokens[i + 1].clone());
                i += 2;
                while i + 1 < tokens.len() && tokens[i] == "," && tokens[i + 1] != "," {
                    names.push(tokens[i + 1].clone());
                    i += 2;
                }
            }
            "INSTANCE" if i + 1 < tokens.len() && tokens[i + 1] != "," => {
                names.push(tokens[i + 1].clone());
                i += 2;
            }
            _ => i += 1,
        }
    }
    names.retain(|name| !BUILTINS.contains(&name.as_str()));
    names
}

fn resolve_module(
    import_dir: &Path,
    name: &str,
    search_dirs: &[PathBuf],
) -> Result<PathBuf, Error> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for dir in std::iter::once(import_dir).chain(search_dirs.iter().map(PathBuf::as_path)) {
        let candidate = dir.join(format!("{name}.tla"));
        if !candidate.exists() {
            continue;
        }
        let canonical = fs::canonicalize(&candidate).map_err(|error| {
            source_error(format!("cannot resolve {}: {error}", candidate.display()))
        })?;
        if seen.insert(canonical.clone()) {
            candidates.push(canonical);
        }
    }
    match candidates.as_slice() {
        [] => Err(source_error(format!(
            "module {name:?} imported from {} was not found",
            import_dir.display()
        ))),
        [only] => Ok(only.clone()),
        _ => Err(source_error(format!(
            "module {name:?} is ambiguous: {}",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub fn spec_from_file(root: impl AsRef<Path>) -> Result<ApalacheSpec, Error> {
    spec_from_files::<PathBuf>(root, &[])
}

/// Read a root TLA+ module and its complete EXTENDS/INSTANCE closure.
/// Sources are breadth-first with the root first and canonical-path deduplication.
pub fn spec_from_files<P: AsRef<Path>>(
    root: impl AsRef<Path>,
    search_dirs: &[P],
) -> Result<ApalacheSpec, Error> {
    let search_dirs: Vec<PathBuf> = if search_dirs.is_empty() {
        std::env::var_os("TLA_LIBRARY_PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default()
    } else {
        search_dirs
            .iter()
            .map(|path| path.as_ref().to_path_buf())
            .collect()
    };
    let root = fs::canonicalize(root.as_ref()).map_err(|error| {
        source_error(format!(
            "cannot resolve root {}: {error}",
            root.as_ref().display()
        ))
    })?;
    let root_source = fs::read_to_string(&root)
        .map_err(|error| source_error(format!("cannot read {}: {error}", root.display())))?;

    let mut sources = vec![root_source.clone()];
    let mut visited = HashSet::from([root.clone()]);
    let mut queue = VecDeque::new();
    let root_dir = root.parent().unwrap_or(Path::new(".")).to_path_buf();
    for name in import_names(&root_source) {
        queue.push_back((root_dir.clone(), name));
    }

    while let Some((import_dir, name)) = queue.pop_front() {
        let path = resolve_module(&import_dir, &name, &search_dirs)?;
        if !visited.insert(path.clone()) {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| source_error(format!("cannot read {}: {error}", path.display())))?;
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        for dependency in import_names(&source) {
            queue.push_back((dir.clone(), dependency));
        }
        sources.push(source);
    }

    Ok(ApalacheSpec { sources })
}

#[cfg(test)]
mod tests {
    use super::{import_names, BUILTINS};

    #[test]
    fn scanner_handles_continued_clauses_and_skips_comments_and_strings() {
        let source = r#"
            EXTENDS
              A,
              B
            Text == "EXTENDS Fake INSTANCE Fake2"
            (* EXTENDS Comment (* INSTANCE Nested *) *)
            \* INSTANCE Line
            Op == INSTANCE C WITH x <- 1
        "#;
        assert_eq!(import_names(source), ["A", "B", "C"]);
        assert!(BUILTINS.contains(&"Integers"));
    }
}
