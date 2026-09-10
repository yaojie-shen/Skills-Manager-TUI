//! Command-line interface. Every TUI action has a subcommand here; output is
//! human-readable by default and JSON with `--json`.

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use skills::history;
use skills::ops::deploy::{self, Action};
use skills::ops::update::Take;
use skills::ops::{edit, install, update};
use skills::reconcile::{AgentDirMode, DeployState, SkillStatus, Snapshot};
use skills::search::{Query, Searcher};
use skills::{Workspace, paths};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "skills",
    version,
    about = "Manage agent skills from the terminal"
)]
pub struct Cli {
    /// Skills root (overrides $SKILLS_HOME)
    #[arg(long, global = true, value_name = "DIR")]
    pub root: Option<PathBuf>,
    /// CLI: use the project store; TUI: use the current directory for deployment scopes
    #[arg(long, global = true, conflicts_with = "root")]
    pub local: bool,
    /// CLI: use <DIR>/.agents/skills; TUI: discover deployment scopes from <DIR>
    #[arg(long, global = true, value_name = "DIR", conflicts_with = "root")]
    pub project: Option<PathBuf>,
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    pub json: bool,
    /// Resolve duplicate frontmatter names when deploying
    #[arg(long, global = true, value_parser = ["replace"])]
    pub same_name: Option<String>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Write a default config.toml into <root>/.skills-meta
    Init,
    /// Fuzzy search and filter skills
    List(ListArgs),
    /// Show one skill in detail
    Show { skill: String },
    /// Full reconciliation report of skills and agents
    Status,
    /// Manage tags
    Tag(TagArgs),
    /// Manage the free-text note of a skill
    Note(NoteArgs),
    /// Record the current content as the new baseline (accept local changes)
    Accept { skill: String },
    /// Repair an external move: migrate metadata, deployment links and references
    Migrate { old: String, new: String },
    /// Install a skill from a git repo, GitHub shorthand, or local path
    Install(InstallArgs),
    /// List registered Git repositories and their installed skills
    Repos,
    /// Bring an existing skill directory under management
    Adopt {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a skill: undeploy everywhere, delete directory and metadata
    Remove {
        skill: String,
        #[arg(long)]
        keep_meta: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// Rename a skill directory, its metadata, and agent links
    Rename { old: String, new: String },
    /// Change the recorded source of a skill without touching its content
    SetSource(SetSourceArgs),
    /// Query upstream for newer revisions
    Check {
        skill: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, conflicts_with_all = ["skill", "all"])]
        repo: Option<String>,
    },
    /// Update git-sourced skills
    Update(UpdateArgs),
    /// Link skills into agent directories
    Deploy(DeployArgs),
    /// Remove links from agent directories
    Undeploy(DeployArgs),
    /// Make agent directories match the desired state from config and presets
    Sync {
        #[arg(long)]
        dry_run: bool,
    },
    /// Agent directories
    Agents(AgentsArgs),
    /// Presets: named groups of skills
    Preset(PresetArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Free text; supports tag:, agent:, status:, source: prefixes
    pub query: Vec<String>,
    #[arg(long, short = 't')]
    pub tag: Vec<String>,
    #[arg(long, short = 'a')]
    pub agent: Vec<String>,
    #[arg(long, short = 's')]
    pub status: Vec<String>,
    #[arg(long)]
    pub untagged: bool,
}

#[derive(Args, Debug)]
pub struct TagArgs {
    #[command(subcommand)]
    pub command: TagCommand,
}

#[derive(Subcommand, Debug)]
pub enum TagCommand {
    Add {
        skill: String,
        tags: Vec<String>,
    },
    Remove {
        skill: String,
        tags: Vec<String>,
    },
    Set {
        skill: String,
        tags: Vec<String>,
    },
    /// List all tags with counts, or the tags of one skill
    List {
        skill: Option<String>,
    },
    Rename {
        old: String,
        new: String,
    },
    Delete {
        tag: String,
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Args, Debug)]
pub struct NoteArgs {
    #[command(subcommand)]
    pub command: NoteCommand,
}

#[derive(Subcommand, Debug)]
pub enum NoteCommand {
    Get {
        skill: String,
    },
    /// Set the note from an argument, or from stdin with `-`
    Set {
        skill: String,
        text: String,
    },
    Clear {
        skill: String,
    },
    /// Open the note in $EDITOR
    Edit {
        skill: String,
    },
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Local repository alias; defaults to owner--repository
    #[arg(long)]
    pub repo_alias: Option<String>,
    /// List discovered skill paths without installing
    #[arg(long)]
    pub list: bool,
    /// Install the outermost skills in all independent branches
    #[arg(long)]
    pub all: bool,
    /// Repository-relative skill paths to install (repeatable; . is the root)
    #[arg(long = "select")]
    pub select: Vec<String>,
    /// Override a local name: upstream/path=local-name
    #[arg(long = "local-name")]
    pub local_names: Vec<String>,
    /// Path, owner/repo[/subpath], GitHub tree URL, or git URL
    pub reference: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub branch: Option<String>,
    #[arg(long)]
    pub subpath: Option<String>,
    /// Deploy to these agents right after installing
    #[arg(long = "deploy", value_name = "AGENT")]
    pub deploy_to: Vec<String>,
}

#[derive(Args, Debug)]
pub struct SetSourceArgs {
    pub skill: String,
    pub reference: String,
    #[arg(long)]
    pub branch: Option<String>,
    #[arg(long)]
    pub subpath: Option<String>,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    #[arg(long, conflicts_with_all = ["skill", "all"])]
    pub repo: Option<String>,
    pub skill: Option<String>,
    #[arg(long)]
    pub all: bool,
    /// Which side wins for a locally modified skill: local | upstream
    #[arg(long)]
    pub take: Option<Take>,
    /// Removed: updates now require one choice for the whole skill
    #[arg(long = "take-file", value_name = "PATH=SIDE", hide = true)]
    pub take_file: Vec<String>,
    /// Alias for --take upstream
    #[arg(long)]
    pub force: bool,
    /// Show what would change without touching the root
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct DeployArgs {
    pub skills: Vec<String>,
    #[arg(long = "agent", short = 'a', value_name = "AGENT")]
    pub agents: Vec<String>,
    #[arg(long)]
    pub all_agents: bool,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub command: Option<AgentsCommand>,
}

#[derive(Subcommand, Debug)]
pub enum AgentsCommand {
    List,
    /// List built-in agents and their global/project directories
    Catalog,
    /// Register a built-in agent, or a custom agent with --dir
    Add {
        agent: String,
        #[arg(long, value_name = "DIR")]
        dir: Option<String>,
    },
    /// Per-entry reconciliation of one or all agents
    Status {
        agent: Option<String>,
    },
    /// Turn a whole-directory link into per-skill links
    Convert {
        agent: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove links whose target is gone; all of them when no skill is named
    Clean {
        agent: String,
        skills: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove one foreign symlink while preserving its external target
    RemoveLink {
        agent: String,
        name: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// Copy a valid foreign skill into the root and repoint its agent link
    AdoptLink {
        agent: String,
        name: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// Replace the agent's own copies that match the root with links to it
    Relink {
        agent: String,
        skills: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Args, Debug)]
pub struct PresetArgs {
    #[command(subcommand)]
    pub command: PresetCommand,
}

#[derive(Subcommand, Debug)]
pub enum PresetCommand {
    List,
    Show {
        name: String,
    },
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long = "agent", value_name = "AGENT")]
        agents: Vec<String>,
        #[arg(long = "skill", value_name = "SKILL")]
        skills: Vec<String>,
    },
    Delete {
        name: String,
        #[arg(long, short)]
        yes: bool,
    },
    Add {
        name: String,
        skills: Vec<String>,
    },
    Remove {
        name: String,
        skills: Vec<String>,
    },
    Deploy {
        name: String,
        #[arg(long = "agent", value_name = "AGENT")]
        agents: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    Undeploy {
        name: String,
        #[arg(long = "agent", value_name = "AGENT")]
        agents: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Move a preset to a new name, taking its auto-deploy entry with it
    Rename {
        old: String,
        new: String,
    },
    /// Set the one-line description; empty or omitted text clears it
    Describe {
        name: String,
        text: Option<String>,
    },
}

// ---------------------------------------------------------------------------

pub struct Ctx {
    pub ws: Workspace,
    pub same_name: Option<String>,
    pub json: bool,
}

impl Ctx {
    fn out<T: Serialize>(&self, value: &T, human: impl FnOnce()) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else {
            human();
        }
        Ok(())
    }
}

impl Cli {
    pub fn tui_workspace(&self) -> Result<Workspace> {
        Workspace::open(&paths::resolve_root(self.root.as_deref())?)
    }

    pub fn workspace(&self, create: bool) -> Result<Workspace> {
        if self.local || self.project.is_some() {
            let project = self.project.clone().unwrap_or(std::env::current_dir()?);
            Workspace::open_local(&project, create)
        } else {
            Workspace::open(&paths::resolve_root(self.root.as_deref())?)
        }
    }
}

pub fn run(cli: Cli) -> Result<()> {
    if matches!(
        &cli.command,
        Some(Command::Agents(AgentsArgs {
            command: Some(AgentsCommand::Catalog)
        }))
    ) {
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(skills::agents::BUILTINS)?
            );
        } else {
            for a in skills::agents::BUILTINS {
                println!("{:<18} {:<28} {}", a.key, a.global_dir, a.local_dir);
            }
        }
        return Ok(());
    }
    let create = matches!(
        &cli.command,
        Some(
            Command::Init
                | Command::Install(_)
                | Command::Agents(AgentsArgs {
                    command: Some(AgentsCommand::Add { .. })
                })
        )
    );
    let ws = cli.workspace(create)?;
    let command = cli
        .command
        .expect("dispatcher only calls run with a subcommand");
    if let Command::Init = command {
        return cmd_init(&ws.root, cli.json, ws.project.is_some());
    }
    let ctx = Ctx {
        ws,
        json: cli.json,
        same_name: cli.same_name,
    };
    match command {
        Command::Init => unreachable!(),
        Command::List(a) => cmd_list(&ctx, a),
        Command::Show { skill } => cmd_show(&ctx, &skill),
        Command::Status => cmd_status(&ctx),
        Command::Tag(a) => cmd_tag(&ctx, a.command),
        Command::Note(a) => cmd_note(&ctx, a.command),
        Command::Accept { skill } => {
            let m = edit::accept(&ctx.ws, &skill)?;
            ctx.out(&m, || println!("baseline updated for {skill}"))
        }
        Command::Migrate { old, new } => {
            edit::migrate_meta(&ctx.ws, &old, &new)?;
            ctx.out(
                &serde_json::json!({"migrated": {"from": old, "to": new}}),
                || println!("migrated {old} -> {new}"),
            )
        }
        Command::Install(a) => cmd_install(&ctx, a),
        Command::Adopt { path, name } => {
            let key = install::adopt(&ctx.ws, &path, name.as_deref())?;
            ctx.out(&serde_json::json!({"adopted": key}), || {
                println!("adopted {key}")
            })
        }
        Command::Remove {
            skill,
            keep_meta,
            yes,
        } => {
            if !yes {
                bail!("refusing to remove {skill} without --yes");
            }
            let snap = ctx.ws.scan()?;
            let log = edit::remove(&ctx.ws, &snap, &skill, keep_meta)?;
            ctx.out(&serde_json::json!({"removed": skill, "log": log}), || {
                for l in &log {
                    println!("{l}");
                }
            })
        }
        Command::Rename { old, new } => {
            let snap = ctx.ws.scan()?;
            let log = edit::rename(&ctx.ws, &snap, &old, &new)?;
            ctx.out(
                &serde_json::json!({"renamed": {"from": old, "to": new}, "log": log}),
                || {
                    for l in &log {
                        println!("{l}");
                    }
                },
            )
        }
        Command::SetSource(a) => {
            let r = install::parse_ref(&a.reference, a.branch.as_deref(), a.subpath.as_deref())?;
            let m = install::set_source(&ctx.ws, &a.skill, &r)?;
            ctx.out(&m, || {
                println!(
                    "source of {} set to {}",
                    a.skill,
                    m.source.as_ref().map(|s| s.summary()).unwrap_or_default()
                )
            })
        }
        Command::Check { skill, all, repo } => {
            if let Some(alias) = repo {
                let snap = ctx.ws.scan()?;
                let results: Vec<_> = snap
                    .skills
                    .iter()
                    .filter(|s| skills::repository::alias_of(&s.key) == Some(alias.as_str()))
                    .map(|s| update::check(&ctx.ws, &s.key))
                    .collect::<Result<_>>()?;
                ctx.out(&results, || {
                    for result in &results {
                        println!(
                            "{}: {}",
                            result.skill,
                            if result.update_available {
                                "update available"
                            } else {
                                "up to date"
                            }
                        );
                    }
                })
            } else {
                cmd_check(&ctx, skill, all)
            }
        }
        Command::Repos => {
            let repositories = skills::repository::Repository::list(&ctx.ws.root)?;
            ctx.out(&repositories, || {
                for repo in &repositories {
                    println!("{}  {}  {}", repo.alias, repo.url, repo.branch);
                }
            })
        }
        Command::Update(a) => cmd_update(&ctx, a),
        Command::Deploy(a) => cmd_deploy(&ctx, a, true),
        Command::Undeploy(a) => cmd_deploy(&ctx, a, false),
        Command::Sync { dry_run } => {
            let snap = ctx.ws.scan()?;
            let actions = deploy::plan_sync(&ctx.ws, &snap)?;
            run_actions(&ctx, &actions, dry_run)
        }
        Command::Agents(a) => cmd_agents(&ctx, a.command),
        Command::Preset(a) => cmd_preset(&ctx, a.command),
    }
}

fn cmd_init(root: &std::path::Path, json: bool, local: bool) -> Result<()> {
    use skills::config::Config;
    if Config::exists(root) {
        bail!("config already exists: {}", Config::path(root).display());
    }
    let cfg = if local {
        Config::local_default()
    } else {
        Config::default()
    };
    cfg.save(root)?;
    if json {
        println!("{}", serde_json::json!({"created": Config::path(root)}));
    } else {
        println!("wrote {}", Config::path(root).display());
    }
    Ok(())
}

#[derive(Serialize)]
struct ListRow<'a> {
    key: &'a str,
    name: Option<&'a str>,
    status: &'static str,
    tags: &'a [String],
    deployed: Vec<&'a str>,
    source: Option<&'static str>,
    description: Option<&'a str>,
    score: f32,
    matched: Vec<skills::search::Field>,
    excerpt: Option<&'a str>,
}

fn cmd_list(ctx: &Ctx, a: ListArgs) -> Result<()> {
    let snap = ctx.ws.scan()?;
    let mut input = a.query.join(" ");
    for t in &a.tag {
        input.push_str(&format!(" tag:{t}"));
    }
    for g in &a.agent {
        input.push_str(&format!(" agent:{g}"));
    }
    for s in &a.status {
        input.push_str(&format!(" status:{s}"));
    }
    if a.untagged {
        input.push_str(" untagged");
    }
    let q = Query::parse(&input);
    let hits = Searcher::for_workspace(&ctx.ws).search(&snap.skills, &q);
    let rows: Vec<ListRow> = hits
        .iter()
        .map(|h| {
            let r = &snap.skills[h.index];
            ListRow {
                key: &r.key,
                name: r.name.as_deref(),
                status: r.status.label(),
                tags: &r.tags,
                deployed: r.deployed_to(),
                source: r.source.as_ref().map(|s| s.kind()),
                description: r.description.as_deref(),
                score: h.score,
                matched: h.fields.clone(),
                excerpt: h.excerpt.as_ref().map(|e| e.text.as_str()),
            }
        })
        .collect();
    ctx.out(&rows, || {
        let w = rows.iter().map(|r| r.key.len()).max().unwrap_or(4).max(4);
        for r in &rows {
            let tags = if r.tags.is_empty() {
                String::new()
            } else {
                format!("[{}]", r.tags.join(","))
            };
            let dep = r.deployed.join(",");
            println!(
                "{:<w$}  {:<10} {:<14} {:<12} {}",
                r.key,
                r.status,
                dep,
                tags,
                truncate(r.description.unwrap_or(""), 70),
                w = w
            );
            if let Some(e) = r.excerpt {
                let fields: Vec<&str> = r.matched.iter().map(|f| f.label()).collect();
                println!(
                    "{:<w$}  ↳ [{}] {}",
                    "",
                    fields.join(","),
                    truncate(e, 110),
                    w = w
                );
            }
        }
    })
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn cmd_show(ctx: &Ctx, key: &str) -> Result<()> {
    let snap = ctx.ws.scan()?;
    let r = snap
        .get(key)
        .with_context(|| format!("no such skill: {key}"))?;
    // A detail request may display the hash; compute only this skill when the
    // reconciliation did not need its content for a status comparison.
    let mut detail = r.clone();
    if detail.name.is_some() && detail.current_hash.is_none() {
        detail.current_hash = skills::hash::hash_directory(&detail.path).ok();
    }
    let r = &detail;
    ctx.out(r, || {
        println!("key:         {}", r.key);
        if let Some(n) = &r.name {
            println!(
                "name:        {}{}",
                n,
                if r.name_mismatch {
                    "  (differs from directory name)"
                } else {
                    ""
                }
            );
        }
        println!(
            "path:        {}{}",
            r.path.display(),
            if r.external { "  (symlink)" } else { "" }
        );
        println!("status:      {}", status_detail(&r.status));
        println!(
            "tags:        {}",
            if r.tags.is_empty() {
                "-".into()
            } else {
                r.tags.join(", ")
            }
        );
        println!(
            "source:      {}",
            r.source
                .as_ref()
                .map(|s| s.summary())
                .unwrap_or_else(|| "-".into())
        );
        let dep: Vec<String> = r
            .deploy
            .iter()
            .map(|(a, s)| format!("{a}={}", deploy_label(s)))
            .collect();
        println!("deploy:      {}", dep.join("  "));
        if let Some(h) = &r.current_hash {
            println!("hash:        {}", &h[..std::cmp::min(h.len(), 23)]);
        }
        if let Some(n) = &r.note {
            println!("note:\n{}", indent(n));
        }
        if let Some(d) = &r.description {
            println!("description:\n{}", indent(d));
        }
    })
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn status_detail(s: &SkillStatus) -> String {
    match s {
        SkillStatus::Managed { no_baseline: true } => "managed (no baseline)".into(),
        SkillStatus::Renamed { to } => format!("renamed? -> {to}"),
        SkillStatus::Invalid { reason } => format!("invalid: {reason}"),
        SkillStatus::CorruptMeta { error } => format!("corrupt-meta: {error}"),
        other => other.label().into(),
    }
}

fn deploy_label(s: &DeployState) -> &'static str {
    match s {
        DeployState::Deployed => "yes",
        DeployState::NotDeployed => "no",
        DeployState::Shadow { same_content: true } => "shadow(same)",
        DeployState::Shadow {
            same_content: false,
        } => "shadow(differs)",
        DeployState::Foreign => "foreign",
        DeployState::Broken => "broken",
        DeployState::NoAgentDir => "no-dir",
    }
}

fn cmd_status(ctx: &Ctx) -> Result<()> {
    let snap = ctx.ws.scan()?;
    ctx.out(&snap, || print_status(&snap))
}

fn print_status(snap: &Snapshot) {
    println!("root: {}", snap.root.display());
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &snap.skills {
        *counts.entry(s.status.label()).or_default() += 1;
    }
    let summary: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
    println!("skills: {} ({})", snap.skills.len(), summary.join(", "));
    for s in snap.skills.iter().filter(|s| !s.status.is_healthy()) {
        println!("  {:<24} {}", s.key, status_detail(&s.status));
    }
    for s in snap.skills.iter().filter(|s| s.name_mismatch) {
        println!(
            "  {:<24} warning: frontmatter name {:?} differs from directory",
            s.key,
            s.name.as_deref().unwrap_or("")
        );
    }
    println!();
    for a in &snap.agents {
        print_agent(a);
    }
}

fn print_agent(a: &skills::reconcile::AgentReport) {
    let mode = match &a.mode {
        AgentDirMode::Missing => "missing".to_string(),
        AgentDirMode::SharedRoot => "shared skills root".into(),
        AgentDirMode::DirLinked => "dir-linked (whole directory -> root)".into(),
        AgentDirMode::DirForeign { target } => format!("dir-foreign -> {}", target.display()),
        AgentDirMode::Real => {
            let d = a.valid_count(|s| matches!(s, skills::reconcile::EntryState::Deployed));
            let other = a.documents.len() - d;
            format!(
                "real dir, {d} deployed{}",
                if other > 0 {
                    format!(", {other} other")
                } else {
                    String::new()
                }
            )
        }
    };
    println!("agent {} ({}): {}", a.key, a.skills_dir.display(), mode);
    for (name, st) in &a.entries {
        if !matches!(st, skills::reconcile::EntryState::Deployed) {
            println!("  {:<24} {}", name, entry_detail(st));
        }
    }
}

fn entry_detail(s: &skills::reconcile::EntryState) -> String {
    use skills::reconcile::EntryState::*;
    match s {
        Deployed => "deployed".into(),
        Broken { target } => format!("broken -> {}", target.display()),
        Foreign { target } => format!("foreign -> {}", target.display()),
        Shadow { same_content } => format!(
            "shadow ({})",
            if *same_content {
                "same content"
            } else {
                "differs"
            }
        ),
        AgentOnly => "agent-only (not in root)".into(),
    }
}

fn cmd_tag(ctx: &Ctx, c: TagCommand) -> Result<()> {
    match c {
        TagCommand::Add { skill, tags } => {
            let m = edit::tag_add(&ctx.ws, &skill, &tags)?;
            ctx.out(&m, || println!("{skill}: {}", m.tags.join(", ")))
        }
        TagCommand::Remove { skill, tags } => {
            let m = edit::tag_remove(&ctx.ws, &skill, &tags)?;
            ctx.out(&m, || println!("{skill}: {}", m.tags.join(", ")))
        }
        TagCommand::Set { skill, tags } => {
            let m = edit::tag_set(&ctx.ws, &skill, &tags)?;
            ctx.out(&m, || println!("{skill}: {}", m.tags.join(", ")))
        }
        TagCommand::List { skill: Some(skill) } => {
            let m = ctx.ws.meta.load(&skill)?.unwrap_or_default();
            ctx.out(&m.tags, || println!("{}", m.tags.join("\n")))
        }
        TagCommand::List { skill: None } => {
            let snap = ctx.ws.scan()?;
            let tags = snap.all_tags();
            ctx.out(&tags, || {
                for (t, n) in &tags {
                    println!("{t:<20} {n}");
                }
            })
        }
        TagCommand::Rename { old, new } => {
            let n = edit::tag_rename(&ctx.ws, &old, &new)?;
            ctx.out(
                &serde_json::json!({"renamed": {"from": old, "to": new}, "skills": n}),
                || println!("renamed tag on {n} skill(s)"),
            )
        }
        TagCommand::Delete { tag, yes } => {
            if !yes {
                bail!("refusing to delete tag {tag:?} from every skill without --yes");
            }
            let n = edit::tag_delete(&ctx.ws, &tag)?;
            ctx.out(&serde_json::json!({"deleted": tag, "skills": n}), || {
                println!("removed tag from {n} skill(s)")
            })
        }
    }
}

fn cmd_note(ctx: &Ctx, c: NoteCommand) -> Result<()> {
    match c {
        NoteCommand::Get { skill } => {
            let m = ctx.ws.meta.load(&skill)?.unwrap_or_default();
            ctx.out(&m.note, || {
                if let Some(n) = &m.note {
                    println!("{n}");
                }
            })
        }
        NoteCommand::Set { skill, text } => {
            let text = if text == "-" {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                s
            } else {
                text
            };
            let m = edit::note_set(&ctx.ws, &skill, Some(&text))?;
            ctx.out(&m, || println!("note set on {skill}"))
        }
        NoteCommand::Clear { skill } => {
            let m = edit::note_set(&ctx.ws, &skill, None)?;
            ctx.out(&m, || println!("note cleared on {skill}"))
        }
        NoteCommand::Edit { skill } => {
            let m = ctx.ws.meta.load(&skill)?.unwrap_or_default();
            match edit_in_editor(m.note.as_deref().unwrap_or(""))? {
                Some(text) => {
                    let m = edit::note_set(&ctx.ws, &skill, Some(&text))?;
                    ctx.out(&m, || println!("note saved on {skill}"))
                }
                None => ctx.out(&m, || println!("note unchanged on {skill}")),
            }
        }
    }
}

/// An unchanged temporary file is a no-op, including an editor's successful
/// discard command (such as `:q!`). Failed editors never supply text to save.
pub fn edit_in_editor(initial: &str) -> Result<Option<String>> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!("skills-note-{}.md", std::process::id()));
    std::fs::write(&path, initial)?;
    let outcome = (|| {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(&path)
            .status()
            .with_context(|| format!("launching editor {editor}"))?;
        if !status.success() {
            bail!("editor exited with {status}; note edit abandoned");
        }
        let text = std::fs::read_to_string(&path)?;
        Ok((text != initial).then_some(text))
    })();
    let _ = std::fs::remove_file(&path);
    outcome
}

