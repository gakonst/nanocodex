//! Host-authorized web tools. The host must enforce URL, DNS, redirect, account,
//! response-size and network policies, including on a proxy. Browser control
//! uses XaiHostTools with the actual browser's caller-supplied catalog.
use crate::host::*;
use serde_json::json;
use std::sync::Arc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebCapability {
    Fetch,
    Search,
}
pub trait ApprovedWebProvider: Send + Sync + 'static {
    fn capabilities(&self) -> Vec<WebCapability>;
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>>;
}
pub struct XaiWeb<P: ApprovedWebProvider + ?Sized> {
    provider: Arc<P>,
}
impl<P: ApprovedWebProvider + ?Sized> XaiWeb<P> {
    pub const fn new(provider: Arc<P>) -> Self {
        Self { provider }
    }
}
impl<P: ApprovedWebProvider + ?Sized> XaiHost for XaiWeb<P> {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.provider.capabilities().into_iter().map(|cap|match cap{
        WebCapability::Fetch=>definition("web_fetch","Fetch an approved public URL through the host's web provider.",json!({"url":{"type":"string"}}),&["url"]),
        WebCapability::Search=>definition("web_search","Search the web through the explicitly installed host provider.",json!({"query":{"type":"string"},"allowed_domains":{"type":"array","items":{"type":"string"}}}),&["query"])
    }).collect()
    }
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        let provider = self.provider.clone();
        Box::pin(async move {
            validate_request(&request)?;
            let cap = match request.tool.as_str() {
                "web_fetch" => {
                    fields(&request.input, &["url"])?;
                    let url = string(&request.input, "url")?;
                    if url.len() > 8192
                        || !(url.starts_with("https://") || url.starts_with("http://"))
                    {
                        return Err("web_fetch requires HTTP(S) URL".into());
                    }
                    WebCapability::Fetch
                }
                "web_search" => {
                    fields(&request.input, &["query", "allowed_domains"])?;
                    let query = string(&request.input, "query")?;
                    if query.trim().is_empty() || query.len() > 8192 {
                        return Err("invalid search query".into());
                    }
                    if let Some(domains) = request.input.get("allowed_domains")
                        && !domains.is_null()
                    {
                        let domains = domains
                            .as_array()
                            .ok_or("allowed_domains must be an array")?;
                        if domains.len() > 100
                            || domains
                                .iter()
                                .any(|d| d.as_str().is_none_or(|s| s.is_empty() || s.len() > 253))
                        {
                            return Err("invalid allowed_domains".into());
                        }
                    }
                    WebCapability::Search
                }
                _ => return Err("web capability not installed".into()),
            };
            if !provider.capabilities().contains(&cap) {
                return Err("web capability is no longer authorized".into());
            }
            provider.call(request).await
        })
    }
}
