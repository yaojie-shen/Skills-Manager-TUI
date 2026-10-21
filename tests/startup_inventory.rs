use skills::config::Config;
use skills::meta::Source;
use skills::ops::DownloadDir;
use skills::reconcile::{SkillStatus, inventory};
use std::path::{Path, PathBuf};

struct Fixture {
    temp: DownloadDir,
    config: Config,
}

impl Fixture {
    fn new(label: &str) -> Self {
        Self {
            temp: DownloadDir::new(label).unwrap(),
            config: Config {
                agents: vec![],
                ..Default::default()
            },
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn metadata_path(&self, name: &str) -> PathBuf {
        self.root().join(".skills-meta/repos").join(name)
    }

    fn write_metadata(&self, name: &str, text: &str) {
        let path = self.metadata_path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn write_skill(&self, key: &str, name: &str) {
        let path = self.root().join(key);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name}\n---\nBody\n"),
        )
        .unwrap();
    }

    fn scan(&self) -> anyhow::Result<skills::reconcile::Snapshot> {
        inventory(self.root(), &self.config)
    }
}

#[test]
fn repository_documents_keep_identity_entry_errors_and_source_inheritance() {
    let fixture = Fixture::new("startup-inventory-document");
    fixture.write_skill("repos/demo/alpha", "alpha");
    fixture.write_skill("repos/package/archive", "archive");
    fixture.write_metadata(
        "demo.toml",
        r#"schema = 1
alias = "demo"
name = "Demo tools"
url = "https://example.com/demo.git"
kind = "git"
branch = "stable"

[skills.alpha]
note = "alpha note"
[skills.alpha.source]
type = "git"
subpath = "skills/alpha"
revision = "rev-alpha"
[skills.alpha.baseline]
hash = "baseline-alpha"
hash_algo = 1

[skills.broken]
note = 17
[skills.broken.source]
type = "git"
subpath = "skills/broken"

[skills.wrong-kind.source]
type = "archive"
subpath = "skills/wrong-kind"

[skills.missing]
[skills.missing.source]
type = "git"
subpath = "skills/missing"
revision = "rev-missing"
[skills.missing.baseline]
hash = "baseline-missing"
hash_algo = 1
"#,
    );
    fixture.write_metadata(
        "package.toml",
        r#"schema = 1
alias = "package"
name = "Package tools"
url = "https://example.com/tools.zip"
kind = "archive"

[skills.archive.source]
type = "archive"
subpath = "nested/archive"
revision = "sha256:archive"
"#,
    );

    let listed = skills::repository::Repository::list(fixture.root()).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|repository| repository.alias.as_str())
            .collect::<Vec<_>>(),
        ["demo", "package"],
        "repository identity listing must ignore corrupt per-skill entries"
    );

    let snapshot = fixture.scan().unwrap();
    assert_eq!(
        snapshot
            .repositories
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["demo", "package"]
    );
    let demo = &snapshot.repositories["demo"];
    assert_eq!(demo.display_name(), "Demo tools");
    assert_eq!(demo.url, "https://example.com/demo.git");
    assert_eq!(demo.branch, "stable");

    let alpha = snapshot.get("repos/demo/alpha").unwrap();
    assert_eq!(alpha.note.as_deref(), Some("alpha note"));
    assert_eq!(alpha.baseline_hash.as_deref(), Some("baseline-alpha"));
    assert_eq!(alpha.status, SkillStatus::Unchecked);
    assert!(matches!(
        alpha.source.as_ref(),
        Some(Source::Git {
            url,
            branch: Some(branch),
            subpath: Some(subpath),
            revision: Some(revision),
        }) if url == "https://example.com/demo.git"
            && branch == "stable"
            && subpath == "skills/alpha"
            && revision == "rev-alpha"
    ));

    let archive = snapshot.get("repos/package/archive").unwrap();
    assert_eq!(archive.status, SkillStatus::MissingBaseline);
    assert!(matches!(
        archive.source.as_ref(),
        Some(Source::Archive {
            url,
            subpath: Some(subpath),
            revision: Some(revision),
        }) if url == "https://example.com/tools.zip"
            && subpath == "nested/archive"
            && revision == "sha256:archive"
    ));

    assert!(matches!(
        snapshot.get("repos/demo/broken").unwrap().status,
        SkillStatus::CorruptMeta { .. }
    ));
    assert!(matches!(
        snapshot.get("repos/demo/wrong-kind").unwrap().status,
        SkillStatus::CorruptMeta { .. }
    ));
    let missing = snapshot.get("repos/demo/missing").unwrap();
    assert_eq!(missing.status, SkillStatus::Missing);
    assert_eq!(missing.baseline_hash.as_deref(), Some("baseline-missing"));

    assert_eq!(
        snapshot
            .skills
            .iter()
            .map(|record| record.key.as_str())
            .collect::<Vec<_>>(),
        [
            "repos/demo/alpha",
            "repos/demo/broken",
            "repos/demo/missing",
            "repos/demo/wrong-kind",
            "repos/package/archive",
        ]
    );
}

