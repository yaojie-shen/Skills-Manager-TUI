use skills::{Workspace, migration};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Case without an `input` directory: an empty, never-initialized Home.
const FRESH_EMPTY: &str = "fresh-empty";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrations")
}

fn copy_tree(source: &Path, target: &Path) {
    assert!(source.is_dir(), "missing directory {}", source.display());
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
    let input = fixtures().join(case).join("input");
    let root = if local {
        let project = owned.join("project");
        fs::create_dir_all(&project).unwrap();
        project.join(".agents/skills")
    } else {
        owned.clone()
    };
    fs::create_dir_all(&root).unwrap();
    if case == FRESH_EMPTY {
        assert!(!input.exists(), "{case} must not have an input directory");
    } else {
        copy_tree(&input, &root);
    }
    let before = tree(&root);
    let result = if local {
        Workspace::open_local(&owned.join("project"), false)
    } else {
        Workspace::open(&root)
    };
    (root, result, before)
}

/// Opening an already-migrated Home again must change nothing and report nothing.
fn assert_fixed_point(root: &Path, label: &str) {
    let before = tree(root);
    assert!(
        migration::ensure_current(root).unwrap().is_none(),
        "{label}: second open reported a migration"
    );
    assert_eq!(tree(root), before, "{label}: second open changed the Home");
    assert!(
        Workspace::open(root).unwrap().migration.is_none(),
        "{label}: reopening reported a migration"
    );
    assert_eq!(tree(root), before, "{label}: reopening changed the Home");
}

#[test]
fn fixture_matrix_matches_global_and_local_results() {
    for case in [
        "v0-released-v0.1.2",
        "v0-branch-intermediate",
        "v0-current-docs-undeclared",
        "v0-duplicate-tags-union",
        "v0-duplicate-tags-conflict",
        "v1-current",
        "v2-future",
        FRESH_EMPTY,
    ] {
        let mut outcomes = Vec::new();
        for local in [false, true] {
            let (root, result, before) = run_case(case, local);
            if matches!(case, "v2-future" | "v0-duplicate-tags-conflict") {
                let expected =
                    fs::read_to_string(fixtures().join(case).join("expected-error")).unwrap();
                assert!(format!("{:#}", result.unwrap_err()).contains(expected.trim()));
                assert_eq!(tree(&root), before);
                assert!(!root.join(".skills-meta/.metadata.lock").exists());
                outcomes.push(tree(&root));
                continue;
            }
            let workspace = result.unwrap();
            let expected_root = fixtures().join(case).join("expected");
            assert_eq!(tree(&root), tree(&expected_root), "{case} local={local}");
            let backups = root.join(".skills-meta/backups");
            if matches!(
                case,
                "v0-released-v0.1.2" | "v0-branch-intermediate" | "v0-duplicate-tags-union"
            ) {
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
            assert_fixed_point(&root, &format!("{case} local={local}"));
            outcomes.push(tree(&root));
        }
        assert_eq!(outcomes[0], outcomes[1], "global/local mismatch for {case}");
    }
}

#[test]
fn expected_fixture_trees_are_fixed_points() {
    for entry in fs::read_dir(fixtures()).unwrap() {
        let expected = entry.unwrap().path().join("expected");
        if !expected.exists() {
            continue;
        }
        let case = expected.parent().unwrap().file_name().unwrap();
        let case = case.to_string_lossy();
        let temp = skills::ops::DownloadDir::new(&format!("migration-expected-fixed-point-{case}"))
            .unwrap();
        copy_tree(&expected, temp.path());
        // Without the declaration the steps run again over their own output.
        fs::remove_file(temp.path().join(".skills-meta/format.toml")).unwrap();
        assert!(
            migration::ensure_current(temp.path()).unwrap().is_none(),
            "{case}: re-running the steps over expected reported a migration"
        );
        assert_eq!(
            tree(temp.path()),
            tree(&expected),
            "{case}: re-running the steps over expected changed more than format.toml"
        );
        assert_fixed_point(temp.path(), &format!("{case} expected"));
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

#[test]
fn legacy_documents_under_a_declaration_point_to_reupgrading() {
    let (root, first, _) = run_case("v1-current", false);
    first.unwrap();
    let meta = root.join(".skills-meta");
    // A legacy document arriving later, e.g. through root sync from an older build.
    fs::create_dir_all(meta.join("tags")).unwrap();
    fs::write(
        meta.join("tags/synced.toml"),
        "name = 'synced'\nskills = ['one']\n",
    )
    .unwrap();
    let error = format!("{:#}", Workspace::open(&root).unwrap_err());
    assert!(
        error.contains("uses legacy Tag schema 0, but this Skills Manager expects Tag schema 1"),
        "{error}"
    );
    assert!(
        error.contains("delete .skills-meta/format.toml if it exists and reopen"),
        "{error}"
    );
    assert!(!error.contains("declares layout"), "{error}");
    assert!(!error.contains("copy the originals"), "{error}");

    fs::remove_file(migration::version::path(&root)).unwrap();
    let reopened = Workspace::open(&root).unwrap();
    assert!(reopened.migration.unwrap().backup_dir.is_dir());
    assert!(reopened.tags.load("synced").unwrap().is_some());
    assert_fixed_point(&root, "re-upgraded");
}

#[test]
fn legacy_config_error_names_the_expected_schema_and_the_file_once() {
    let (root, first, _) = run_case("v1-current", false);
    first.unwrap();
    let config = root.join(".skills-meta/config.toml");
    fs::write(&config, "schema = 1\n").unwrap();
    let error = format!("{:#}", Workspace::open(&root).unwrap_err());
    assert!(
        error.contains(
            "uses legacy config schema 1, but this Skills Manager expects config schema 2"
        ),
        "{error}"
    );
    assert!(error.contains("delete .skills-meta/format.toml"), "{error}");
    assert_eq!(
        error.matches(&config.display().to_string()).count(),
        1,
        "{error}"
    );
}
