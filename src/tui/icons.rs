//! Optional Nerd Font decorations; text mode needs no patched font.
//! Resolve decorations before measuring, fitting or assigning mouse hitboxes.
use skills::{config::Icons, meta::Source};

pub fn decoration<'a>(mode: Icons, nerd: &'a str, text: &'a str) -> &'a str {
    match mode {
        Icons::Nerd => nerd,
        Icons::Text => text,
    }
}

fn github(url: &str) -> bool {
    let host = if let Some((_, rest)) = url.split_once("://") {
        rest.split('/')
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
    } else if let Some((host, _)) = url.split_once(':') {
        host.rsplit('@').next().unwrap_or("")
    } else {
        return false;
    };
    host.eq_ignore_ascii_case("github.com")
}

pub fn git(mode: Icons, url: &str) -> &'static str {
    match (mode, github(url)) {
        (Icons::Nerd, true) => "󰊤",
        (Icons::Nerd, false) => "󰊢",
        (Icons::Text, true) => "github",
        (Icons::Text, false) => "git",
    }
}

pub fn source_icon(mode: Icons, source: &Source) -> &'static str {
    match source {
        Source::Git { url, .. } => git(mode, url),
        Source::Archive { .. } => match mode {
            Icons::Nerd => "󰏗",
            Icons::Text => "archive",
        },
        Source::Local { .. } => local(mode),
    }
}

pub fn branch(mode: Icons) -> &'static str {
    match mode {
        Icons::Nerd => "󰘬",
        Icons::Text => "branch",
    }
}

pub fn package(mode: Icons) -> &'static str {
    match mode {
        Icons::Nerd => "󰏗",
        Icons::Text => "installed",
    }
}

pub fn preset_caps(mode: Icons) -> (&'static str, &'static str) {
    match mode {
        Icons::Nerd => ("", ""),
        Icons::Text => ("/", "/"),
    }
}

pub fn tag_caps(mode: Icons, caps: skills::config::PillCaps) -> (&'static str, &'static str) {
    match mode {
        Icons::Nerd => caps.glyphs(),
        Icons::Text => ("(", ")"),
    }
}

pub fn local(mode: Icons) -> &'static str {
    match mode {
        Icons::Nerd => "󰉋 local",
        Icons::Text => "local",
    }
}

pub fn scope(mode: Icons, global: bool, repository: bool) -> &'static str {
    match (mode, global, repository) {
        (Icons::Text, _, _) => "",
        (Icons::Nerd, true, _) => "󰋜 ",
        (Icons::Nerd, false, true) => "󰊢 ",
        (Icons::Nerd, false, false) => "󰉋 ",
    }
}

pub fn source(mode: Icons, source: &Source) -> String {
    match source {
        Source::Git { url, .. } | Source::Archive { url, .. } => {
            let mut text = format!("{} {url}", source_icon(mode, source));
            if let Some(path) = source.subpath() {
                text.push_str(&format!(" · {path}"));
            }
            if let Some(name) = source.branch() {
                text.push_str(&format!(" · {} {name}", branch(mode)));
            }
            if let Some(rev) = source.revision() {
                text.push_str(&format!(" ({})", skills::meta::short_rev(rev)));
            }
            text
        }
        Source::Local { path } => match path {
            Some(path) => format!("{} {path}", local(mode)),
            None => local(mode).into(),
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn has_pua(text: &str) -> bool {
        text.chars()
            .any(|c| matches!(c as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd))
    }

    #[test]
    fn github_icons_require_the_actual_host() {
        for url in [
            "https://github.com/example/tools",
            "git@github.com:example/tools.git",
            "ssh://git@github.com:22/example/tools",
        ] {
            assert_eq!(git(Icons::Nerd, url), "󰊤");
            assert_eq!(git(Icons::Text, url), "github");
        }
        for url in [
            "https://github.com.example.org/tools",
            "https://example.org/github.com/tools",
            "https://github.com@example.org/tools",
            "/tmp/github.com/tools",
            "git@example.org:tools",
        ] {
            assert_eq!(git(Icons::Nerd, url), "󰊢");
            assert_eq!(git(Icons::Text, url), "git");
        }
    }

    #[test]
    fn archive_source_uses_archive_identity_even_when_hosted_on_github() {
        let archive = Source::Archive {
            url: "https://github.com/example/tools/releases/download/v1/skills.zip".into(),
            subpath: Some("bundle/review".into()),
            revision: Some("sha256:0123456789abcdef".into()),
        };
        assert_eq!(source_icon(Icons::Text, &archive), "archive");
        assert_ne!(
            source_icon(Icons::Nerd, &archive),
            git(Icons::Nerd, archive.url().unwrap())
        );
        let text = source(Icons::Text, &archive);
        assert!(text.starts_with("archive https://github.com/"));
        assert!(text.contains(" · bundle/review"));
        assert!(text.ends_with("(0123456789ab)"));
        assert!(!text.contains("branch"));
    }

    #[test]
    fn icon_configuration_is_optional_and_validated() {
        use skills::config::UiConfig;
        assert_eq!(toml::from_str::<UiConfig>("").unwrap().icons, Icons::Nerd);
        assert_eq!(
            toml::from_str::<UiConfig>("icons = 'nerd'").unwrap().icons,
            Icons::Nerd
        );
        assert_eq!(
            toml::from_str::<UiConfig>("icons = 'text'").unwrap().icons,
            Icons::Text
        );
        assert!(toml::from_str::<UiConfig>("icons = 'auto'").is_err());
    }
}
