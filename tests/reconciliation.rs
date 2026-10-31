use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use symlinker::plan;
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    config: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let config = bin.join("symlinker.links");
        fs::create_dir(&bin).unwrap();

        Self { root, config, bin }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, "keep").unwrap();

        path
    }

    fn manifest(&self, links: &[(&str, &str)]) {
        let links = links
            .iter()
            .map(|(name, target)| format!("{name} = {target}\n"))
            .collect::<String>();
        fs::write(&self.config, links).unwrap();
    }

    fn cli(&self, command: &str) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_symlinker"))
            .arg(&self.config)
            .arg(command)
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap();
        assert_success(&output);

        String::from_utf8(output.stdout).unwrap()
    }
}

fn link(target: &Path, path: &Path, directory: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        if directory {
            std::os::windows::fs::symlink_dir(target, path)
        } else {
            std::os::windows::fs::symlink_file(target, path)
        }
    }
    #[cfg(unix)]
    {
        let _ = directory;
        std::os::unix::fs::symlink(target, path)
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn creates_links_and_preserves_equivalent_targets_idempotently() {
    let fixture = Fixture::new();
    let source = fixture.file("source with spaces.exe");
    fs::create_dir(fixture.path("scripts")).unwrap();
    fs::write(fixture.path("scripts/child"), "keep").unwrap();
    let relative_name = if cfg!(windows) {
        "Relative"
    } else {
        "relative"
    };
    link(
        Path::new("../source with spaces.exe"),
        &fixture.bin.join(relative_name),
        false,
    )
    .unwrap();
    fixture.manifest(&[
        ("tool_1+.exe", "../source with spaces.exe"),
        ("scripts", "../scripts"),
        ("relative", source.to_str().unwrap()),
    ]);

    fixture.cli("apply");
    assert_eq!(fs::read(fixture.bin.join("tool_1+.exe")).unwrap(), b"keep");
    assert_eq!(
        fs::read(fixture.bin.join("scripts/child")).unwrap(),
        b"keep"
    );
    assert!(fixture.cli("plan").is_empty());
    assert!(fixture.cli("apply").is_empty());
    assert_eq!(
        fs::read_link(fixture.bin.join("relative")).unwrap(),
        Path::new("../source with spaces.exe")
    );
}

#[test]
fn plans_read_only_then_replaces_and_prunes_only_immediate_symlinks() {
    let fixture = Fixture::new();
    let source = fixture.file("source");
    let wrong = fixture.file("wrong");
    let directory = fixture.path("directory");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("child"), "keep").unwrap();
    for name in ["to-dir", "wrong", "obsolete"] {
        link(&wrong, &fixture.bin.join(name), false).unwrap();
    }
    for name in ["to-file", "obsolete-dir"] {
        link(&directory, &fixture.bin.join(name), true).unwrap();
    }
    for (name, is_dir) in [
        ("broken", false),
        ("broken-file", false),
        ("broken-dir", true),
    ] {
        link(&fixture.path("missing"), &fixture.bin.join(name), is_dir).unwrap();
    }
    fs::write(fixture.bin.join("real-file"), "keep").unwrap();
    fs::create_dir(fixture.bin.join("real-dir")).unwrap();
    link(&source, &fixture.bin.join("real-dir/nested"), false).unwrap();
    fixture.manifest(&[
        ("to-dir", "../directory"),
        ("to-file", "../source"),
        ("wrong", "../source"),
        ("broken", "../source"),
    ]);

    let names = ["broken", "obsolete", "to-dir", "to-file", "wrong"];
    let before = names.map(|name| fs::read_link(fixture.bin.join(name)).unwrap());
    let text = fixture.cli("plan");
    assert_eq!(text, fixture.cli("plan"));
    assert!(text.contains("replace: broken ->"));
    assert!(text.contains("remove: obsolete-dir"));
    assert!(text.find("broken").unwrap() < text.find("to-dir").unwrap());
    for (name, target) in names.into_iter().zip(before) {
        assert_eq!(fs::read_link(fixture.bin.join(name)).unwrap(), target);
    }

    plan(&fixture.config).unwrap().apply().unwrap();
    assert!(fixture.bin.join("to-dir").is_dir());
    for name in ["to-file", "wrong", "broken"] {
        assert_eq!(fs::read(fixture.bin.join(name)).unwrap(), b"keep");
    }
    for name in ["obsolete", "obsolete-dir", "broken-file", "broken-dir"] {
        assert!(fs::symlink_metadata(fixture.bin.join(name)).is_err());
    }
    for path in [
        wrong,
        directory.join("child"),
        fixture.bin.join("real-file"),
        fixture.bin.join("real-dir/nested"),
    ] {
        assert_eq!(fs::read(path).unwrap(), b"keep");
    }
}

