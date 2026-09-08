//! Optional Nerd Font source decorations; text mode needs no patched font.
use skills::{config::Icons, meta::Source};

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

pub fn local(mode: Icons) -> &'static str {
    match mode {
        Icons::Nerd => "󰉋 local",
        Icons::Text => "local",
    }
}

pub fn source(mode: Icons, source: &Source) -> String {
    match source {
        Source::Git {
            url,
            subpath,
            branch: tracked,
            revision,
        } => {
            let mut text = format!("{} {url}", git(mode, url));
            if let Some(path) = subpath {
                text.push_str(&format!(" · {path}"));
            }
            if let Some(name) = tracked {
                text.push_str(&format!(" · {} {name}", branch(mode)));
            }
            if let Some(rev) = revision {
                text.push_str(&format!(" ({})", rev.chars().take(7).collect::<String>()));
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
mod tests {
    use super::*;

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
