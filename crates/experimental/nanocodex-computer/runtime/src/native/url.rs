//! Source-grounded native URL display; no resource is opened or fetched.
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub individual_limit: Option<usize>,
    pub total_limit: Option<usize>,
    pub include_query: bool,
    pub include_fragment: bool,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            individual_limit: None,
            total_limit: None,
            include_query: true,
            include_fragment: true,
        }
    }
}
#[derive(Default)]
pub struct Shortener {
    configuration: Configuration,
    total: usize,
    domains: BTreeMap<String, usize>,
    urls: BTreeSet<String>,
    retained_bytes: usize,
    budget_exhausted: bool,
}
struct DisplayUrl {
    display: String,
    host: Option<String>,
    file: bool,
}
impl Shortener {
    pub fn new(configuration: Configuration) -> Result<Self> {
        if configuration.individual_limit == Some(0) {
            return Err(Error::invalid("URL individual limit must be positive"));
        }
        Ok(Self {
            configuration,
            ..Default::default()
        })
    }
    pub fn configuration(&self) -> &Configuration {
        &self.configuration
    }
    pub fn total(&self) -> usize {
        self.total
    }
    pub fn budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }
    pub fn reset(&mut self) {
        self.total = 0;
        self.domains.clear();
        self.urls.clear();
        self.retained_bytes = 0;
        self.budget_exhausted = false;
    }
    pub fn trim(&mut self, url: &str, is_image: bool) -> Option<String> {
        // The native observation already bounds nodes and metadata. Keep an
        // independent ceiling for direct Rust callers of this helper.
        let new = !self.urls.contains(url);
        if url.len() > 1024 * 1024
            || (new
                && (self.urls.len() >= 5000
                    || self.retained_bytes.saturating_add(url.len()) > 8 * 1024 * 1024))
        {
            self.budget_exhausted = true;
            return None;
        }
        if new {
            self.retained_bytes += url.len();
            self.urls.insert(url.into());
        }
        let value = render_url(url, is_image, &self.configuration, &mut self.total)?;
        if value.file {
            return Some(value.display);
        }
        if let Some(host) = value.host {
            *self.domains.entry(host).or_default() += 1;
        }
        Some(value.display)
    }
    /// Recovered configuration tightening. This estimates each distinct URL,
    /// leaving the active render counter untouched as the native helper does.
    pub fn compact_if_needed(&mut self) -> usize {
        let limit = *self.configuration.total_limit.get_or_insert(4000);
        loop {
            let mut total = 0;
            for url in &self.urls {
                let _ = render_url(url, false, &self.configuration, &mut total);
            }
            if total <= limit {
                return total;
            }
            if self.configuration.include_fragment {
                self.configuration.include_fragment = false;
            } else if self.configuration.include_query {
                self.configuration.include_query = false;
            } else if let Some(next) = (20..=120).rev().step_by(10).find(|&candidate| {
                self.configuration
                    .individual_limit
                    .is_none_or(|current| current > candidate)
            }) {
                self.configuration.individual_limit = Some(next);
            } else {
                return total;
            }
        }
    }
}

fn render_url(
    url: &str,
    is_image: bool,
    configuration: &Configuration,
    total: &mut usize,
) -> Option<DisplayUrl> {
    let mut value = display_url(url, is_image, configuration)?;
    if value.file {
        return Some(value);
    }
    if let Some(limit) = configuration.individual_limit
        && value.display.graphemes(true).nth(limit).is_some()
    {
        value.display = value
            .display
            .graphemes(true)
            .take(limit.saturating_sub(1))
            .collect::<String>()
            + "…";
    }
    let count = value.display.graphemes(true).count();
    if configuration
        .total_limit
        .is_some_and(|limit| total.saturating_add(count) > limit)
    {
        value.display = "…".into();
        *total = total.saturating_add(1);
    } else {
        *total = total.saturating_add(count);
    }
    Some(value)
}

#[cfg(target_os = "macos")]
fn display_url(input: &str, is_image: bool, config: &Configuration) -> Option<DisplayUrl> {
    use objc2_foundation::{NSURL, NSURLComponents};
    let url = NSURL::URLWithString(&crate::selection::native_string(input))?;
    let scheme = url.scheme()?.to_string();
    if is_image || matches!(scheme.to_ascii_lowercase().as_str(), "webdoc" | "data") {
        return None;
    }
    if url.isFileURL() {
        return Some(DisplayUrl {
            display: url.absoluteString()?.to_string(),
            host: None,
            file: true,
        });
    }
    let parts = NSURLComponents::componentsWithURL_resolvingAgainstBaseURL(&url, false)?;
    if parts
        .scheme()
        .is_some_and(|value| matches!(value.to_string().as_str(), "http" | "https"))
    {
        parts.setScheme(None);
    }
    let host = parts
        .host()
        .map(|value| value.to_string())
        .map(|value| value.strip_prefix("www.").unwrap_or(&value).to_owned());
    if let Some(host) = &host {
        parts.setHost(Some(&crate::selection::native_string(host)));
    }
    // The original validates serialization once before removing optional fields.
    parts.string()?;
    if !config.include_query {
        parts.setQuery(None);
    }
    if !config.include_fragment {
        parts.setFragment(None);
    }
    let display = parts.string()?.to_string();
    Some(DisplayUrl {
        display: display.strip_prefix("//").unwrap_or(&display).to_owned(),
        host,
        file: false,
    })
}

