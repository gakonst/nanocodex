//! Static sites published from a managed thread and their public links.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    ManagedClient, ManagedError,
    client::{agent_path, response_error, validate_id},
};

/// One immutable published version.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct SiteVersion {
    /// Version number, starting at 1.
    pub version: u64,
    /// Relative file served at `/`.
    pub entry: String,
    /// Number of published files.
    pub files: u64,
    /// Total published bytes.
    pub bytes: u64,
    /// Thread path the version was published from.
    pub source: String,
    /// Publication time as Unix milliseconds.
    pub created_at: u64,
}

/// An active public link to one site version.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct SiteShare {
    /// Revocable link ID.
    pub id: String,
    /// Site the link serves.
    pub site_id: String,
    /// Version the link serves; it never moves to a newer version.
    pub version: u64,
    /// Public URL. Anyone with it can open the version until it is revoked or expires.
    pub url: String,
    /// Creation time as Unix milliseconds.
    pub created_at: u64,
    /// Expiry as Unix milliseconds, if any.
    pub expires_at: Option<u64>,
}

/// A site and its versions and active links, newest version first.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct Site {
    /// Stable site ID within the thread.
    pub id: String,
    /// Display title.
    pub title: String,
    /// Newest version number.
    pub latest_version: u64,
    /// Creation time as Unix milliseconds.
    pub created_at: u64,
    /// Last publication time as Unix milliseconds.
    pub updated_at: u64,
    /// Every version, newest first.
    pub versions: Vec<SiteVersion>,
    /// Active public links.
    pub shares: Vec<SiteShare>,
}

/// A short-lived private URL for the owner to open one version.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct SiteView {
    /// Site being viewed.
    pub site_id: String,
    /// Version being viewed.
    pub version: u64,
    /// Private URL; it stops working at `expires_at`.
    pub url: String,
    /// Expiry as Unix milliseconds.
    pub expires_at: u64,
}

/// Options for publishing a thread path as a site version.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PublishSite {
    /// Absolute thread path, such as `/workspace/app/dist` or `/brain/outputs/report.html`.
    pub path: String,
    /// Stable site ID; defaults to a slug of the title or path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Display title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Relative file served at `/`; defaults to `index.html` or the only file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// Serve the entry for unknown extensionless paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spa: Option<bool>,
}

/// The receipt for one publish.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct PublishedSite {
    /// Site ID.
    pub site_id: String,
    /// Display title.
    pub title: String,
    /// Version created, or the identical latest version.
    pub version: u64,
    /// Relative file served at `/`.
    pub entry: String,
    /// Number of published files.
    pub files: u64,
    /// Total published bytes.
    pub bytes: u64,
    /// Files skipped because they look like secrets or dependencies.
    pub excluded: u64,
    /// False when identical content was already the latest version.
    pub created: bool,
}

#[derive(Deserialize)]
struct List<T> {
    data: Vec<T>,
}

fn sites_path(agent_id: &str) -> Result<String, ManagedError> {
    validate_id("agent", agent_id)?;
    Ok(format!("{}/sites", agent_path(agent_id)))
}

fn site_path(agent_id: &str, site_id: &str) -> Result<String, ManagedError> {
    if !valid_site_id(site_id) {
        return Err(ManagedError::Configuration(
            "site ID must be 1-63 lowercase letters, digits, or hyphens".into(),
        ));
    }
    Ok(format!("{}/{site_id}", sites_path(agent_id)?))
}

/// True for IDs the service accepts: 1-63 lowercase letters, digits, or hyphens.
pub fn valid_site_id(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_share_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.to_string() == value)
}

/// Site URLs are a 26-character random label on the sites zone, with no path or credentials.
fn valid_site_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "https" | "http")
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .host_str()
                .and_then(|host| host.split_once('.'))
                .is_some_and(|(label, _)| {
                    label.len() == 26
                        && label
                            .bytes()
                            .all(|byte| matches!(byte, b'a'..=b'z' | b'2'..=b'7'))
                })
    })
}

