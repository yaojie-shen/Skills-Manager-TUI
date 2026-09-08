//! Installing from a repository: subdirectory installs, the several-skills
//! question, name clashes, and the clean update path.

use skills::Workspace;
use skills::config::{Config, DeployConfig};
use skills::hash::hash_directory;
use skills::meta::Source;
use skills::ops::install::{self, NotOneSkill};
use skills::ops::update::{self, Take};
use skills::reconcile::SkillStatus;
use std::path::{Path, PathBuf};

/// A skills root with no agents: nothing here deploys, so agent directories
/// would only add noise to what install and update actually do.
struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("skills-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("skills");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config {
            schema: 1,
            agents: vec![],
            deploy: DeployConfig {
                all_to_all: true,
                presets: vec![],
            },
            tags: vec![],
            search: Default::default(),
            ui: Default::default(),
        };
        cfg.save(&root).unwrap();
        Self { base, root }
    }

    fn ws(&self) -> Workspace {
        Workspace::open(&self.root).unwrap()
    }

    fn staging_empty(&self) -> bool {
        !self
            .root
            .join(".skills-meta/.staging")
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(args: &[&str], cwd: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn skill_md(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {body}\n---\n{body}\n")
}

/// Create a repository at `dir` with an initial commit, returning its revision.
fn init_repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    git(&["init", "-q", "-b", "main"], dir);
    git(&["config", "user.email", "t@example.com"], dir);
    git(&["config", "user.name", "t"], dir);
    commit(dir, "init")
}

fn commit(dir: &Path, msg: &str) -> String {
    git(&["add", "-A"], dir);
    git(&["commit", "-q", "--allow-empty", "-m", msg], dir);
    git(&["rev-parse", "HEAD"], dir)
}

fn url_of(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

/// `Fetched` is not `Debug`, so `unwrap_err` cannot be used on a fetch.
fn fetch_err(ws: &Workspace, r: &install::InstallRef) -> anyhow::Error {
    match install::fetch(ws, r) {
        Ok(f) => panic!("expected a failure, fetched {}", f.skill_dir.display()),
        Err(e) => e,
    }
}

/// A repo whose skill lives in a subdirectory: the subdirectory alone lands in
/// the root, and everything needed to update it later is recorded.
#[test]
fn install_from_repo_subdirectory() {
    let f = Fixture::new("subdir");
    let ws = f.ws();
    let up = f.base.join("upstream");
    write(&up.join("README.md"), "not part of the skill\n");
    write(
        &up.join("skills/writer/SKILL.md"),
        &skill_md("writer", "v1"),
    );
    write(&up.join("skills/writer/ref/notes.md"), "notes v1\n");
    let rev = init_repo(&up);

    // No branch is passed, so the recorded branch has to come from the clone.
    let r = install::parse_ref(&url_of(&up), None, Some("skills/writer")).unwrap();
    let key = install::install(&ws, &r, None).unwrap();
    assert_eq!(key, "writer", "the subpath names the skill");

    let dir = f.root.join("writer");
    assert!(dir.join("SKILL.md").is_file());
    assert_eq!(
        std::fs::read_to_string(dir.join("ref/notes.md")).unwrap(),
        "notes v1\n",
        "nested files come across"
    );
    assert!(!dir.join("README.md").exists(), "only the subpath is taken");
    assert!(!dir.join(".git").exists());

    let snap = ws.scan().unwrap();
    let rec = snap.get("writer").unwrap();
    assert_eq!(rec.status, SkillStatus::Managed { no_baseline: false });
    match &rec.source {
        Some(Source::Git {
            url,
            subpath,
            branch,
            revision,
        }) => {
            assert_eq!(url, &url_of(&up));
            assert_eq!(subpath.as_deref(), Some("skills/writer"));
            assert_eq!(branch.as_deref(), Some("main"));
            assert_eq!(revision.as_deref(), Some(rev.as_str()));
        }
        other => panic!("unexpected source {other:?}"),
    }
    assert_eq!(
        rec.baseline_hash.as_deref(),
        Some(hash_directory(&dir).unwrap().as_str()),
        "baseline is the hash of what was installed"
    );
    assert!(f.staging_empty(), "staging cleaned");
}

/// Pointing at a repository of many skills is a question, not a failure.
#[test]
fn several_skills_reports_choices() {
    let f = Fixture::new("choices");
    let ws = f.ws();
    let up = f.base.join("many");
    write(&up.join("skills/beta/SKILL.md"), &skill_md("beta", "b"));
    write(&up.join("skills/alpha/SKILL.md"), &skill_md("alpha", "a"));
    write(&up.join("tools/gamma/SKILL.md"), &skill_md("gamma", "g"));
    // Hidden directories are not offered even though they hold a SKILL.md.
    write(&up.join(".archive/old/SKILL.md"), &skill_md("old", "o"));
    init_repo(&up);

    let r = install::parse_ref(&url_of(&up), None, None).unwrap();
    let err = fetch_err(&ws, &r);
    let choice = err
        .downcast_ref::<NotOneSkill>()
        .unwrap_or_else(|| panic!("expected NotOneSkill, got {err:#}"));
    assert_eq!(
        choice.choices,
        ["skills/alpha", "skills/beta", "tools/gamma"]
    );

    // Choices stay relative to the repository root even when the reference
    // already narrowed to a subdirectory, because that is what a caller passes
    // back as the subpath on the second attempt.
    let r = install::parse_ref(&url_of(&up), None, Some("skills")).unwrap();
    let err = fetch_err(&ws, &r);
    let choice = err.downcast_ref::<NotOneSkill>().unwrap();
    assert_eq!(choice.choices, ["skills/alpha", "skills/beta"]);

    // And that subpath really does install, which is the point of reporting it.
    let r = install::parse_ref(&url_of(&up), None, Some(&choice.choices[0])).unwrap();
    assert_eq!(install::install(&ws, &r, None).unwrap(), "alpha");

    // A repository holding no skill at all is an ordinary failure.
    let empty = f.base.join("empty");
    write(&empty.join("README.md"), "nothing here\n");
    init_repo(&empty);
    let r = install::parse_ref(&url_of(&empty), None, None).unwrap();
    let err = fetch_err(&ws, &r);
    assert!(
        err.downcast_ref::<NotOneSkill>().is_none(),
        "nothing to choose between"
    );
    assert!(
        format!("{err:#}").contains("no SKILL.md"),
        "unexpected message: {err:#}"
    );
    assert!(f.staging_empty(), "a failed fetch leaves no staging behind");
}

#[test]
fn discover_finds_nested_skills_and_skips_dot_dirs() {
    let f = Fixture::new("discover");
    let tree = f.base.join("tree");
    write(&tree.join("SKILL.md"), &skill_md("root", "r"));
    write(&tree.join("b/SKILL.md"), &skill_md("b", "b"));
    write(&tree.join("a/nested/SKILL.md"), &skill_md("nested", "n"));
    write(&tree.join(".hidden/SKILL.md"), &skill_md("hidden", "h"));
    write(&tree.join("c/.hidden/SKILL.md"), &skill_md("hidden", "h"));
    write(&tree.join("d/SKILL.md.bak"), "not a skill\n");

    assert_eq!(install::discover(&tree), ["a/nested", "b"]);
    assert_eq!(
        install::discover(&tree.join("d")),
        Vec::<String>::new(),
        "SKILL.md.bak is not SKILL.md"
    );
}

/// The second install must not damage the first one's directory or metadata.
#[test]
fn duplicate_name_fails_and_keeps_the_first() {
    let f = Fixture::new("dup");
    let ws = f.ws();
    let up = f.base.join("first");
    write(&up.join("SKILL.md"), &skill_md("dup", "first"));
    let rev = init_repo(&up);
    let r = install::parse_ref(&url_of(&up), Some("main"), None).unwrap();
    assert_eq!(install::install(&ws, &r, Some("dup")).unwrap(), "dup");

    // A different source, same name.
    let other = f.base.join("second");
    write(&other.join("SKILL.md"), &skill_md("dup", "second"));
    let r2 = install::parse_ref(other.to_str().unwrap(), None, None).unwrap();
    let err = install::install(&ws, &r2, Some("dup")).unwrap_err();
    assert!(
        format!("{err:#}").contains("already exists"),
        "unexpected message: {err:#}"
    );

    assert_eq!(
        std::fs::read_to_string(f.root.join("dup/SKILL.md")).unwrap(),
        skill_md("dup", "first"),
        "the installed skill is untouched"
    );
    let snap = ws.scan().unwrap();
    let rec = snap.get("dup").unwrap();
    assert_eq!(rec.status, SkillStatus::Managed { no_baseline: false });
    match &rec.source {
        Some(Source::Git { url, revision, .. }) => {
            assert_eq!(url, &url_of(&up));
            assert_eq!(revision.as_deref(), Some(rev.as_str()));
        }
        other => panic!("source was overwritten: {other:?}"),
    }
    assert!(
        f.staging_empty(),
        "the failed install cleaned up after itself"
    );
}

/// The uncomplicated update: nothing changed locally, so the upstream tree
/// simply replaces the local one and the recorded revision moves with it.
#[test]
fn check_and_update_without_local_changes() {
    let f = Fixture::new("update");
    let ws = f.ws();
    let up = f.base.join("upstream");
    write(&up.join("skills/up/SKILL.md"), &skill_md("up", "v1"));
    write(&up.join("skills/up/extra.md"), "extra v1\n");
    let rev1 = init_repo(&up);

    let r = install::parse_ref(&url_of(&up), Some("main"), Some("skills/up")).unwrap();
    assert_eq!(install::install(&ws, &r, None).unwrap(), "up");
    let dir = f.root.join("up");

    let c = update::check(&ws, "up").unwrap();
    assert!(!c.update_available);
    assert_eq!(c.remote, rev1);
    assert_eq!(c.installed.as_deref(), Some(rev1.as_str()));

    // Upstream rewrites SKILL.md, drops a file and adds another.
    write(&up.join("skills/up/SKILL.md"), &skill_md("up", "v2"));
    std::fs::remove_file(up.join("skills/up/extra.md")).unwrap();
    write(&up.join("skills/up/added.md"), "added v2\n");
    let rev2 = commit(&up, "v2");
    assert_ne!(rev1, rev2);

    let c = update::check(&ws, "up").unwrap();
    assert!(c.update_available);
    assert_eq!(c.remote, rev2);
    assert_eq!(c.branch.as_deref(), Some("main"));

    let snap = ws.scan().unwrap();
    let prepared = update::prepare(&ws, &snap, "up").unwrap();
    assert!(
        !prepared.needs_resolution(),
        "an unmodified skill has nothing to resolve"
    );
    assert_eq!(prepared.from_revision.as_deref(), Some(rev1.as_str()));
    assert_eq!(prepared.to_revision, rev2);
    assert!(!prepared.workdir.starts_with(&ws.root));
    assert_eq!(
        prepared.workdir.parent(),
        Some(std::env::temp_dir().as_path())
    );
    assert!(
        f.staging_empty(),
        "preparing an update writes nothing to central staging"
    );
    update::apply(&ws, &prepared, Take::Upstream, &Default::default()).unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
        skill_md("up", "v2")
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("added.md")).unwrap(),
        "added v2\n"
    );
    assert!(
        !dir.join("extra.md").exists(),
        "a file dropped upstream is dropped locally"
    );

    let snap = ws.scan().unwrap();
    let rec = snap.get("up").unwrap();
    assert_eq!(
        rec.status,
        SkillStatus::Managed { no_baseline: false },
        "the update is not a local modification"
    );
    assert_eq!(
        rec.baseline_hash.as_deref(),
        Some(hash_directory(&dir).unwrap().as_str())
    );
    match &rec.source {
        Some(Source::Git { revision, .. }) => assert_eq!(revision.as_deref(), Some(rev2.as_str())),
        other => panic!("unexpected source {other:?}"),
    }
    assert!(!update::check(&ws, "up").unwrap().update_available);
    assert!(
        !prepared.workdir.exists(),
        "download cleaned after applying"
    );
    assert!(f.staging_empty(), "staging cleaned");
}

