// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::Presentation;
use crate::tui::{theme::Theme, transcript::ToolEntry};
use ratatui::{style::Style, text::Line};
use serde_json::{Map, Value};

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    match tool.family() {
        "account_connectors" => connectors_present(tool, width, theme, expanded),
        "runtimeInfo" => runtime_present(tool, width, theme, expanded),
        _ => account_info(tool, width, theme, expanded),
    }
}

fn account_info(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let result = tool.result.as_ref();
    let status = result
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
        .map(humanize)
        .unwrap_or_else(|| "account snapshot".to_owned());
    let mut counts = Vec::new();
    if let Some(result) = result {
        if let Some(summary) = connector_summary(result) {
            counts.push(summary);
        }
        for (key, singular, plural) in [
            ("machines", "machine", "machines"),
            ("vault", "Vault item", "Vault items"),
        ] {
            if let Some(values) = result.get(key).and_then(Value::as_array) {
                counts.push(super::count_label(values.len(), singular, plural));
            }
        }
        for (key, singular, plural) in [
            ("stablecoins", "balance", "balances"),
            ("authorizations", "authorization", "authorizations"),
        ] {
            if let Some(values) = result.get(key).and_then(Value::as_array)
                && !values.is_empty()
            {
                counts.push(super::count_label(values.len(), singular, plural));
            }
        }
    }
    let mut presentation = Presentation::new("Account info", status);
    if !counts.is_empty() {
        presentation = presentation.outcome(counts.join(" · "));
    }
    if !expanded {
        return presentation;
    }

    let Some(result) = result.and_then(Value::as_object) else {
        return super::with_generic_details(presentation, tool, width, theme);
    };
    let details = account_details(result, width, theme);
    presentation
        .unselectable_details(details)
        .footer("live account snapshot")
}

fn connectors_present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let operation = tool
        .arguments
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("manage");
    let connector = tool
        .arguments
        .get("connector")
        .and_then(Value::as_str)
        .map(humanize);
    let subject = connector.map_or_else(
        || humanize(operation),
        |name| format!("{} {name}", humanize(operation)),
    );
    let mut presentation = Presentation::new("Account connectors", subject);
    if let Some(status) = tool
        .result
        .as_ref()
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
    {
        presentation = presentation.outcome(humanize(status));
    } else if let Some(connectors) = tool
        .result
        .as_ref()
        .and_then(|result| result.get("connectors"))
        .and_then(Value::as_object)
    {
        let connected = connectors
            .values()
            .filter(|connector| connector.get("connected").and_then(Value::as_bool) == Some(true))
            .count();
        presentation = presentation.outcome(super::count_label(
            connected,
            "connected provider",
            "connected providers",
        ));
    }
    if !expanded {
        return presentation;
    }
    let Some(result) = tool.result.as_ref().and_then(Value::as_object) else {
        return super::with_generic_details(presentation, tool, width, theme);
    };
    let mut details = Vec::new();
    if let Some(connectors) = result.get("connectors").and_then(Value::as_object) {
        for (name, connector) in connectors {
            let connected = connector.get("connected").and_then(Value::as_bool) == Some(true);
            let mut summary = if connected {
                "connected"
            } else {
                "not connected"
            }
            .to_owned();
            if let Some(connections) = connector.get("connections").and_then(Value::as_array) {
                let labels = connections
                    .iter()
                    .filter_map(|connection| connection.get("label").and_then(Value::as_str))
                    .collect::<Vec<_>>();
                if !labels.is_empty() {
                    summary.push_str(" · ");
                    summary.push_str(&labels.join(", "));
                }
            }
            row(&mut details, &humanize(name), &summary, width, theme);
        }
    }
    if let Some(message) = string(result.get("message")) {
        row(&mut details, "Message", message, width, theme);
    }
    if let Some(url) = string(result.get("authorization_url")) {
        row(&mut details, "Authorize", url, width, theme);
    }
    if details.is_empty() {
        return super::with_generic_details(presentation, tool, width, theme);
    }
    presentation
        .unselectable_details(details)
        .footer("account connector status")
}