#[test]
fn root_metadata_keeps_remote_entries_and_ignores_local_entries() {
    let fixture = Fixture::new("startup-inventory-root-document");
    fixture.write_skill("local-one", "local-one");
    fixture.write_skill("tracked", "tracked");
    fixture.write_metadata(
        ".root.toml",
        r#"schema = 1

[skills.local-one.source]
type = "local"
path = "/tmp/original"
[skills.local-one.baseline]
hash = "must-not-apply"
hash_algo = 1

[skills.tracked]
note = "tracked note"
[skills.tracked.source]
type = "git"
url = "https://example.com/tracked.git"
branch = "main"
subpath = "tracked"
revision = "rev-one"
[skills.tracked.baseline]
hash = "tracked-baseline"
hash_algo = 1
"#,
    );

    let snapshot = fixture.scan().unwrap();
    let local = snapshot.get("local-one").unwrap();
    assert_eq!(local.status, SkillStatus::Local);
    assert!(local.source.is_none());
    assert!(local.baseline_hash.is_none());

    let tracked = snapshot.get("tracked").unwrap();
    assert_eq!(tracked.status, SkillStatus::Unchecked);
    assert_eq!(tracked.note.as_deref(), Some("tracked note"));
    assert_eq!(tracked.baseline_hash.as_deref(), Some("tracked-baseline"));
    assert!(matches!(
        tracked.source.as_ref(),
        Some(Source::Git { url, branch: Some(branch), .. })
            if url == "https://example.com/tracked.git" && branch == "main"
    ));
}

#[test]
fn every_scan_observes_external_repository_metadata_edits() {
    let fixture = Fixture::new("startup-inventory-fresh-scan");
    fixture.write_skill("repos/demo/skill", "skill");
    fixture.write_metadata(
        "demo.toml",
        &repository_document(
            "Old display",
            "https://example.com/old.git",
            "old-branch",
            "old note",
        ),
    );

    let first = fixture.scan().unwrap();
    assert_eq!(first.repositories["demo"].display_name(), "Old display");
    assert_eq!(
        first.get("repos/demo/skill").unwrap().note.as_deref(),
        Some("old note")
    );

    fixture.write_metadata(
        "demo.toml",
        &repository_document(
            "New display",
            "https://example.com/new.git",
            "new-branch",
            "new note",
        ),
    );
    let second = fixture.scan().unwrap();
    assert_eq!(second.repositories["demo"].display_name(), "New display");
    let repository = &second.repositories["demo"];
    assert_eq!(repository.url, "https://example.com/new.git");
    assert_eq!(repository.branch, "new-branch");
    let skill = second.get("repos/demo/skill").unwrap();
    assert_eq!(skill.note.as_deref(), Some("new note"));
    assert!(matches!(
        skill.source.as_ref(),
        Some(Source::Git { url, branch: Some(branch), .. })
            if url == "https://example.com/new.git" && branch == "new-branch"
    ));
}

fn repository_document(name: &str, url: &str, branch: &str, note: &str) -> String {
    format!(
        r#"schema = 1
alias = "demo"
name = "{name}"
url = "{url}"
kind = "git"
branch = "{branch}"

[skills.skill]
note = "{note}"
[skills.skill.source]
type = "git"
subpath = "skill"
revision = "revision"
"#
    )
}

#[test]
fn document_level_failures_remain_scan_failures() {
    let cases = [
        (
            "future-schema.toml",
            "schema = 99\nalias = 'future-schema'\nurl = 'https://example.com/repo'\n",
            "unsupported repository metadata schema 99",
        ),
        (
            ".root.toml",
            "schema = 99\n[skills.local]\nnote = 'future'\n",
            "unsupported repository metadata schema 99",
        ),
        (
            "filename.toml",
            "schema = 1\nalias = 'different'\nurl = 'https://example.com/repo'\n",
            "does not match alias",
        ),
        (
            "missing-url.toml",
            "schema = 1\nalias = 'missing-url'\n",
            "repository URL missing",
        ),
        (
            "bad-skills.toml",
            "schema = 1\nalias = 'bad-skills'\nurl = 'https://example.com/repo'\nskills = 7\n",
            "skills must be a table",
        ),
        (
            "inline-skills.toml",
            "schema = 1\nalias = 'inline-skills'\nurl = 'https://example.com/repo'\nskills = { one = { note = 'inline' } }\n",
            "skills must be a table",
        ),
        (
            "bad-key.toml",
            "schema = 1\nalias = 'bad-key'\nurl = 'https://example.com/repo'\n[skills.'bad/name']\n",
            "invalid skill identity",
        ),
        (
            "invalid-toml.toml",
            "schema = 1\nalias = [\n",
            "invalid metadata",
        ),
    ];

    for (index, (filename, document, expected)) in cases.into_iter().enumerate() {
        let fixture = Fixture::new(&format!("startup-inventory-global-error-{index}"));
        fixture.write_metadata(filename, document);
        let error = fixture.scan().unwrap_err();
        assert!(
            format!("{error:#}").contains(expected),
            "{filename}: expected {expected:?}, got {error:#}"
        );
    }
}

#[test]
fn parallel_document_failures_are_merged_in_path_order() {
    let fixture = Fixture::new("startup-inventory-error-order");
    for alias in ["middle-a", "middle-b", "middle-c", "middle-d", "middle-e"] {
        fixture.write_metadata(
            &format!("{alias}.toml"),
            &format!("schema = 1\nalias = '{alias}'\nurl = 'https://example.com/{alias}'\n"),
        );
    }
    fixture.write_metadata("z-invalid.toml", "schema = 1\nalias = [\n");
    fixture.write_metadata("a-invalid.toml", "schema = 1\nalias = [\n");

    let error = fixture.scan().unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("a-invalid.toml"), "{error}");
    assert!(!error.contains("z-invalid.toml"), "{error}");
}