fn cmd_install(ctx: &Ctx, a: InstallArgs) -> Result<()> {
    let r = install::parse_ref(&a.reference, a.branch.as_deref(), a.subpath.as_deref())?;
    if matches!(r, install::InstallRef::Git { .. }) {
        let fetched =
            skills::repository::FetchedRepository::fetch(&ctx.ws, &r, a.repo_alias.as_deref())?;
        let result = (|| {
            let paths: Vec<String> = if a.all {
                fetched
                    .choices
                    .iter()
                    .filter(|p| {
                        !fetched.invalid.contains_key(*p)
                            && !fetched.choices.iter().any(|ancestor| {
                                !fetched.invalid.contains_key(ancestor)
                                    && skills::repository::overlaps(ancestor, p)
                            })
                    })
                    .cloned()
                    .collect()
            } else if !a.select.is_empty() {
                a.select
                    .iter()
                    .map(|s| if s == "." { String::new() } else { s.clone() })
                    .collect()
            } else if let install::InstallRef::Git {
                subpath: Some(path),
                ..
            } = &r
            {
                if fetched.choices.contains(path) {
                    vec![path.clone()]
                } else {
                    vec![]
                }
            } else if fetched.choices.len() == 1 {
                fetched.choices.clone()
            } else {
                vec![]
            };
            if a.list || paths.is_empty() {
                return ctx.out(&serde_json::json!({"repository": fetched.repository, "choices": fetched.choices, "invalid": fetched.invalid, "installed": []}), || {
                    println!("{} — select paths with --select PATH or --all", fetched.repository.alias);
                    for path in &fetched.choices {
                        let display = if path.is_empty() { "." } else { path };
                        if let Some(error) = fetched.invalid.get(path) { println!("{display} [invalid: {error}]"); }
                        else { println!("{display}"); }
                    }
                });
            }
            let mut names = BTreeMap::new();
            for pair in &a.local_names {
                let (path, name) = pair.split_once('=').context("expected PATH=NAME")?;
                names.insert(
                    if path == "." {
                        String::new()
                    } else {
                        path.into()
                    },
                    name.into(),
                );
            }
            if let Some(name) = &a.name {
                if paths.len() != 1 {
                    bail!("--name requires exactly one selection")
                }
                names.insert(paths[0].clone(), name.clone());
            }
            let mut notices = Vec::new();
            let keys = fetched.install_with_progress(&ctx.ws, &paths, &names, &mut |message| {
                if message.starts_with("Warning:") || message.starts_with("Already installed:") {
                    notices.push(message.to_string());
                }
            })?;

            let actions = if a.deploy_to.is_empty() {
                vec![]
            } else {
                let snap = ctx.ws.scan()?;
                let plan = deploy::plan_deploy(&ctx.ws, &snap, &keys, &a.deploy_to)?;
                let actions = deploy::resolve_names(&snap, &plan, ctx.same_name.as_deref())?;
                deploy::apply(&actions)?;
                actions
            };
            ctx.out(
                &serde_json::json!({"installed": keys, "actions": actions, "notices": notices}),
                || {
                    for notice in &notices {
                        println!("{notice}");
                    }
                    for key in &keys {
                        println!("installed {key}");
                    }
                },
            )
        })();
        fetched.cleanup();
        return result;
    }
    let key = install::install(&ctx.ws, &r, a.name.as_deref())?;
    let mut actions = Vec::new();
    if !a.deploy_to.is_empty() {
        let snap = ctx.ws.scan()?;
        actions = deploy::plan_deploy(&ctx.ws, &snap, std::slice::from_ref(&key), &a.deploy_to)?;
        actions = deploy::resolve_names(&snap, &actions, ctx.same_name.as_deref())?;
        deploy::apply(&actions)?;
    }
    ctx.out(
        &serde_json::json!({"installed": key, "actions": actions}),
        || {
            println!("installed {key}");
            for a in &actions {
                println!("{}", a.describe());
            }
        },
    )
}