#[test]
fn real_entry_conflicts_fail_during_planning() {
    let fixture = Fixture::new();
    fixture.file("source");
    fs::write(fixture.bin.join("file"), "keep").unwrap();
    fs::create_dir(fixture.bin.join("directory")).unwrap();
    fixture.manifest(&[("file", "../source"), ("a-new", "../source")]);
    assert!(plan(&fixture.config).is_err());
    fixture.manifest(&[("directory", "../source"), ("a-new", "../source")]);
    assert!(plan(&fixture.config).is_err());
    assert!(!fixture.bin.join("a-new").exists());
    assert_eq!(fs::read(fixture.bin.join("file")).unwrap(), b"keep");
    assert!(fixture.bin.join("directory").is_dir());
}

#[test]
fn rejects_invalid_links_with_line_numbers() {
    let fixture = Fixture::new();
    fixture.file("file");
    for (entry, message) in [
        ("../outside = ../file", "invalid link name"),
        ("tool~1 = ../file", "invalid link name"),
        ("tool. = ../file", "invalid link name"),
        (" = ../file", "invalid link name"),
        ("tool = \t", "target must not be empty"),
        ("tool = ../fi\0le", "NUL characters"),
        ("tool", "expected name = target"),
        ("[destination]", "expected name = target"),
        ("tool = ../file\ntool = ../file", "duplicate link name"),
    ] {
        fs::write(&fixture.config, format!("# Links\n\n{entry}\n")).unwrap();
        let error = match plan(&fixture.config) {
            Ok(_) => panic!("accepted {entry:?}"),
            Err(error) => format!("{error:#}"),
        };
        let line_number = if entry.contains('\n') { 4 } else { 3 };
        assert!(error.contains(&format!("line {line_number}")), "{error}");
        assert!(error.contains(message), "{error}");
        assert!(!fixture.bin.join("tool").exists());
    }
}

#[test]
fn rejects_unsafe_import_sources_and_symlink_destinations() {
    let fixture = Fixture::new();
    let file = fixture.file("file");
    let root = fixture.root.path().ancestors().last().unwrap();
    let alias = fixture.path("alias");
    link(&fixture.bin, &alias, true).unwrap();
    for source in [
        file,
        fixture.path("missing"),
        root.to_path_buf(),
        alias.clone(),
    ] {
        assert!(
            symlinker::import(&source, &fixture.config).is_err(),
            "accepted {source:?}"
        );
        assert!(!fixture.config.exists());
    }

    fixture.manifest(&[]);
    assert!(plan(&alias.join("symlinker.links")).is_err());
}

#[test]
fn import_round_trips_relative_and_broken_links_without_overwriting() {
    let fixture = Fixture::new();
    let source = fixture.file("source with spaces");
    fs::create_dir(fixture.path("scripts")).unwrap();
    link(&source, &fixture.bin.join("z.exe"), false).unwrap();
    let relative = Path::new("../source with spaces");
    link(relative, &fixture.bin.join("a.exe"), false).unwrap();
    link(&fixture.path("scripts"), &fixture.bin.join("scripts"), true).unwrap();
    fs::write(fixture.bin.join("real-file"), "keep").unwrap();
    fs::create_dir(fixture.bin.join("real-dir")).unwrap();
    link(&source, &fixture.bin.join("real-dir/nested"), false).unwrap();
    fs::create_dir(fixture.path("manifests")).unwrap();
    let config = fixture.path("manifests/imported.links");

    let output = Command::new(env!("CARGO_BIN_EXE_symlinker"))
        .arg(&config)
        .arg("import")
        .arg(&fixture.bin)
        .output()
        .unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Imported 3 symlinks"));
    let text = fs::read_to_string(&config).unwrap();
    assert_eq!(
        text,
        format!(
            "a.exe = {}\nscripts = {}\nz.exe = {}\n",
            fixture.bin.join(relative).display(),
            fixture.path("scripts").display(),
            source.display()
        )
    );
    assert_eq!(symlinker::import(&fixture.bin, &fixture.config).unwrap(), 3);
    assert_eq!(fs::read_to_string(&fixture.config).unwrap(), text);
    assert!(fixture.cli("plan").is_empty());
    plan(&config).unwrap().apply().unwrap();
    for name in ["a.exe", "z.exe"] {
        assert_eq!(
            fs::read(fixture.path("manifests").join(name)).unwrap(),
            b"keep"
        );
    }
    assert!(fixture.path("manifests/scripts").is_dir());
    let mut remaining = Vec::new();
    plan(&config).unwrap().display(&mut remaining).unwrap();
    assert!(remaining.is_empty());
    assert_eq!(fs::read_link(fixture.bin.join("a.exe")).unwrap(), relative);
    assert!(symlinker::import(&fixture.bin, &config).is_err());
    assert_eq!(fs::read_to_string(&config).unwrap(), text);

    let missing = Path::new("../missing");
    for (name, is_dir) in [("broken-file", false), ("broken-dir", true)] {
        link(missing, &fixture.bin.join(name), is_dir).unwrap();
    }
    let config = fixture.path("broken.links");
    assert_eq!(symlinker::import(&fixture.bin, &config).unwrap(), 5);
    let imported = fs::read_to_string(&config).unwrap();
    for name in ["broken-file", "broken-dir"] {
        assert!(imported.contains(&format!(
            "{name} = {}\n",
            fixture.bin.join(missing).display()
        )));
    }
    assert!(plan(&config).is_err());
}