#[cfg(not(target_os = "macos"))]
fn display_url(input: &str, is_image: bool, config: &Configuration) -> Option<DisplayUrl> {
    // Portable fixtures use the Rust URL parser. Foundation's case/escaping and
    // URL equivalence rules are a distinct macOS contract, not claimed here.
    let mut url = ::url::Url::parse(input).ok()?;
    if is_image || matches!(url.scheme(), "webdoc" | "data") {
        return None;
    }
    if url.scheme() == "file" {
        return Some(DisplayUrl {
            display: url.to_string(),
            host: None,
            file: true,
        });
    }
    let host = url
        .host_str()
        .map(|host| host.strip_prefix("www.").unwrap_or(host).to_owned());
    if let Some(host) = &host {
        url.set_host(Some(host)).ok()?;
    }
    if !config.include_query {
        url.set_query(None);
    }
    if !config.include_fragment {
        url.set_fragment(None);
    }
    let absolute = url.to_string();
    let display = if matches!(url.scheme(), "http" | "https") {
        absolute.split_once("://")?.1.to_owned()
    } else {
        absolute
    };
    Some(DisplayUrl {
        display,
        host,
        file: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_url_display_follows_source_scheme_host_and_suppression_branches() {
        let mut shortener = Shortener::default();
        for (url, expected) in [
            (
                "https://www.example.com/path?q=1#part",
                Some("example.com/path?q=1#part"),
            ),
            ("http://example.com/path", Some("example.com/path")),
            ("ftp://www.example.com/path", Some("ftp://example.com/path")),
            (
                "file:///tmp/owned%20fixture",
                Some("file:///tmp/owned%20fixture"),
            ),
            ("data:text/plain,owned", None),
            ("webdoc://owned/path", None),
            ("relative/path", None),
        ] {
            assert_eq!(shortener.trim(url, false).as_deref(), expected, "{url}");
        }
        assert_eq!(shortener.trim("https://example.com/image", true), None);
    }
    #[test]
    fn url_display_limits_query_flags_and_file_exception_have_independent_state() {
        let mut shortener = Shortener::new(Configuration {
            individual_limit: Some(12),
            total_limit: Some(15),
            include_query: false,
            include_fragment: false,
        })
        .unwrap();
        assert_eq!(
            shortener
                .trim("https://example.com/long?q=1#part", false)
                .as_deref(),
            Some("example.com…")
        );
        assert_eq!(shortener.total(), 12);
        assert_eq!(
            shortener
                .trim("https://second.example/path", false)
                .as_deref(),
            Some("…")
        );
        assert_eq!(shortener.total(), 13);
        assert_eq!(
            shortener
                .trim("file:///tmp/long-owned-file", false)
                .as_deref(),
            Some("file:///tmp/long-owned-file")
        );
        assert_eq!(shortener.total(), 13);
        shortener.reset();
        assert_eq!(shortener.total(), 0);
        assert!(
            Shortener::new(Configuration {
                individual_limit: Some(0),
                ..Default::default()
            })
            .is_err()
        );
        let mut flags = Shortener::new(Configuration {
            include_query: false,
            include_fragment: false,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            flags
                .trim("https://www.example.com/p?q=1#part", false)
                .as_deref(),
            Some("example.com/p")
        );
    }
    #[test]
    fn compaction_tightens_flags_then_limits_without_resetting_render_counter() {
        let mut shortener = Shortener::new(Configuration {
            total_limit: Some(0),
            ..Default::default()
        })
        .unwrap();
        shortener.trim("https://www.example.com/path?q=1#fragment", false);
        assert_eq!(shortener.compact_if_needed(), 1);
        assert_eq!(
            shortener.configuration(),
            &Configuration {
                individual_limit: Some(20),
                total_limit: Some(0),
                include_query: false,
                include_fragment: false
            }
        );
        assert_eq!(shortener.total(), 1);
        let mut default = Shortener::default();
        default.trim("https://example.com/path", false);
        assert_eq!(default.compact_if_needed(), 16);
        assert_eq!(default.configuration().total_limit, Some(4000));
        assert!(default.configuration().include_query);
    }
    #[test]
    fn retained_url_bytes_are_bounded_independently_of_suppressed_output() {
        let mut shortener = Shortener::default();
        for n in 0..10 {
            shortener.trim(&format!("data:{n},{}", "x".repeat(1024 * 1024 - 8)), true);
        }
        assert!(shortener.budget_exhausted());
        assert!(shortener.retained_bytes <= 8 * 1024 * 1024);
        assert_eq!(shortener.total(), 0);
        shortener.reset();
        assert!(!shortener.budget_exhausted());
        assert_eq!(shortener.retained_bytes, 0);
    }
}