fn cmd_check(ctx: &Ctx, skill: Option<String>, all: bool) -> Result<()> {
    let keys: Vec<String> = if all {
        let snap = ctx.ws.scan()?;
        snap.skills
            .iter()
            .filter(|s| matches!(s.source, Some(skills::meta::Source::Git { .. })))
            .map(|s| s.key.clone())
            .collect()
    } else {
        vec![skill.context("pass a skill or --all")?]
    };
    let mut results = Vec::new();
    let mut errors = Vec::new();
    for k in keys {
        match update::check(&ctx.ws, &k) {
            Ok(r) => results.push(r),
            Err(e) => errors.push(serde_json::json!({"skill": k, "error": format!("{e:#}")})),
        }
    }
    ctx.out(
        &serde_json::json!({"results": results, "errors": errors}),
        || {
            for r in &results {
                println!(
                    "{:<24} {}  {} -> {}",
                    r.skill,
                    if r.update_available {
                        "UPDATE"
                    } else {
                        "ok    "
                    },
                    r.installed
                        .as_deref()
                        .map(skills::meta::short_rev)
                        .unwrap_or("-"),
                    skills::meta::short_rev(&r.remote)
                );
            }
            for e in &errors {
                println!(
                    "{:<24} error: {}",
                    e["skill"].as_str().unwrap_or(""),
                    e["error"].as_str().unwrap_or("")
                );
            }
        },
    )
}