#[test]
fn parses_comments_whitespace_and_literal_target_characters() {
    let fixture = Fixture::new();
    let source = fixture.file("source = # with spaces.exe");
    fs::write(
        &fixture.config,
        " \t# Executables\r\n\r\n \ttool.exe \t= ../source = # with spaces.exe \t\r\n",
    )
    .unwrap();

    fixture.cli("apply");
    assert_eq!(
        fs::canonicalize(fixture.bin.join("tool.exe")).unwrap(),
        fs::canonicalize(source).unwrap()
    );
    assert!(fixture.cli("plan").is_empty());
}

#[test]
fn creates_and_imports_mixed_case_names_preserving_their_spelling() {
    let fixture = Fixture::new();
    fixture.file("Source.EXE");
    fs::create_dir(fixture.path("Scripts")).unwrap();
    fixture.manifest(&[
        ("Tool.EXE", "../Source.EXE"),
        ("BuildScripts", "../Scripts"),
    ]);

    fixture.cli("apply");
    assert_eq!(fs::read(fixture.bin.join("Tool.EXE")).unwrap(), b"keep");
    assert!(fixture.bin.join("BuildScripts").is_dir());
    assert!(fixture.cli("plan").is_empty());

    let imported = fixture.path("imported.links");
    assert_eq!(symlinker::import(&fixture.bin, &imported).unwrap(), 2);
    let text = fs::read_to_string(imported).unwrap();
    assert!(text.starts_with("BuildScripts = "));
    assert!(text.contains("\nTool.EXE = "));
}

#[test]
fn case_only_name_collisions_follow_platform_rules() {
    let fixture = Fixture::new();
    fs::write(fixture.path("first"), "first").unwrap();
    fs::write(fixture.path("second"), "second").unwrap();
    fixture.manifest(&[("Tool.EXE", "../first"), ("tool.exe", "../second")]);

    if cfg!(windows) {
        let output = Command::new(env!("CARGO_BIN_EXE_symlinker"))
            .arg(&fixture.config)
            .arg("apply")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("duplicate link names"), "{error}");
        assert!(error.contains("Tool.EXE"), "{error}");
        assert!(error.contains("tool.exe"), "{error}");
        assert!(!fixture.bin.join("Tool.EXE").exists());
    } else {
        fixture.cli("apply");
        assert_eq!(fs::read(fixture.bin.join("Tool.EXE")).unwrap(), b"first");
        assert_eq!(fs::read(fixture.bin.join("tool.exe")).unwrap(), b"second");
        assert!(fixture.cli("plan").is_empty());
    }
}

#[cfg(windows)]
#[test]
fn matches_existing_entries_regardless_of_ascii_case() {
    let fixture = Fixture::new();
    let wrong = fixture.file("wrong");
    let source = fixture.file("source");
    link(&wrong, &fixture.bin.join("tool.exe"), false).unwrap();
    fixture.manifest(&[("Tool.EXE", "../source")]);

    let text = fixture.cli("apply");
    assert!(text.contains("replace: Tool.EXE ->"));
    assert_eq!(
        fs::canonicalize(fixture.bin.join("tool.exe")).unwrap(),
        fs::canonicalize(source).unwrap()
    );
    assert!(fixture.cli("plan").is_empty());

    fs::write(fixture.bin.join("real.exe"), "keep").unwrap();
    fixture.manifest(&[("REAL.EXE", "../source")]);
    assert!(plan(&fixture.config).is_err());
    assert_eq!(fs::read(fixture.bin.join("real.exe")).unwrap(), b"keep");
    assert!(fixture.bin.join("tool.exe").exists());
}

#[test]
fn accepts_an_explicit_bare_filename_in_the_current_directory() {
    let fixture = Fixture::new();
    fixture.file("source");
    fixture.manifest(&[("tool", "../source")]);

    let output = Command::new(env!("CARGO_BIN_EXE_symlinker"))
        .args(["symlinker.links", "apply"])
        .current_dir(&fixture.bin)
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(fs::read(fixture.bin.join("tool")).unwrap(), b"keep");
}

#[test]
fn requires_an_explicit_file_and_import_source_without_default_lookups() {
    let fixture = Fixture::new();
    fixture.manifest(&[]);
    fs::write(fixture.bin.join("symlinker.toml"), "").unwrap();

    for args in [
        vec!["plan"],
        vec!["apply"],
        vec!["import"],
        vec!["import", "."],
        vec!["symlinker.links", "import"],
        vec!["missing.links", "plan"],
        vec!["missing.links", "apply"],
        vec!["-c", "symlinker.links", "plan"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_symlinker"))
            .args(&args)
            .current_dir(&fixture.bin)
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {args:?}");
    }

    for arg in ["--help", "--version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_symlinker"))
            .arg(arg)
            .output()
            .unwrap();
        assert_success(&output);
    }
    assert_eq!(fs::read_to_string(&fixture.config).unwrap(), "");
}