fn runtime_present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let result = tool.result.as_ref();
    let runtime = result
        .and_then(|result| result.get("runtime"))
        .and_then(Value::as_str)
        .map(humanize)
        .unwrap_or_else(|| "runtime snapshot".to_owned());
    let mut presentation = Presentation::new("Runtime info", runtime);
    if let Some(workspace) = result
        .and_then(|result| result.get("workspace"))
        .and_then(Value::as_str)
    {
        presentation = presentation.outcome(workspace);
    }
    if !expanded {
        return presentation;
    }
    let Some(result) = result.and_then(Value::as_object) else {
        return super::with_generic_details(presentation, tool, width, theme);
    };
    let mut details = Vec::new();
    for (key, label) in [
        ("runtime", "Runtime"),
        ("shell", "Shell"),
        ("shell_network", "Network"),
        ("sandbox", "Sandbox"),
        ("workspace", "Workspace"),
    ] {
        if let Some(value) = string(result.get(key)) {
            let value = if key == "workspace" {
                value.to_owned()
            } else {
                humanize(value)
            };
            row(&mut details, label, &value, width, theme);
        }
    }
    for (key, label) in [
        ("commands", "Commands"),
        ("custom_commands", "Custom commands"),
    ] {
        if let Some(values) = result.get(key).and_then(Value::as_array) {
            row(&mut details, label, &values.len().to_string(), width, theme);
        }
    }
    if let Some(account) = result.get("account").and_then(Value::as_object) {
        details.extend(account_details(account, width, theme));
    }
    if details.is_empty() {
        return super::with_generic_details(presentation, tool, width, theme);
    }
    presentation
        .unselectable_details(details)
        .footer("runtime snapshot")
}

fn account_details(result: &Map<String, Value>, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(status) = string(result.get("status")) {
        row(&mut lines, "Status", &humanize(status), width, theme);
    }
    connectors(&mut lines, result, width, theme);
    machines(&mut lines, result.get("machines"), width, theme);
    vault(&mut lines, result.get("vault"), width, theme);
    identity(&mut lines, result.get("identity"), width, theme);
    balances(&mut lines, result.get("stablecoins"), width, theme);
    authorizations(&mut lines, result.get("authorizations"), width, theme);
    if lines.is_empty() {
        row(
            &mut lines,
            "Account",
            "No recognized account fields returned",
            width,
            theme,
        );
    }
    lines
}

fn connectors(
    lines: &mut Vec<Line<'static>>,
    result: &Map<String, Value>,
    width: u16,
    theme: &Theme,
) {
    let accounts = result.get("accounts").and_then(Value::as_object);
    let connector_accounts = result.get("connectorAccounts").and_then(Value::as_object);
    if let Some(connectors) = connector_accounts {
        for (service, connections) in connectors {
            let labels = connections
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|connection| {
                    let fields = connection.as_object()?;
                    string(fields.get("label"))
                        .or_else(|| string(fields.get("accountId")))
                        .or_else(|| string(fields.get("id")))
                })
                .collect::<Vec<_>>();
            let value = if labels.is_empty() {
                accounts
                    .and_then(|accounts| string(accounts.get(service)))
                    .unwrap_or("connected")
                    .to_owned()
            } else {
                labels.join(", ")
            };
            row(lines, &humanize(service), &value, width, theme);
        }
    }
    if let Some(accounts) = accounts {
        for (service, label) in accounts {
            if connector_accounts.is_some_and(|connectors| connectors.contains_key(service)) {
                continue;
            }
            if let Some(label) = string(Some(label)) {
                row(lines, &humanize(service), label, width, theme);
            }
        }
    }
    if let Some(authenticated) = result.get("authenticated").and_then(Value::as_array) {
        let services = authenticated
            .iter()
            .filter_map(Value::as_str)
            .filter(|service| {
                !connector_accounts.is_some_and(|connectors| connectors.contains_key(*service))
                    && !accounts.is_some_and(|accounts| accounts.contains_key(*service))
            })
            .map(humanize)
            .collect::<Vec<_>>();
        if !services.is_empty() {
            row(lines, "Connectors", &services.join(", "), width, theme);
        }
    }
}