fn cmd_update(ctx: &Ctx, a: UpdateArgs) -> Result<()> {
    let snap = ctx.ws.scan()?;
    let keys: Vec<String> = if let Some(alias) = &a.repo {
        snap.skills
            .iter()
            .filter(|s| skills::repository::alias_of(&s.key) == Some(alias.as_str()))
            .map(|s| s.key.clone())
            .collect()
    } else if a.all {
        snap.skills
            .iter()
            .filter(|s| matches!(s.source, Some(skills::meta::Source::Git { .. })))
            .filter(|s| {
                matches!(
                    s.status,
                    SkillStatus::Managed { .. } | SkillStatus::Modified
                )
            })
            .map(|s| s.key.clone())
            .collect()
    } else {
        vec![a.skill.clone().context("pass a skill or --all")?]
    };
    let take = if a.force {
        Some(Take::Upstream)
    } else {
        a.take
    };
    anyhow::ensure!(
        a.take_file.is_empty(),
        "choose --take local or --take upstream for the whole skill; per-file merging is not supported"
    );
    let per_file = BTreeMap::new();
    let mut report = Vec::new();
    for k in keys {
        let prepared = match update::prepare(&ctx.ws, &snap, &k) {
            Ok(prepared) => prepared,
            Err(error) => {
                report.push(serde_json::json!({"skill": k, "result": "skipped", "reason": format!("{error:#}")}));
                continue;
            }
        };

        let up_to_date = prepared.from_revision.as_deref() == Some(prepared.to_revision.as_str());
        let mut entry = serde_json::to_value(&prepared)?;
        if up_to_date {
            entry["result"] = "up-to-date".into();
            prepared.cleanup();
        } else if a.dry_run {
            entry["result"] = "dry-run".into();
            prepared.cleanup();
        } else if prepared.needs_resolution() && take.is_none() {
            entry["result"] = "needs-resolution".into();
            prepared.cleanup();
        } else {
            update::apply(&ctx.ws, &prepared, take.unwrap_or_default(), &per_file)?;
            entry["result"] = if take == Some(Take::Local) {
                "skipped-local"
            } else {
                "updated"
            }
            .into();
        }
        report.push(entry);
    }
    ctx.out(&report, || {
        for e in &report {
            if let Some(reason) = e["reason"].as_str() {
                println!("{}: {reason}", e["skill"].as_str().unwrap_or(""));
            }
            if let Some(new) = e["new_skills"].as_array()
                && !new.is_empty()
            {
                println!(
                    "New upstream skills (not installed): {}",
                    new.iter()
                        .filter_map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let skill = e["skill"].as_str().unwrap_or("");
            let result = e["result"].as_str().unwrap_or("");
            println!(
                "{skill:<24} {result}  {} -> {}",
                e["from_revision"]
                    .as_str()
                    .map(skills::meta::short_rev)
                    .unwrap_or("-"),
                e["to_revision"]
                    .as_str()
                    .map(skills::meta::short_rev)
                    .unwrap_or("-")
            );
            if result == "needs-resolution" || (result == "dry-run" && e["status"] == "modified") {
                if let Some(files) = e["files"].as_object() {
                    for (f, c) in files {
                        if c != "unchanged" {
                            println!("    {:<16} {f}", c.as_str().unwrap_or(""));
                        }
                    }
                }
                if result == "needs-resolution" {
                    println!(
                        "    pass --take local|upstream (and --take-file PATH=SIDE for exceptions)"
                    );
                }
            }
        }
    })
}

fn resolve_agents(ctx: &Ctx, agents: &[String], all: bool) -> Result<Vec<String>> {
    if all {
        return Ok(ctx.ws.config.agent_keys());
    }
    if agents.is_empty() {
        bail!("pass --agent <key> or --all-agents");
    }
    Ok(agents.to_vec())
}

fn cmd_deploy(ctx: &Ctx, a: DeployArgs, on: bool) -> Result<()> {
    if a.skills.is_empty() {
        bail!("pass at least one skill");
    }
    let agents = resolve_agents(ctx, &a.agents, a.all_agents)?;
    let snap = ctx.ws.scan()?;
    let actions = if on {
        deploy::plan_deploy(&ctx.ws, &snap, &a.skills, &agents)?
    } else {
        deploy::plan_undeploy(&ctx.ws, &snap, &a.skills, &agents)?
    };
    run_actions(ctx, &actions, a.dry_run)
}

fn run_actions(ctx: &Ctx, actions: &[Action], dry_run: bool) -> Result<()> {
    let snap = ctx.ws.scan()?;
    let conflicts = deploy::name_conflicts(&snap, actions);
    let resolved = if dry_run && ctx.same_name.is_none() {
        actions.to_vec()
    } else {
        deploy::resolve_names(&snap, actions, ctx.same_name.as_deref())?
    };
    let actions = resolved.as_slice();
    let applied = if dry_run { 0 } else { deploy::apply(actions)? };
    let changes = actions.iter().filter(|a| a.is_change()).count();
    ctx.out(
        &serde_json::json!({"dry_run": dry_run, "applied": applied, "actions": actions, "name_conflicts": conflicts}),
        || {
            for a in actions {
                println!("{}", a.describe());
            }
            if dry_run {
                println!("dry run: {changes} change(s) not applied");
            } else {
                println!("applied {applied} change(s)");
            }
        },
    )
}

fn cmd_agents(ctx: &Ctx, c: Option<AgentsCommand>) -> Result<()> {
    match c.unwrap_or(AgentsCommand::List) {
        AgentsCommand::Catalog => ctx.out(&skills::agents::BUILTINS, || {
            for a in skills::agents::BUILTINS {
                println!("{:<18} {:<28} {}", a.key, a.global_dir, a.local_dir);
            }
        }),
        AgentsCommand::Add { agent, dir } => {
            let local = ctx.ws.project.is_some();
            let entry = match dir {
                Some(skills_dir) => skills::config::AgentConfig {
                    key: agent.clone(),
                    name: agent,
                    skills_dir,
                },
                None => skills::agents::BUILTINS
                    .iter()
                    .find(|a| a.key == agent)
                    .with_context(|| {
                        format!("unknown built-in agent: {agent}; use agents catalog or --dir")
                    })?
                    .config(local),
            };
            if let Some(project) = &ctx.ws.project {
                paths::ensure_local_path(project, &project.join(entry.skills_path()))?;
            }
            skills::config::Config::add_agent(&ctx.ws.root, &entry, local)?;
            ctx.out(&entry, || {
                println!("added {}: {}", entry.key, entry.skills_dir)
            })
        }
        AgentsCommand::List => {
            let rows: Vec<serde_json::Value> = ctx
                .ws
                .config
                .agents
                .iter()
                .map(|a| serde_json::json!({"key": a.key, "name": a.display_name(), "skills_dir": a.skills_dir}))
                .collect();
            ctx.out(&rows, || {
                for a in &ctx.ws.config.agents {
                    println!("{:<10} {:<14} {}", a.key, a.display_name(), a.skills_dir);
                }
            })
        }
        AgentsCommand::Status { agent } => {
            let snap = ctx.ws.scan()?;
            let reports: Vec<_> = snap
                .agents
                .iter()
                .filter(|a| agent.as_deref().map(|k| k == a.key).unwrap_or(true))
                .collect();
            if reports.is_empty() {
                bail!("unknown agent");
            }
            ctx.out(&reports, || {
                for a in &reports {
                    print_agent(a);
                }
            })
        }
        AgentsCommand::Convert {
            agent,
            dry_run,
            yes,
        } => {
            let snap = ctx.ws.scan()?;
            let actions = deploy::plan_convert(&ctx.ws, &snap, &agent)?;
            if !dry_run && !yes {
                bail!(
                    "converting replaces the whole-directory link; re-run with --yes (or --dry-run to preview)"
                );
            }
            run_actions(ctx, &actions, dry_run)
        }
        AgentsCommand::Clean {
            agent,
            skills,
            dry_run,
            yes,
        } => {
            let snap = ctx.ws.scan()?;
            let actions = deploy::plan_clean(&ctx.ws, &snap, &agent, &skills)?;
            gate(&actions, dry_run, yes, "cleaning removes links")?;
            run_actions(ctx, &actions, dry_run)
        }
        AgentsCommand::RemoveLink {
            agent,
            name,
            dry_run,
            yes,
        } => run_foreign_link(
            ctx,
            &agent,
            &name,
            skills::ops::agent_links::Repair::Remove,
            dry_run,
            yes,
        ),
        AgentsCommand::AdoptLink {
            agent,
            name,
            dry_run,
            yes,
        } => run_foreign_link(
            ctx,
            &agent,
            &name,
            skills::ops::agent_links::Repair::Adopt,
            dry_run,
            yes,
        ),
        AgentsCommand::Relink {
            agent,
            skills,
            dry_run,
            yes,
        } => {
            let snap = ctx.ws.scan()?;
            let actions = deploy::plan_relink(&ctx.ws, &snap, &agent, &skills)?;
            gate(
                &actions,
                dry_run,
                yes,
                "relinking deletes the agent's own copies",
            )?;
            run_actions(ctx, &actions, dry_run)
        }
    }
}

fn run_foreign_link(
    ctx: &Ctx,
    agent: &str,
    name: &str,
    operation: skills::ops::agent_links::Repair,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let plan = skills::ops::agent_links::plan(&ctx.ws, agent, name, operation)?;
    if dry_run {
        return ctx.out(&plan, || {
            println!(
                "{operation:?} {} -> {}; external target will be preserved",
                plan.path.display(),
                plan.target.display()
            )
        });
    }
    if !yes {
        bail!("changing an agent link requires --yes (or --dry-run to preview)");
    }
    let message = plan.apply(&ctx.ws)?;
    ctx.out(
        &serde_json::json!({"message": message, "plan": plan}),
        || println!("{message}"),
    )
}

/// The `--yes` check for a plan that deletes something. A plan with nothing
/// in it but skips is let through without the flag: there is nothing to
/// consent to, and the skips are the answer being asked for.
fn gate(actions: &[Action], dry_run: bool, yes: bool, what: &str) -> Result<()> {
    if !dry_run && !yes && actions.iter().any(|a| a.is_change()) {
        bail!("{what}; re-run with --yes (or --dry-run to preview)");
    }
    Ok(())
}

fn cmd_preset(ctx: &Ctx, c: PresetCommand) -> Result<()> {
    use skills::preset::Preset;
    let store = &ctx.ws.presets;
    match c {
        PresetCommand::List => {
            let list = store.list()?;
            ctx.out(&list, || {
                for p in &list {
                    println!(
                        "{:<20} {:>3} skills  agents: {}",
                        p.name,
                        p.skills.len(),
                        if p.agents.is_empty() {
                            "all".into()
                        } else {
                            p.agents.join(",")
                        }
                    );
                }
            })
        }
        PresetCommand::Show { name } => {
            let p = store
                .load(&name)?
                .with_context(|| format!("no such preset: {name}"))?;
            ctx.out(&p, || {
                println!("{}", toml::to_string_pretty(&p).unwrap_or_default());
            })
        }
        PresetCommand::Create {
            name,
            description,
            agents,
            skills,
        } => {
            if store.load(&name)?.is_some() {
                bail!("preset {name} already exists");
            }
            for a in &agents {
                ctx.ws
                    .config
                    .agent(a)
                    .with_context(|| format!("unknown agent: {a}"))?;
            }
            let p = Preset {
                name: name.clone(),
                description,
                skills,
                agents,
            };
            store.save(&p)?;
            ctx.out(&p, || println!("created preset {name}"))
        }
        PresetCommand::Delete { name, yes } => {
            if !yes {
                bail!("refusing to delete preset {name} without --yes");
            }
            store.remove(&name)?;
            ctx.out(&serde_json::json!({"deleted": name}), || {
                println!("deleted preset {name}")
            })
        }
        PresetCommand::Add { name, skills } => {
            let mut p = store
                .load(&name)?
                .with_context(|| format!("no such preset: {name}"))?;
            for s in skills {
                if !p.skills.contains(&s) {
                    p.skills.push(s);
                }
            }
            store.save(&p)?;
            ctx.out(&p, || println!("{name}: {}", p.skills.join(", ")))
        }
        PresetCommand::Remove { name, skills } => {
            let mut p = store
                .load(&name)?
                .with_context(|| format!("no such preset: {name}"))?;
            p.skills.retain(|s| !skills.contains(s));
            store.save(&p)?;
            ctx.out(&p, || println!("{name}: {}", p.skills.join(", ")))
        }
        PresetCommand::Deploy {
            name,
            agents,
            dry_run,
        } => preset_links(ctx, &name, &agents, dry_run, true),
        PresetCommand::Undeploy {
            name,
            agents,
            dry_run,
        } => preset_links(ctx, &name, &agents, dry_run, false),
        // Both go through the same functions the TUI uses, so the message and
        // the config follow-through are the same from either side.
        PresetCommand::Rename { old, new } => {
            let (message, _) = history::preset_rename(&ctx.ws, &old, &new)?;
            ctx.out(
                &serde_json::json!({"renamed": {"from": old, "to": new}, "message": message}),
                || println!("{message}"),
            )
        }
        PresetCommand::Describe { name, text } => {
            let (message, _) = history::preset_description_edit(&ctx.ws, &name, text.as_deref())?;
            let p = store
                .load(&name)?
                .with_context(|| format!("no such preset: {name}"))?;
            ctx.out(
                &serde_json::json!({"preset": p, "message": message}),
                || println!("{message}"),
            )
        }
    }
}

fn preset_links(ctx: &Ctx, name: &str, agents: &[String], dry_run: bool, on: bool) -> Result<()> {
    let p = ctx
        .ws
        .presets
        .load(name)?
        .with_context(|| format!("no such preset: {name}"))?;
    let targets: Vec<String> = if !agents.is_empty() {
        agents.to_vec()
    } else if !p.agents.is_empty() {
        p.agents.clone()
    } else {
        ctx.ws.config.agent_keys()
    };
    let snap = ctx.ws.scan()?;
    // Members that are absent are reported as skips instead of aborting the whole preset.
    let (present, absent): (Vec<String>, Vec<String>) = p
        .skills
        .iter()
        .cloned()
        .partition(|s| snap.get(s).is_some());
    let mut actions = if on {
        deploy::plan_deploy(&ctx.ws, &snap, &present, &targets)?
    } else {
        deploy::plan_undeploy(&ctx.ws, &snap, &present, &targets)?
    };
    for s in absent {
        actions.push(Action::Skip {
            agent: "*".into(),
            skill: s,
            reason: "not in skills root".into(),
        });
    }
    run_actions(ctx, &actions, dry_run)
}

#[cfg(test)]
mod tui_library_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn project_launch_selects_deployment_context_without_opening_a_second_library() {
        let base = std::env::temp_dir().join(format!("skills-tui-library-{}", std::process::id()));
        let root = base.join("library");
        let project = base.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let mut cli = Cli::parse_from(["skills", "--root", root.to_str().unwrap()]);
        // These fields are deliberately set directly to keep the test independent
        // of process-wide SKILLS_HOME while checking project launch semantics.
        cli.project = Some(project.clone());
        cli.local = true;
        let ws = cli.tui_workspace().unwrap();
        assert_eq!(ws.root, root.canonicalize().unwrap());
        assert!(ws.project.is_none());
        assert!(!project.join(".agents").exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