/// Installing from a directory copies it; the original must survive so a
/// mistyped name cannot cost the user their working copy.
#[test]
fn install_from_local_directory_copies() {
    let f = Fixture::new("local");
    let ws = f.ws();
    let src = f.base.join("workshop/handy");
    write(&src.join("SKILL.md"), &skill_md("handy", "local"));
    write(&src.join("ref/data.txt"), "payload\n");

    let r = install::parse_ref(src.to_str().unwrap(), None, None).unwrap();
    assert_eq!(install::install(&ws, &r, None).unwrap(), "handy");

    assert!(
        src.join("SKILL.md").is_file(),
        "source was copied, not moved"
    );
    assert_eq!(
        std::fs::read_to_string(src.join("ref/data.txt")).unwrap(),
        "payload\n"
    );
    let dir = f.root.join("handy");
    assert_eq!(
        hash_directory(&dir).unwrap(),
        hash_directory(&src).unwrap(),
        "the copy is identical"
    );

    let snap = ws.scan().unwrap();
    let rec = snap.get("handy").unwrap();
    match &rec.source {
        Some(Source::Local { path }) => assert_eq!(
            path.as_deref(),
            Some(skills::paths::contract_tilde(&std::fs::canonicalize(&src).unwrap()).as_str())
        ),
        other => panic!("unexpected source {other:?}"),
    }
    assert_eq!(
        rec.baseline_hash.as_deref(),
        Some(hash_directory(&dir).unwrap().as_str())
    );
    assert!(f.staging_empty(), "staging cleaned");
}