fn machines(lines: &mut Vec<Line<'static>>, value: Option<&Value>, width: u16, theme: &Theme) {
    for machine in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(fields) = machine.as_object() else {
            continue;
        };
        let name = string(fields.get("name"))
            .or_else(|| string(fields.get("id")))
            .unwrap_or("unnamed machine");
        let mut details = Vec::new();
        if let Some(kind) = string(fields.get("kind")) {
            details.push(humanize(kind));
        }
        if let Some(workspace) = string(fields.get("workspace")) {
            details.push(workspace.to_owned());
        }
        if let Some(capabilities) = string_list(fields.get("capabilities"))
            && !capabilities.is_empty()
        {
            details.push(capabilities.join(", "));
        }
        row(
            lines,
            &format!("Machine {name}"),
            &details.join(" · "),
            width,
            theme,
        );
    }
}

fn vault(lines: &mut Vec<Line<'static>>, value: Option<&Value>, width: u16, theme: &Theme) {
    for item in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(fields) = item.as_object() else {
            continue;
        };
        let name = string(fields.get("name")).unwrap_or("unnamed item");
        let kind = string(fields.get("kind")).map(humanize);
        let safe_detail = string(fields.get("username"))
            .map(str::to_owned)
            .or_else(|| string(fields.get("last4")).map(|last4| format!("•••• {last4}")))
            .or_else(|| string(fields.get("phone_number")).map(str::to_owned))
            .or_else(|| {
                let city = string(fields.get("city"))?;
                let country = string(fields.get("country"));
                Some(
                    country.map_or_else(|| city.to_owned(), |country| format!("{city}, {country}")),
                )
            });
        let detail = [kind, safe_detail]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        row(lines, &format!("Vault · {name}"), &detail, width, theme);
    }
}

fn identity(lines: &mut Vec<Line<'static>>, value: Option<&Value>, width: u16, theme: &Theme) {
    let Some(fields) = value.and_then(Value::as_object) else {
        return;
    };
    if let Some(address) = string(fields.get("tempoAddress")) {
        row(lines, "Tempo address", address, width, theme);
    }
    if let Some(host) = fields.get("hostPrincipal").and_then(Value::as_object)
        && let Some(id) = string(host.get("id"))
    {
        row(lines, "Host principal", id, width, theme);
    }
}

fn balances(lines: &mut Vec<Line<'static>>, value: Option<&Value>, width: u16, theme: &Theme) {
    for balance in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(fields) = balance.as_object() else {
            continue;
        };
        let Some(amount) = string(fields.get("balance")) else {
            continue;
        };
        let symbol = string(fields.get("symbol")).unwrap_or("token");
        row(
            lines,
            "Balance",
            &format!("{amount} {symbol}"),
            width,
            theme,
        );
    }
}

fn authorizations(
    lines: &mut Vec<Line<'static>>,
    value: Option<&Value>,
    width: u16,
    theme: &Theme,
) {
    for authorization in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(fields) = authorization.as_object() else {
            continue;
        };
        let app = string(fields.get("appId")).unwrap_or("unknown app");
        let details = [
            string(fields.get("permission")).map(humanize),
            string(fields.get("status")).map(humanize),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        row(
            lines,
            &format!("Authorization · {app}"),
            &details,
            width,
            theme,
        );
    }
}

fn row(lines: &mut Vec<Line<'static>>, label: &str, value: &str, width: u16, theme: &Theme) {
    let text = if value.is_empty() {
        label.to_owned()
    } else {
        format!("{label}  {value}")
    };
    lines.extend(super::super::markdown::wrap_plain(
        &text,
        width,
        Style::default().fg(theme.text()),
    ));
}

fn connector_summary(result: &Value) -> Option<String> {
    if let Some(connectors) = result.get("connectorAccounts").and_then(Value::as_object) {
        let connections = connectors
            .values()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .sum();
        if connections > 0 {
            return Some(super::count_label(connections, "connection", "connections"));
        }
    }
    result
        .get("authenticated")
        .and_then(Value::as_array)
        .map(|connectors| super::count_label(connectors.len(), "connector", "connectors"))
}

fn string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn string_list(value: Option<&Value>) -> Option<Vec<&str>> {
    Some(
        value?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .collect(),
    )
}

fn humanize(value: &str) -> String {
    value.replace(['_', '-'], " ")
}
