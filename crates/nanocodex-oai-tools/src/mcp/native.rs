//! Transport access for native harnesses. No Responses results or Code Mode calls.
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};

use super::{
    McpHandle,
    catalog::{ConnectedCatalog, ToolEntry},
};

fn definition(entry: &ToolEntry) -> Value {
    json!({"name": entry.canonical_name,
        "description": entry.definition.description(),
        "input_schema": entry.definition.parameters().map(|schema| schema.as_value())})
}

impl McpHandle {
    /// Current native catalog. Hidden and CodeModeOnly policies are preserved.
    /// The owning provider must have been started and retained by the embedding.
    #[must_use]
    pub fn native_definitions(&self) -> Vec<Value> {
        self.state
            .native_entries()
            .iter()
            .map(|entry| definition(entry))
            .collect()
    }

    /// Discover tools using the existing ranked catalog, or exact `select:name,...`.
    /// Schemas use native name/description/input_schema fields.
    pub async fn native_search(&self, query: &str, max_results: usize) -> Result<Value, String> {
        if max_results == 0 {
            return Err("max_results must be at least 1".into());
        }
        self.state.wait_for_startup().await;
        // Search is an explicit discovery boundary: observe remote additions and
        // removals over the existing connections before returning references.
        for configured in self
            .servers
            .iter()
            .filter(|server| server.config.tool_exposure.is_deferred())
        {
            let Some(client) = self.state.native_clients().remove(&configured.name) else {
                continue;
            };
            let Some(generation) = self
                .state
                .native_refresh_generation(&configured.name, &client)
            else {
                continue;
            };
            let result =
                tokio::time::timeout(configured.config.tool_timeout, client.native_tools())
                    .await
                    .map_err(|_| "MCP tool discovery timed out".to_owned())
                    .and_then(|result| result);
            let result = result.map(|tools| ConnectedCatalog {
                entries: ToolEntry::new_many(
                    &configured.name,
                    &configured.model_namespace,
                    tools
                        .into_iter()
                        .filter(|tool| configured.config.includes_tool(tool.name.as_ref()))
                        .collect(),
                    client.clone(),
                    &configured.config,
                ),
                client,
            });
            let error = result.as_ref().err().cloned();
            self.state
                .complete_server(&configured.name, generation, result);
            if let Some(error) = error {
                return Err(format!(
                    "MCP discovery failed for {}: {error}",
                    configured.name
                ));
            }
        }
        let definitions = self.native_definitions();
        let selected = if let Some(names) = query.trim().strip_prefix("select:") {
            let names = names.split(',').map(str::trim).collect::<Vec<_>>();
            if names.is_empty() || names.iter().any(|name| name.is_empty()) {
                return Err("select: requires at least one tool name".into());
            }
            for name in &names {
                if !definitions
                    .iter()
                    .any(|tool| tool["name"].as_str() == Some(name))
                {
                    return Err(format!("MCP tool is unavailable: {name}"));
                }
            }
            definitions
                .into_iter()
                .filter(|tool| names.contains(&tool["name"].as_str().unwrap_or_default()))
                .take(max_results)
                .collect::<Vec<_>>()
        } else {
            let response = serde_json::to_value(self.state.search(query, Some(max_results)).await?)
                .map_err(|e| e.to_string())?;
            let names = response["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>();
            names
                .into_iter()
                .filter_map(|name| {
                    definitions
                        .iter()
                        .find(|tool| tool["name"].as_str() == Some(name))
                        .cloned()
                })
                .collect()
        };
        Ok(json!({"tools": selected, "servers": self.state.native_status()}))
    }

    /// Wait for actual discovery completion, bounded by the caller's timeout.
    /// Pending and failed servers are reported independently; timeout is not readiness.
    pub async fn native_wait(&self, timeout: Duration) -> Value {
        let _ = tokio::time::timeout(timeout, self.state.wait_for_startup()).await;
        self.state.native_status()
    }

    /// Call a native-visible tool once, returning the original MCP protocol result.
    /// Authentication, payment policy, connection ownership and timeout remain transport-owned.
    pub async fn native_call(
        &self,
        name: &str,
        input: Value,
        metadata: Value,
    ) -> Result<Value, String> {
        self.native_call_checked(name, input, metadata, None).await
    }

    /// Execute only if the current name, description and input schema still
    /// match the definition admitted by the native harness.
    pub async fn native_call_admitted(
        &self,
        name: &str,
        input: Value,
        metadata: Value,
        admitted_definition: &Value,
    ) -> Result<Value, String> {
        self.native_call_checked(name, input, metadata, Some(admitted_definition))
            .await
    }

    async fn native_call_checked(
        &self,
        name: &str,
        input: Value,
        metadata: Value,
        admitted_definition: Option<&Value>,
    ) -> Result<Value, String> {
        let entry = self
            .state
            .ready_entry(name)
            .await
            .filter(|entry| entry.tool_exposure.is_deferred())
            .ok_or_else(|| format!("MCP tool is unavailable: {name}"))?;
        if admitted_definition.is_some_and(|admitted| admitted != &definition(&entry)) {
            return Err(format!(
                "MCP tool {name} changed since admission; rediscover before executing"
            ));
        }
        let arguments = input
            .as_object()
            .ok_or("MCP tool input must be an object")?
            .clone();
        let mut params =
            CallToolRequestParams::new(entry.remote_name.clone()).with_arguments(arguments);
        if !metadata.is_null() {
            params
                .meta
                .get_or_insert_with(rmcp::model::RequestMetaObject::new)
                .0
                .0
                .insert("nanocodex/invocation".into(), metadata);
        }
        entry.client.refresh_oauth().await?;
        // OAuth can yield while a host removes/reconnects the server. Do not
        // dispatch through a superseded client after that authorization wait.
        let current = self
            .state
            .ready_entry(name)
            .await
            .filter(|current| current.tool_exposure.is_deferred())
            .ok_or_else(|| format!("MCP tool is unavailable: {name}"))?;
        if !std::sync::Arc::ptr_eq(&current.client, &entry.client)
            || definition(&current) != definition(&entry)
        {
            return Err(format!(
                "MCP tool {name} changed since admission; rediscover before executing"
            ));
        }
        let result = tokio::time::timeout(entry.timeout, entry.client.call_tool(params))
            .await
            .map_err(|_| {
                format!(
                    "MCP tool {name} timed out; outcome may be unknown, do not retry automatically"
                )
            })??;
        serde_json::to_value(result).map_err(|e| e.to_string())
    }

    /// List resources and resource templates, following bounded protocol pagination.
    /// Omitting a server lists every configured, resource-capable native server.
    pub async fn native_list_resources(&self, server: Option<&str>) -> Result<Value, String> {
        self.state.wait_for_startup().await;
        if let Some(name) = server {
            self.native_server(name)?;
        }
        let clients = self.state.native_clients();
        let mut resources = Vec::new();
        let mut templates = Vec::new();
        let mut failures = serde_json::Map::new();
        for configured in self
            .servers
            .iter()
            .filter(|s| s.config.tool_exposure.is_deferred())
        {
            if server.is_some_and(|name| name != configured.name) {
                continue;
            }
            let Some(client) = clients.get(&configured.name) else {
                failures.insert(configured.name.clone(), json!("server is not ready"));
                continue;
            };
            if !client.supports_resources() {
                continue;
            }
            let result =
                tokio::time::timeout(configured.config.tool_timeout, client.list_resources()).await;
            match result {
                Ok(Ok(value)) => {
                    for (key, target) in [
                        ("resources", &mut resources),
                        ("resourceTemplates", &mut templates),
                    ] {
                        for item in value[key].as_array().into_iter().flatten() {
                            let mut item = item.clone();
                            item["server"] = json!(configured.name);
                            target.push(item);
                        }
                    }
                }
                Ok(Err(error)) => {
                    failures.insert(configured.name.clone(), json!(error));
                }
                Err(_) => {
                    failures.insert(
                        configured.name.clone(),
                        json!("resource discovery timed out"),
                    );
                }
            }
        }
        Ok(
            json!({"resources": resources, "resourceTemplates": templates, "failed_servers": failures,
            "servers": self.state.native_status()}),
        )
    }

    /// Read a URI through its configured MCP server. Never fetches the URI locally.
    pub async fn native_read_resource(&self, server: &str, uri: &str) -> Result<Value, String> {
        let configured = self.native_server(server)?;
        if uri.is_empty() || uri.len() > 64 * 1024 || uri.contains('\0') {
            return Err("uri must be nonempty, NUL-free and at most 64 KiB".into());
        }
        self.state.wait_for_startup().await;
        let client = self
            .state
            .native_clients()
            .remove(server)
            .ok_or_else(|| format!("MCP server is not ready: {server}"))?;
        if !client.supports_resources() {
            return Err(format!("MCP server does not support resources: {server}"));
        }
        tokio::time::timeout(configured.config.tool_timeout, client.read_resource(uri))
            .await
            .map_err(|_| "MCP resource read timed out".to_owned())?
    }

    fn native_server(&self, name: &str) -> Result<&super::NamedServer, String> {
        self.servers
            .iter()
            .find(|server| server.name == name && server.config.tool_exposure.is_deferred())
            .ok_or_else(|| format!("MCP server is unavailable: {name}"))
    }
}