/// Downloading and preparing are independent of the destination filesystem;
/// only publishing requires its staging directory.
#[test]
fn git_download_and_update_prepare_do_not_need_central_staging() {
    let f = Fixture::new("download-local");
    let ws = f.ws();
    let up = f.base.join("upstream");
    write(&up.join("SKILL.md"), &skill_md("local-download", "v1"));
    init_repo(&up);
    commit(&up, "another revision");
    let r = install::parse_ref(&url_of(&up), Some("main"), None).unwrap();
    let staging = f.root.join(".skills-meta/.staging");
    write(&staging, "staging unavailable");
    let fetched = install::fetch(&ws, &r).unwrap();
    assert_eq!(
        fetched.workdir.parent(),
        Some(std::env::temp_dir().as_path())
    );
    assert!(!fetched.workdir.starts_with(&ws.root));
    assert_eq!(git(&["rev-list", "--count", "HEAD"], &fetched.workdir), "1");
    std::fs::remove_dir_all(&fetched.workdir).unwrap();
    std::fs::remove_file(&staging).unwrap();
    install::install(&ws, &r, None).unwrap();

    write(&up.join("SKILL.md"), &skill_md("local-download", "v2"));
    commit(&up, "v2");
    std::fs::remove_dir_all(&staging).unwrap();
    write(&staging, "staging unavailable");
    let prepared = update::prepare(&ws, &ws.scan().unwrap(), "local-download").unwrap();
    assert!(!prepared.workdir.starts_with(&ws.root));
    assert!(update::apply(&ws, &prepared, Take::Upstream, &Default::default()).is_err());
    assert_eq!(
        std::fs::read_to_string(ws.skill_path("local-download").join("SKILL.md")).unwrap(),
        skill_md("local-download", "v1")
    );
    std::fs::remove_file(&staging).unwrap();
    update::apply(&ws, &prepared, Take::Upstream, &Default::default()).unwrap();
    assert_eq!(
        std::fs::read_to_string(ws.skill_path("local-download").join("SKILL.md")).unwrap(),
        skill_md("local-download", "v2")
    );
    assert!(!prepared.workdir.exists());
    assert!(f.staging_empty());
}
