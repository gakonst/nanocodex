//! Owner-only managed thread sharing commands and presentation.

use nanocodex_managed::{CreatedShareLink, ManagedError, ShareLink, SharePermission};

const USAGE: &str = "Share this managed thread\n/share read  create a view-only link\n/share write  create a comment link\n/share list  list active links\n/share revoke <id>  revoke a link\nAnyone holding a link can view the conversation until revoked. Treat it as a secret; guest comments are not prompts.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Help,
    Create(SharePermission),
    List,
    Revoke(String),
}

impl Command {
    pub(crate) fn parse(input: &str) -> Option<Result<Self, String>> {
        let mut args = input.split_whitespace();
        if args.next()? != "/share" {
            return None;
        }
        let action = args.next();
        let second = args.next();
        let extra = args.next();
        Some(match (action, second, extra) {
            (None, None, None) => Ok(Self::Help),
            (Some("read"), None, None) => Ok(Self::Create(SharePermission::Read)),
            (Some("write"), None, None) => Ok(Self::Create(SharePermission::Write)),
            (Some("list"), None, None) => Ok(Self::List),
            (Some("revoke"), Some(id), None)
                if id.len() == 36
                    && uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id) =>
            {
                Ok(Self::Revoke(id.to_owned()))
            }
            _ => Err("Usage: /share [read|write|list|revoke <link UUID>]".into()),
        })
    }
}

pub(crate) enum Outcome {
    Created(CreatedShareLink),
    Listed(Vec<ShareLink>),
    Revoked,
}

pub(crate) fn help() -> String {
    USAGE.into()
}

pub(crate) fn list_text(links: &[ShareLink]) -> String {
    if links.is_empty() {
        return "No active share links. Use /share read or /share write to create one.".into();
    }
    let mut text = "Active share links (no bearer URLs; only available at creation):\n".to_owned();
    for link in links {
        let mode = match link.permission {
            SharePermission::Read => "view",
            SharePermission::Write => "comment",
        };
        text.push_str(&format!(
            "\n{} · {} · created {} (Unix ms)",
            link.id, mode, link.created_at
        ));
    }
    text.push_str("\n\n/share revoke <id> to revoke access.");
    text
}

pub(crate) fn error(error: &ManagedError) -> String {
    match error {
        ManagedError::Http { status, .. } if status.as_u16() == 401 || status.as_u16() == 403 =>
            "Sharing requires owner account access. Sign in or check this API key's permissions.".into(),
        ManagedError::Http { status, .. } if status.as_u16() == 404 =>
            "Managed thread or share link not found (it may have been revoked).".into(),
        ManagedError::Http { status, .. } if status.as_u16() == 429 =>
            "Too many active share links. Revoke one before creating another.".into(),
        ManagedError::Transport(_) =>
            "Could not confirm the share request. If creating or revoking, check /share list before trying again; the request may have succeeded.".into(),
        _ => "Sharing failed. Check the managed connection and /share list before retrying a mutation.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_boundary_consumes_every_share_variant_and_rejects_invalid_input() {
        assert_eq!(Command::parse("/share"), Some(Ok(Command::Help)));
        assert_eq!(
            Command::parse("/share read"),
            Some(Ok(Command::Create(SharePermission::Read)))
        );
        assert_eq!(
            Command::parse("/share write"),
            Some(Ok(Command::Create(SharePermission::Write)))
        );
        assert_eq!(Command::parse("/share list"), Some(Ok(Command::List)));
        assert_eq!(
            Command::parse("/share revoke 00000000-0000-4000-8000-000000000001"),
            Some(Ok(Command::Revoke(
                "00000000-0000-4000-8000-000000000001".into()
            )))
        );
        for input in [
            "/share invalid",
            "/share read extra",
            "/share revoke",
            "/share revoke a/b",
            "/share list extra",
        ] {
            assert!(matches!(Command::parse(input), Some(Err(_))), "{input}");
        }
        assert_eq!(Command::parse("/shared read"), None);
    }
}
