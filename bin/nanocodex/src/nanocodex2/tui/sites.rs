//! Owner-only commands for static sites published from a managed thread.

use nanocodex_managed::{
    ManagedError, PublishSite, PublishedSite, Site, SiteShare, SiteView, valid_site_id,
};

const USAGE: &str = "Static sites from this thread\n/sites  list sites, versions, and links\n/sites publish <path> [site-id]  publish a directory or file, such as /workspace/app/dist\n/sites open <site-id> [version]  open a private preview for an hour\n/sites share <site-id> [version]  create a public link to a version\n/sites revoke <site-id> <link-id>  turn a link off\nPublishing is private. Anyone with a shared link can open that version until you revoke it. .env files, keys, .git, and node_modules are never published.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Help,
    List,
    Publish { path: String, id: Option<String> },
    Open { site: String, version: Option<u64> },
    Share { site: String, version: Option<u64> },
    Revoke { site: String, link: String },
}

impl Command {
    pub(crate) fn parse(input: &str) -> Option<Result<Self, String>> {
        let mut args = input.split_whitespace();
        if args.next()? != "/sites" {
            return None;
        }
        let args: Vec<&str> = args.collect();
        let site = |value: &str| valid_site_id(value).then(|| value.to_owned());
        let version = |value: Option<&&str>| match value {
            None => Some(None),
            Some(value) => value
                .parse::<u64>()
                .ok()
                .filter(|version| *version > 0)
                .map(Some),
        };
        let link = |value: &str| {
            uuid::Uuid::parse_str(value)
                .is_ok_and(|parsed| parsed.to_string() == value)
                .then(|| value.to_owned())
        };
        let parsed = match args.as_slice() {
            [] | ["list"] => Some(Self::List),
            ["help"] => Some(Self::Help),
            ["publish", path] if path.starts_with('/') => Some(Self::Publish {
                path: (*path).to_owned(),
                id: None,
            }),
            ["publish", path, id] if path.starts_with('/') => site(id).map(|id| Self::Publish {
                path: (*path).to_owned(),
                id: Some(id),
            }),
            ["open", id, rest @ ..] if rest.len() <= 1 => site(id)
                .zip(version(rest.first()))
                .map(|(site, version)| Self::Open { site, version }),
            ["share", id, rest @ ..] if rest.len() <= 1 => site(id)
                .zip(version(rest.first()))
                .map(|(site, version)| Self::Share { site, version }),
            ["revoke", id, value] => site(id)
                .zip(link(value))
                .map(|(site, link)| Self::Revoke { site, link }),
            _ => None,
        };
        Some(parsed.ok_or_else(|| "Usage: /sites [list|publish <path> [id]|open <id> [version]|share <id> [version]|revoke <id> <link UUID>]".into()))
    }
}

pub(crate) enum Outcome {
    Listed(Vec<Site>),
    Published(PublishedSite),
    Opened(SiteView),
    Shared(SiteShare),
    Revoked,
}

pub(crate) fn help() -> String {
    USAGE.into()
}

pub(crate) fn list_text(sites: &[Site]) -> String {
    if sites.is_empty() {
        return "No sites yet. Ask the agent to publish a build, or run /sites publish <path>."
            .into();
    }
    let mut text = String::new();
    for site in sites {
        text.push_str(&format!(
            "{} · {} · latest v{}\n",
            site.id, site.title, site.latest_version
        ));
        if let Some(latest) = site.versions.first() {
            text.push_str(&format!(
                "  v{}: {} files, {} from {}\n",
                latest.version,
                latest.files,
                bytes(latest.bytes),
                latest.source
            ));
        }
        if site.shares.is_empty() {
            text.push_str("  No public links\n");
        }
        for share in &site.shares {
            let expiry = share.expires_at.map_or_else(String::new, |expires| {
                format!(" · expires {expires} (Unix ms)")
            });
            text.push_str(&format!(
                "  v{} link {}{expiry}\n  {}\n",
                share.version, share.id, share.url
            ));
        }
        text.push('\n');
    }
    text.push_str(
        "/sites share <id> to create a link · /sites revoke <id> <link-id> to turn one off.",
    );
    text
}

pub(crate) fn published_text(site: &PublishedSite) -> String {
    let skipped = match site.excluded {
        0 => String::new(),
        1 => " Skipped 1 file that looked like a secret or dependency.".into(),
        count => format!(" Skipped {count} files that looked like secrets or dependencies."),
    };
    let state = if site.created {
        "Published"
    } else {
        "Already published"
    };
    format!(
        "{state} {} v{} ({} files, {}).{skipped}",
        site.site_id,
        site.version,
        site.files,
        bytes(site.bytes)
    )
}

pub(crate) fn error(error: &ManagedError) -> String {
    match error {
        ManagedError::Http { status, .. } if status.as_u16() == 401 || status.as_u16() == 403 =>
            "Sites require owner account access. Sign in or check this API key's permissions.".into(),
        ManagedError::Http { status, message, .. } if matches!(status.as_u16(), 400 | 404 | 413 | 422 | 429) => message.clone(),
        ManagedError::Http { status, .. } if status.as_u16() == 503 =>
            "Site publishing isn't configured for this account's service.".into(),
        ManagedError::Transport(_) =>
            "Could not confirm the sites request. Run /sites before retrying a publish, share, or revoke; it may have succeeded.".into(),
        _ => "The sites request failed. Run /sites before retrying a change.".into(),
    }
}

fn bytes(value: u64) -> String {
    match value {
        0..1_024 => format!("{value} B"),
        1_024..1_048_576 => format!("{:.1} KB", value as f64 / 1_024.0),
        _ => format!("{:.1} MB", value as f64 / 1_048_576.0),
    }
}

pub(crate) fn publish_request(path: String, id: Option<String>) -> PublishSite {
    PublishSite {
        path,
        id,
        ..PublishSite::default()
    }
}