fn validate_share(share: &SiteShare) -> Result<(), ManagedError> {
    if !valid_share_id(&share.id) || !valid_site_id(&share.site_id) || !valid_site_url(&share.url) {
        return Err(ManagedError::InvalidResponse("invalid site link"));
    }
    Ok(())
}

impl ManagedClient {
    /// Lists the thread's sites with their versions and active links.
    pub async fn list_sites(&self, agent_id: &str) -> Result<Vec<Site>, ManagedError> {
        let response = self
            .request(Method::GET, &sites_path(agent_id)?, None, None)
            .await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let list: List<Site> = response.json().await.map_err(ManagedError::Transport)?;
        for site in &list.data {
            if !valid_site_id(&site.id) {
                return Err(ManagedError::InvalidResponse("invalid site metadata"));
            }
            site.shares.iter().try_for_each(validate_share)?;
        }
        Ok(list.data)
    }

    /// Publishes a thread path as a new private site version.
    pub async fn publish_site(
        &self,
        agent_id: &str,
        request: &PublishSite,
    ) -> Result<PublishedSite, ManagedError> {
        let body = serde_json::to_vec(request)
            .map_err(|_| ManagedError::InvalidResponse("invalid site publish request"))?;
        let response = self
            .request(Method::POST, &sites_path(agent_id)?, Some(&body), None)
            .await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let receipt: PublishedSite = response.json().await.map_err(ManagedError::Transport)?;
        if !valid_site_id(&receipt.site_id) || receipt.version == 0 {
            return Err(ManagedError::InvalidResponse(
                "invalid site publish receipt",
            ));
        }
        Ok(receipt)
    }

    /// Mints a private URL, valid for an hour, to open one version.
    pub async fn open_site(
        &self,
        agent_id: &str,
        site_id: &str,
        version: Option<u64>,
    ) -> Result<SiteView, ManagedError> {
        let body = serde_json::to_vec(&version_body(version))
            .map_err(|_| ManagedError::InvalidResponse("invalid site request"))?;
        let path = format!("{}/open", site_path(agent_id, site_id)?);
        let response = self.request(Method::POST, &path, Some(&body), None).await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let view: SiteView = response.json().await.map_err(ManagedError::Transport)?;
        if view.site_id != site_id || !valid_site_url(&view.url) {
            return Err(ManagedError::InvalidResponse("invalid site view"));
        }
        Ok(view)
    }

    /// Creates a public link to one version exactly once. On an uncertain
    /// transport failure, list sites rather than retrying.
    pub async fn create_site_share(
        &self,
        agent_id: &str,
        site_id: &str,
        version: Option<u64>,
    ) -> Result<SiteShare, ManagedError> {
        let body = serde_json::to_vec(&version_body(version))
            .map_err(|_| ManagedError::InvalidResponse("invalid site request"))?;
        let path = format!("{}/shares", site_path(agent_id, site_id)?);
        let response = self.request(Method::POST, &path, Some(&body), None).await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let share: SiteShare = response.json().await.map_err(ManagedError::Transport)?;
        validate_share(&share)?;
        if share.site_id != site_id || version.is_some_and(|version| version != share.version) {
            return Err(ManagedError::InvalidResponse("invalid site link receipt"));
        }
        Ok(share)
    }

    /// Revokes one public link. The next request to its URL fails.
    pub async fn revoke_site_share(
        &self,
        agent_id: &str,
        site_id: &str,
        share_id: &str,
    ) -> Result<(), ManagedError> {
        if !valid_share_id(share_id) {
            return Err(ManagedError::Configuration(
                "site link ID must be a UUID".into(),
            ));
        }
        let path = format!("{}/shares/{share_id}", site_path(agent_id, site_id)?);
        let response = self.request(Method::DELETE, &path, None, None).await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        Ok(())
    }
}

fn version_body(version: Option<u64>) -> serde_json::Value {
    version.map_or_else(
        || serde_json::json!({}),
        |version| serde_json::json!({ "version": version }),
    )
}
