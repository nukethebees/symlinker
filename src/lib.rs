use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
use std::os::windows::fs::{FileTypeExt, MetadataExt, symlink_dir, symlink_file};

use anyhow::{Context, Result, ensure};

enum Operation {
    Create(PathBuf),
    Replace(PathBuf),
    Remove,
}

struct PlannedOperation {
    name: OsString,
    operation: Operation,
}

pub struct Plan {
    destination: PathBuf,
    operations: Vec<PlannedOperation>,
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && !name.ends_with('.')
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-+".contains(&c)),
        "invalid link name {name:?}: use ASCII letters, digits, ., _, -, +; no trailing dot"
    );

    Ok(())
}

fn validate_target(target: &str) -> Result<()> {
    ensure!(!target.is_empty(), "target must not be empty");
    ensure!(
        target.trim() == target && !target.contains(['\r', '\n', '\0']),
        "target must fit on one line without surrounding whitespace or NUL characters"
    );

    Ok(())
}

fn insert_link(links: &mut BTreeMap<String, PathBuf>, name: &str, target: PathBuf) -> Result<()> {
    validate_name(name)?;
    for existing in links.keys() {
        ensure!(
            !names_equal(OsStr::new(existing), OsStr::new(name)),
            "duplicate link names {existing:?} and {name:?}"
        );
    }
    links.insert(name.to_owned(), target);

    Ok(())
}

fn parse_links(text: &str) -> Result<BTreeMap<String, PathBuf>> {
    let mut links = BTreeMap::new();

    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line_number = index + 1;
        let (name, target) = line
            .split_once('=')
            .with_context(|| format!("line {line_number}: expected name = target"))?;
        let name = name.trim();
        let target = target.trim();
        validate_target(target).with_context(|| format!("line {line_number}"))?;
        insert_link(&mut links, name, PathBuf::from(target))
            .with_context(|| format!("line {line_number}"))?;
    }

    Ok(links)
}

fn names_equal(left: &OsStr, right: &OsStr) -> bool {
    if cfg!(windows) {
        left.as_encoded_bytes()
            .eq_ignore_ascii_case(right.as_encoded_bytes())
    } else {
        left == right
    }
}

fn resolve_destination(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect destination {}", path.display()))?;

    #[cfg(windows)]
    let is_link = metadata.file_attributes() & 0x400 != 0;
    #[cfg(not(windows))]
    let is_link = metadata.is_symlink();

    ensure!(
        metadata.is_dir() && !is_link,
        "destination must be an existing real directory: {}",
        path.display()
    );

    let destination = fs::canonicalize(path)?;
    ensure!(
        destination.parent().is_some(),
        "filesystem root cannot be a destination"
    );

    Ok(destination)
}

/// Snapshot immediate symlinks, including broken targets, into a new links file.
pub fn import(directory: &Path, links_path: &Path) -> Result<usize> {
    let directory = std::path::absolute(directory)?;
    resolve_destination(&directory)?;
    let mut links = BTreeMap::new();

    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let target = directory.join(fs::read_link(entry.path())?);
            validate_target(target.to_str().context("target path must be valid UTF-8")?)?;
            insert_link(&mut links, &name, target)?;
        }
    }

    let count = links.len();
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(links_path)
        .with_context(|| format!("cannot create links file {}", links_path.display()))?;
    for (name, target) in links {
        writeln!(output, "{name} = {}", target.display())?;
    }

    Ok(count)
}

/// Inspect immediate children without writing; fail on the first invalid entry.
pub fn plan(links_path: &Path) -> Result<Plan> {
    let links_path = std::path::absolute(links_path)?;
    let text = fs::read_to_string(&links_path)
        .with_context(|| format!("cannot read links file {}", links_path.display()))?;
    let links = parse_links(&text)
        .with_context(|| format!("invalid links file {}", links_path.display()))?;
    let base = links_path.parent().unwrap();
    let destination = resolve_destination(base)?;
    let mut entries = fs::read_dir(&destination)?.collect::<io::Result<Vec<_>>>()?;
    let mut operations = Vec::new();

    for (name, value) in links {
        let target = base.join(value);
        let canonical = fs::canonicalize(&target)
            .with_context(|| format!("cannot resolve target {}", target.display()))?;
        let existing = entries
            .iter()
            .position(|entry| names_equal(&entry.file_name(), OsStr::new(&name)));
        let operation = if let Some(index) = existing {
            let entry = entries.swap_remove(index);
            let kind = entry.file_type()?;
            ensure!(kind.is_symlink(), "real entry conflicts with link {name:?}");

            let same_target = fs::read_link(entry.path())
                .and_then(|target| fs::canonicalize(destination.join(target)))
                .is_ok_and(|existing| names_equal(existing.as_os_str(), canonical.as_os_str()));
            #[cfg(windows)]
            let same_target =
                same_target && kind.is_symlink_dir() == fs::metadata(&canonical)?.is_dir();

            if same_target {
                continue;
            }
            Operation::Replace(target)
        } else {
            Operation::Create(target)
        };

        operations.push(PlannedOperation {
            name: name.into(),
            operation,
        });
    }

    for entry in entries {
        if entry.file_type()?.is_symlink() {
            operations.push(PlannedOperation {
                name: entry.file_name(),
                operation: Operation::Remove,
            });
        }
    }
    operations.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(Plan {
        destination,
        operations,
    })
}

fn create_link(target: &Path, link: &Path) -> Result<()> {
    #[cfg(windows)]
    let result = if fs::metadata(target)?.is_dir() {
        symlink_dir(target, link)
    } else {
        symlink_file(target, link)
    };
    #[cfg(unix)]
    let result = symlink(target, link);

    result.with_context(|| format!("cannot create symlink {}", link.display()))
}

fn remove_link(link: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(link)?;
    ensure!(
        metadata.is_symlink(),
        "refusing to remove non-symlink {}",
        link.display()
    );

    #[cfg(windows)]
    if metadata.file_type().is_symlink_dir() {
        return fs::remove_dir(link)
            .with_context(|| format!("cannot remove directory symlink {}", link.display()));
    }

    fs::remove_file(link).with_context(|| format!("cannot remove symlink {}", link.display()))
}

impl Plan {
    pub fn display(&self, output: &mut impl Write) -> io::Result<()> {
        for op in &self.operations {
            let (action, target) = match &op.operation {
                Operation::Create(target) => ("create", Some(target)),
                Operation::Replace(target) => ("replace", Some(target)),
                Operation::Remove => ("remove", None),
            };
            write!(output, "{action}: {}", op.name.to_string_lossy())?;
            if let Some(target) = target {
                write!(output, " -> {}", target.display())?;
            }
            writeln!(output)?;
        }

        Ok(())
    }

    /// Execute in order, stopping on the first error. Requires exclusive access.
    pub fn apply(&self) -> Result<()> {
        for op in &self.operations {
            let link = self.destination.join(&op.name);
            match &op.operation {
                Operation::Create(target) => create_link(target, &link)?,
                Operation::Replace(target) => {
                    remove_link(&link)?;
                    create_link(target, &link)?;
                }
                Operation::Remove => remove_link(&link)?,
            }
        }

        Ok(())
    }
}
