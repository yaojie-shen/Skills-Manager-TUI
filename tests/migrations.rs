use skills::{Workspace, migration};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn copy_tree(source: &Path, target: &Path) {
    if !source.exists() {
        return;
    }
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let relative = entry.path().strip_prefix(base).unwrap().to_path_buf();
            if relative.starts_with(".skills-meta/backups")
                || relative == Path::new(".skills-meta/.metadata.lock")
            {
                continue;
            }
            assert!(!entry.file_name().to_string_lossy().contains(".tmp"));
            if entry.file_type().unwrap().is_dir() {
                visit(base, &entry.path(), out);
            } else {
                out.insert(relative, fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn run_case(
    case: &str,
    local: bool,
) -> (
    PathBuf,
    anyhow::Result<Workspace>,
    BTreeMap<PathBuf, Vec<u8>>,
) {
    let temp = skills::ops::DownloadDir::new(&format!("migration-fixture-{case}-{local}")).unwrap();
    let owned = temp.path().to_path_buf();
    std::mem::forget(temp);
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/migrations")
        .join(case)
        .join("input");
    let root = if local {
        let project = owned.join("project");
        fs::create_dir_all(&project).unwrap();
        let root = project.join(".agents/skills");
        fs::create_dir_all(&root).unwrap();
        copy_tree(&input, &root);
        root
    } else {
        copy_tree(&input, &owned);
        owned.clone()
    };
    let before = tree(&root);
    let result = if local {
        Workspace::open_local(&owned.join("project"), false)
    } else {
        Workspace::open(&root)
    };
    (root, result, before)
}

#[test]
fn fixture_matrix_matches_global_and_local_results() {
    for case in [
        "v0-released-v0.1.2",
        "v0-branch-intermediate",
        "v0-current-docs-undeclared",
        "v1-current",
        "v2-future",
        "fresh-empty",
    ] {
        let mut outcomes = Vec::new();
        for local in [false, true] {
            let (root, result, before) = run_case(case, local);
            if case == "v2-future" {
                let expected = fs::read_to_string(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/migrations")
                        .join(case)
                        .join("expected-error"),
                )
                .unwrap();
                assert!(format!("{:#}", result.unwrap_err()).contains(expected.trim()));
                assert_eq!(tree(&root), before);
                assert!(!root.join(".skills-meta/.metadata.lock").exists());
                outcomes.push(tree(&root));
                continue;
            }
            let workspace = result.unwrap();
            let expected_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/migrations")
                .join(case)
                .join("expected");
            assert_eq!(tree(&root), tree(&expected_root), "{case} local={local}");
            let backups = root.join(".skills-meta/backups");
            if matches!(case, "v0-released-v0.1.2" | "v0-branch-intermediate") {
                let backup = fs::read_dir(backups)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                for (relative, bytes) in tree(&backup) {
                    assert_eq!(
                        before.get(&Path::new(".skills-meta").join(&relative)),
                        Some(&bytes)
                    );
                }
                assert!(workspace.migration.is_some());
            } else {
                assert!(!backups.exists());
                assert!(workspace.migration.is_none());
            }
            outcomes.push(tree(&root));
        }
        assert_eq!(outcomes[0], outcomes[1], "global/local mismatch for {case}");
    }
}

#[test]
fn declare_only_skips_a_busy_library_guard() {
    let (root, _, _) = run_case("v0-current-docs-undeclared", false);
    fs::remove_file(root.join(".skills-meta/format.toml")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        root.join(".git/skills-sync.lock"),
        format!("{}\ttest\n", std::process::id()),
    )
    .unwrap();
    assert!(Workspace::open(&root).is_ok());
    assert!(!migration::version::path(&root).exists());
}

#[test]
fn restoring_backup_and_removing_declaration_reruns_detection() {
    let (root, first, _) = run_case("v0-released-v0.1.2", false);
    let first = first.unwrap();
    let expected = tree(&root);
    let backup = first.migration.unwrap().backup_dir;
    copy_tree(&backup, &root.join(".skills-meta"));
    fs::remove_file(migration::version::path(&root)).unwrap();
    Workspace::open(&root).unwrap();
    assert_eq!(tree(&root), expected);
}
