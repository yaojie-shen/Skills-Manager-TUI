use skills::{
    Workspace,
    config::{AgentConfig, Config},
    meta::{Source, SourceKind},
    ops::{DownloadDir, install, update},
    reconcile::SkillStatus,
    repository::{FetchedRepository, Repository},
};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Command, Output},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

struct HttpArchive {
    address: SocketAddr,
    content: Arc<RwLock<Vec<u8>>>,
    requests: Arc<Mutex<Vec<String>>>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl HttpArchive {
    fn new(content: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let content = Arc::new(RwLock::new(content));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_content = Arc::clone(&content);
        let worker_requests = Arc::clone(&requests);
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::spawn(move || {
            while !worker_stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => Self::respond(stream, &worker_content, &worker_requests),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            content,
            requests,
            stopping,
            worker: Some(worker),
        }
    }

    fn respond(mut stream: TcpStream, content: &RwLock<Vec<u8>>, requests: &Mutex<Vec<String>>) {
        // macOS inherits the listener's nonblocking mode; request reads need to wait for data.
        stream.set_nonblocking(false).unwrap();
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") && request.len() < 16384 {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(count) => request.extend_from_slice(&buffer[..count]),
            }
        }
        let request = String::from_utf8_lossy(&request);
        let mut words = request.lines().next().unwrap_or("").split_whitespace();
        let method = words.next().unwrap_or("");
        let path = words.next().unwrap_or("");
        requests.lock().unwrap().push(path.to_string());
        let (status, headers, body) = if path.contains("/info/refs") {
            (
                "404 Not Found",
                "Content-Type: text/plain\r\n",
                b"not a Git endpoint".to_vec(),
            )
        } else if path == "/redirect" {
            (
                "302 Found",
                "Location: /download?token=user@secret\r\n",
                vec![],
            )
        } else if path.starts_with("/download") || path == "/bundle.zip" {
            // Neither the suffix nor this generic MIME type identifies the archive.
            (
                "200 OK",
                "Content-Type: application/octet-stream\r\n",
                content.read().unwrap().clone(),
            )
        } else {
            (
                "404 Not Found",
                "Content-Type: text/plain\r\n",
                b"not found".to_vec(),
            )
        };
        let head = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if stream.write_all(head.as_bytes()).is_ok() && method != "HEAD" {
            let _ = stream.write_all(&body);
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
    fn replace(&self, content: Vec<u8>) {
        *self.content.write().unwrap() = content;
    }
}

impl Drop for HttpArchive {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Fixture {
    ws: Workspace,
    temp: DownloadDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = DownloadDir::new("archive-integration").unwrap();
        let root = temp.path().join("library");
        Config {
            agents: vec![AgentConfig {
                key: "sample".into(),
                name: "Sample".into(),
                skills_dir: temp.path().join("agent").display().to_string(),
            }],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        Self { ws, temp }
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_skills"))
            .arg("--root")
            .arg(&self.ws.root)
            .arg("--json")
            .args(args)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
    }
}

fn skill(name: &str, version: &str) -> String {
    format!("---\nname: {name}\ndescription: Tools {version}\n---\n# {name}\n{version}\n")
}

fn files(version: &str) -> Vec<(String, String)> {
    vec![
        ("bundle/README.md".into(), "Package readme".into()),
        (
            "bundle/skills/review/SKILL.md".into(),
            skill("review", version),
        ),
        (
            "bundle/skills/review/scripts/run.sh".into(),
            "#!/bin/sh\nprintf 'ready\\n'\n".into(),
        ),
        (
            "bundle/skills/writer/SKILL.md".into(),
            skill("writer", version),
        ),
        ("bundle/nested/SKILL.md".into(), skill("nested", version)),
        (
            "bundle/nested/child/SKILL.md".into(),
            skill("child", version),
        ),
    ]
}

fn zip_archive(version: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (path, content) in files(version) {
        zip.start_file(path, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn tar_gz_archive(version: &str) -> Vec<u8> {
    let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    for (path, content) in files(version) {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, content.as_bytes())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

#[test]
fn archive_cli_reuses_repository_selection_names_deployment_and_duplicate_handling() {
    let fixture = Fixture::new();
    let server = HttpArchive::new(zip_archive("v1"));
    let url = server.url("/bundle.zip");
    let listing = fixture.json(&["install", &url, "--repo-alias", "tools", "--list"]);
    assert_eq!(
        listing["choices"],
        serde_json::json!([
            "bundle/nested",
            "bundle/nested/child",
            "bundle/skills/review",
            "bundle/skills/writer"
        ])
    );
    assert_eq!(listing["repository"]["kind"], "archive");
    assert!(fixture.ws.scan().unwrap().skills.is_empty());
    assert!(Repository::list(&fixture.ws.root).unwrap().is_empty());

    let installed = fixture.json(&[
        "install",
        &url,
        "--repo-alias",
        "tools",
        "--source-name",
        "Merlin skills",
        "--select",
        "bundle/skills/review",
        "--name",
        "review-copy",
        "--deploy",
        "sample",
    ]);
    assert_eq!(
        installed["installed"],
        serde_json::json!(["repos/tools/review-copy"])
    );
    let key = "repos/tools/review-copy";
    assert_eq!(
        std::fs::read_link(fixture.temp.path().join("agent/review-copy")).unwrap(),
        fixture.ws.skill_path(key)
    );
    assert!(fixture.ws.skill_path(key).join("scripts/run.sh").is_file());
    assert!(!fixture.ws.skill_path(key).join("README.md").exists());
    let before = fixture.ws.meta.load(key).unwrap().unwrap();
    assert!(before.baseline.is_some());
    assert!(
        matches!(&before.source, Some(Source::Archive { url: recorded, subpath: Some(path), revision: Some(_) }) if recorded == &url && path == "bundle/skills/review")
    );
    assert_eq!(
        fixture.ws.scan().unwrap().get(key).unwrap().status,
        SkillStatus::Repository
    );
    assert_eq!(
        Repository::list(&fixture.ws.root).unwrap()[0].display_name(),
        "Merlin skills"
    );

    let duplicate = fixture.json(&[
        "install",
        &url,
        "--repo-alias",
        "other",
        "--select",
        "bundle/skills/review",
    ]);
    assert_eq!(duplicate["installed"], serde_json::json!([]));
    assert_eq!(fixture.ws.meta.load(key).unwrap().unwrap(), before);
    assert!(!fixture.ws.root.join("repos/other").exists());
    let renamed = fixture.json(&[
        "install",
        &url,
        "--repo-alias",
        "tools",
        "--select",
        "bundle/skills/writer",
        "--local-name",
        "bundle/skills/writer=writer-copy",
    ]);
    assert_eq!(
        renamed["installed"],
        serde_json::json!(["repos/tools/writer-copy"])
    );
    let metadata_before = fixture.ws.meta.load(key).unwrap();
    let rename = fixture.cli(&["repos", "rename", "tools", "Merlin tools"]);
    assert!(
        rename.status.success(),
        "{}",
        String::from_utf8_lossy(&rename.stderr)
    );
    assert_eq!(fixture.ws.meta.load(key).unwrap(), metadata_before);
    let all = fixture.json(&["install", &url, "--repo-alias", "tools", "--all"]);
    assert_eq!(all["installed"], serde_json::json!(["repos/tools/nested"]));
    assert_eq!(fixture.ws.scan().unwrap().skills.len(), 3);
    assert!(!fixture.ws.root.join("repos/tools/child").exists());
    assert_eq!(
        Repository::list(&fixture.ws.root).unwrap()[0].display_name(),
        "Merlin tools"
    );
}

#[test]
fn archive_cli_requires_a_source_name_separate_from_a_skill_name() {
    let fixture = Fixture::new();
    let server = HttpArchive::new(zip_archive("v1"));
    let url = server.url("/bundle.zip");
    let output = fixture.cli(&[
        "install",
        &url,
        "--subpath",
        "bundle/skills/review",
        "--name",
        "renamed-skill",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--source-name"));
    assert!(
        server.requests.lock().unwrap().is_empty(),
        "missing name must be reported before downloading"
    );
    assert!(fixture.ws.meta.list_keys().unwrap().is_empty());
    assert!(Repository::list(&fixture.ws.root).unwrap().is_empty());
}

#[test]
fn archive_subpath_preserves_wrapper_paths_and_detects_format_from_bytes() {
    let fixture = Fixture::new();
    let server = HttpArchive::new(tar_gz_archive("v1"));
    let url = server.url("/download?token=user@secret");
    let listing = fixture.json(&[
        "install",
        &url,
        "--repo-alias",
        "tools",
        "--subpath",
        "bundle/skills",
        "--list",
    ]);
    assert_eq!(
        listing["choices"],
        serde_json::json!(["bundle/skills/review", "bundle/skills/writer"])
    );
    let result = fixture.json(&[
        "install",
        &url,
        "--repo-alias",
        "tools",
        "--source-name",
        "Downloaded tools",
        "--subpath",
        "bundle/skills/review",
    ]);
    assert_eq!(
        result["installed"],
        serde_json::json!(["repos/tools/review"])
    );
    let meta = fixture.ws.meta.load("repos/tools/review").unwrap().unwrap();
    assert_eq!(meta.source.unwrap().url(), Some(url.as_str()));
    let requests = server.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|path| path == "/download?token=user@secret")
    );
    assert!(!requests.iter().any(|path| path == "/download?token=user"));
}

#[test]
fn ambiguous_git_url_keeps_git_configuration_and_inline_branch_syntax() {
    let fixture = Fixture::new();
    let upstream = fixture.temp.path().join("git-upstream");
    std::fs::create_dir(&upstream).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&upstream)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet", "-b", "develop"]);
    std::fs::write(upstream.join("SKILL.md"), skill("toolkit", "v1")).unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "fixture"]);
    let config = fixture.temp.path().join("gitconfig");
    let url = "https://git.example.invalid/team/toolkit";
    std::fs::write(
        &config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = {url}\n",
            upstream.display()
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_skills"))
        .args(["--json", "--root"])
        .arg(&fixture.ws.root)
        .args([
            "install",
            &format!("{url}@develop"),
            "--repo-alias",
            "compat",
            "--all",
        ])
        .env("GIT_CONFIG_GLOBAL", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source = fixture
        .ws
        .meta
        .load("repos/compat/toolkit")
        .unwrap()
        .unwrap()
        .source
        .unwrap();
    assert_eq!(source.kind(), "git");
    assert_eq!(source.url(), Some(url));
    assert_eq!(source.branch(), Some("develop"));
    std::fs::write(upstream.join("SKILL.md"), skill("toolkit", "v2")).unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "new version"]);
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_skills"))
            .args(["--json", "--root"])
            .arg(&fixture.ws.root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &config)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let checked = run(&["check", "--all"]);
    assert_eq!(checked["results"][0]["update_available"], true);
    let updated = run(&["update", "--all"]);
    assert_eq!(updated[0]["result"], "updated");
    assert_eq!(updated[0]["to_revision"], checked["results"][0]["remote"]);
    assert_eq!(
        run(&["check", "--all"])["results"][0]["update_available"],
        false
    );
}

#[test]
fn redirected_archive_uses_shared_fetch_install_check_and_update_flow() {
    let fixture = Fixture::new();
    let server = HttpArchive::new(zip_archive("v1"));
    let url = server.url("/redirect");
    let reference = install::parse_ref(&url, None, None).unwrap();
    let mut messages = vec![];
    let mut fetched = FetchedRepository::fetch_with_progress(
        &fixture.ws,
        &reference,
        Some("tools"),
        &mut |message| messages.push(message.to_string()),
    )
    .unwrap();
    assert_eq!(fetched.repository.kind, SourceKind::Archive);
    assert_eq!(fetched.repository.url, url);
    fetched.repository.set_name("Downloaded tools").unwrap();
    assert!(messages.iter().any(|message| message.contains("Scan:")));
    let keys = fetched
        .install(
            &fixture.ws,
            &["bundle/skills/review".into()],
            &BTreeMap::new(),
        )
        .unwrap();
    fetched.cleanup();
    let key = &keys[0];
    let installed_revision = fixture
        .ws
        .meta
        .load(key)
        .unwrap()
        .unwrap()
        .source
        .unwrap()
        .revision()
        .unwrap()
        .to_string();
    let check = fixture.json(&["check", "--all"]);
    assert_eq!(check["results"].as_array().unwrap().len(), 1);
    assert_eq!(check["results"][0]["update_available"], false);
    assert_eq!(check["errors"], serde_json::json!([]));

    server.replace(zip_archive("v2"));
    assert!(update::check(&fixture.ws, key).unwrap().update_available);
    let report = fixture.json(&["update", "--all"]);
    assert_eq!(report[0]["result"], "updated");
    assert_eq!(
        fixture.ws.scan().unwrap().skills.len(),
        1,
        "newly discovered skills are not installed during update"
    );
    let record = fixture.ws.scan().unwrap().get(key).unwrap().clone();
    assert_eq!(record.status, SkillStatus::Repository);
    assert_eq!(record.description.as_deref(), Some("Tools v2"));
    assert_ne!(
        record.source.as_ref().unwrap().revision(),
        Some(installed_revision.as_str())
    );
    assert_eq!(record.source.as_ref().unwrap().url(), Some(url.as_str()));
    assert!(!update::check(&fixture.ws, key).unwrap().update_available);
    assert_eq!(
        Repository::list(&fixture.ws.root).unwrap()[0].display_name(),
        "Downloaded tools"
    );
}

#[test]
fn archive_update_retains_local_resolution_and_revalidates_before_publish() {
    let fixture = Fixture::new();
    let server = HttpArchive::new(zip_archive("v1"));
    let url = server.url("/bundle.zip");
    fixture.json(&[
        "install",
        &url,
        "--repo-alias",
        "tools",
        "--source-name",
        "Downloaded tools",
        "--subpath",
        "bundle/skills/review",
    ]);
    let key = "repos/tools/review";
    let file = fixture.ws.skill_path(key).join("SKILL.md");
    let local = skill("review", "local edits");
    std::fs::write(&file, &local).unwrap();
    server.replace(zip_archive("v2"));
    let snap = fixture.ws.scan().unwrap();
    assert_eq!(snap.get(key).unwrap().status, SkillStatus::Modified);
    let prepared = update::prepare(&fixture.ws, &snap, key).unwrap();
    assert!(prepared.needs_resolution());
    assert!(
        prepared.baseline_dir.is_none(),
        "an archive URL does not provide historical revisions"
    );
    let meta = fixture.ws.meta.load(key).unwrap();
    update::apply(
        &fixture.ws,
        &prepared,
        update::Take::Local,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), local);
    assert_eq!(fixture.ws.meta.load(key).unwrap(), meta);
    let prepared = update::prepare(&fixture.ws, &snap, key).unwrap();
    let concurrent = skill("review", "newer local edits");
    std::fs::write(&file, &concurrent).unwrap();
    assert!(
        update::apply(
            &fixture.ws,
            &prepared,
            update::Take::Upstream,
            &BTreeMap::new()
        )
        .is_err()
    );
    prepared.cleanup();
    assert_eq!(std::fs::read_to_string(file).unwrap(), concurrent);
}

#[test]
fn archive_cli_rejects_git_branch_without_installing() {
    let fixture = Fixture::new();
    let server = HttpArchive::new(zip_archive("v1"));
    let output = fixture.cli(&[
        "install",
        &server.url("/bundle.zip"),
        "--branch",
        "main",
        "--all",
    ]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("branch"), "{error}");
    assert!(fixture.ws.scan().unwrap().skills.is_empty());
}

#[test]
fn mixed_git_and_redirected_archive_checks_match_batch_updates() {
    let fixture = Fixture::new();
    let server = HttpArchive::new(zip_archive("v1"));
    let url = server.url("/redirect");
    let mut keys = Vec::new();
    for name in ["review", "writer"] {
        keys.push(
            install::install(
                &fixture.ws,
                &install::InstallRef::Url {
                    url: url.clone(),
                    branch: None,
                    subpath: Some(format!("bundle/skills/{name}")),
                },
                Some(name),
            )
            .unwrap(),
        );
    }
    let repository = fixture.temp.path().join("git-source");
    std::fs::create_dir(&repository).unwrap();
    let git = |args: &[&str]| skills::ops::git(args, Some(&repository)).unwrap();
    git(&["init", "--initial-branch=main"]);
    let commit = || {
        git(&["add", "SKILL.md"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "version",
        ]);
    };
    std::fs::write(repository.join("SKILL.md"), skill("git-tool", "v1")).unwrap();
    commit();
    keys.push(
        install::install(
            &fixture.ws,
            &install::InstallRef::Git {
                url: format!("file://{}", repository.display()),
                branch: Some("main".into()),
                subpath: None,
            },
            Some("git-tool"),
        )
        .unwrap(),
    );
    let downloads = || {
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.as_str() == "/download?token=user@secret")
            .count()
    };

    let before = downloads();
    let unchanged = fixture.json(&["check", "--all"]);
    assert_eq!(unchanged["results"].as_array().unwrap().len(), 3);
    assert_eq!(unchanged["errors"], serde_json::json!([]));
    assert!(
        unchanged["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["update_available"] == false)
    );
    assert_eq!(
        downloads() - before,
        1,
        "one archive download per check batch"
    );

    server.replace(zip_archive("v2"));
    std::fs::write(repository.join("SKILL.md"), skill("git-tool", "v2")).unwrap();
    commit();
    let checked = fixture.json(&["check", "--all"]);
    assert!(
        checked["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["update_available"] == true)
    );
    let before = downloads();
    let updated = fixture.json(&["update", "--all"]);
    assert_eq!(
        downloads() - before,
        1,
        "one archive download per update batch"
    );
    for result in updated.as_array().unwrap() {
        assert_eq!(result["result"], "updated");
        let check = checked["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["skill"] == result["skill"])
            .unwrap();
        assert_eq!(result["to_revision"], check["remote"]);
    }
    for key in &keys {
        assert!(
            std::fs::read_to_string(fixture.ws.skill_path(key).join("SKILL.md"))
                .unwrap()
                .contains("v2")
        );
    }
    let current = fixture.json(&["check", "--all"]);
    assert!(
        current["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["update_available"] == false)
    );

    // A combined library session also reuses the checked archive for preparation.
    server.requests.lock().unwrap().clear();
    let mut session = update::UpdateSession::default();
    let snap = fixture.ws.scan().unwrap();
    for key in &keys[..2] {
        let checked = session.check(&fixture.ws, key, &mut |_| {}).unwrap();
        let prepared = session.prepare(&snap, key, &mut |_| {}).unwrap();
        assert_eq!(checked.remote, prepared.to_revision);
        prepared.cleanup();
    }
    assert_eq!(downloads(), 1);

    // Changing only the URL query must invalidate the source identity.
    server.replace(zip_archive("v3"));
    let mut meta = fixture.ws.meta.load(&keys[0]).unwrap().unwrap();
    let previous = meta
        .source
        .as_ref()
        .unwrap()
        .revision()
        .unwrap()
        .to_string();
    let Source::Archive { url, .. } = meta.source.as_mut().unwrap() else {
        panic!("expected archive source")
    };
    *url = server.url("/download?token=changed@query");
    fixture.ws.meta.save(&keys[0], &meta).unwrap();
    let changed = session.check(&fixture.ws, &keys[0], &mut |_| {}).unwrap();
    assert!(changed.update_available);
    assert_ne!(changed.remote, previous);
    let prepared = session
        .prepare(&fixture.ws.scan().unwrap(), &keys[0], &mut |_| {})
        .unwrap();
    assert_eq!(prepared.to_revision, changed.remote);
    prepared.cleanup();
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path == "/download?token=changed@query")
    );
}
