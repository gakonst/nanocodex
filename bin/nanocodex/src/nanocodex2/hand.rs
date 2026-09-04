use nanocodex_browser::{Browser, BrowserAction, BrowserTool};
use nanocodex_managed::ManagedError;
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolResult, Tools, attachment::AttachmentMachine,
    runtime::schema_for,
};

use super::{Hand, vm_hand};

const ATTACHED_BROWSER_DESCRIPTION: &str = "Control the isolated Chromium browser running on this attached machine. Page, frame, and worker requests leave through this machine's own network connection. Open a URL before inspecting it, prefer fresh snapshot references for interaction, and use text or evaluation reads for IP-address checks. The session is private and does not use the person's normal browser profile.";

pub(crate) struct HandRuntime {
    vm: Option<vm_hand::VmHand>,
    browser: Option<Browser>,
    tools: Tools,
    machine: AttachmentMachine,
}

impl HandRuntime {
    pub(crate) async fn start(config: &Hand) -> Result<Self, ManagedError> {
        let has_vm = config.rootfs.is_some();
        let machine = AttachmentMachine::new(
            &config.machine_id,
            &config.machine_name,
            if has_vm { &config.vm_workspace } else { "/" },
            capabilities(config),
        )
        .map_err(|error| configuration(error.to_string()))?;

        let vm = if has_vm {
            Some(vm_hand::VmHand::start(config).await?)
        } else {
            None
        };
        let mut tools = vm.as_ref().map_or_else(
            || Tools::builder().without_defaults(),
            vm_hand::VmHand::tools_builder,
        );

        let browser = if config.browser {
            let mut builder = Browser::builder();
            if let Some(executable) = &config.browser_executable {
                builder = builder.executable(executable);
            }
            match builder.build() {
                Ok(browser) => {
                    tools = tools.tool(AttachedBrowserTool::new(browser.clone()));
                    Some(browser)
                }
                Err(error) => {
                    let message = format!("failed to configure attached browser: {error}");
                    return Err(with_vm_shutdown(message, vm).await);
                }
            }
        } else {
            None
        };

        let tools = match tools.build() {
            Ok(tools) => tools,
            Err(error) => {
                let message = format!("failed to prepare hand tools: {error}");
                return Err(shutdown_after_start_failure(message, browser, vm).await);
            }
        };
        Ok(Self {
            vm,
            browser,
            tools,
            machine,
        })
    }

    pub(crate) const fn machine(&self) -> &AttachmentMachine {
        &self.machine
    }

    pub(crate) fn tools(&self) -> Tools {
        self.tools.clone()
    }

    pub(crate) async fn shutdown(self) -> Result<(), ManagedError> {
        let Self {
            vm,
            browser,
            tools,
            machine: _,
        } = self;
        drop(tools);
        shutdown_components(browser, vm).await
    }
}

fn capabilities(config: &Hand) -> Vec<String> {
    let mut capabilities = Vec::new();
    if config.rootfs.is_some() {
        capabilities.extend([
            "filesystem".to_owned(),
            "linux".to_owned(),
            "process".to_owned(),
            "pty".to_owned(),
            "shell".to_owned(),
            "vm".to_owned(),
            format!("cpu:{}", config.vm_cpus),
            format!("memory-mib:{}", config.vm_memory_mib),
        ]);
        if !config.vm_no_network {
            capabilities.push("network".to_owned());
        }
    }
    if config.browser {
        capabilities.extend(["browser".to_owned(), "browser-egress".to_owned()]);
    }
    capabilities.sort_unstable();
    capabilities
}

async fn shutdown_after_start_failure(
    message: String,
    browser: Option<Browser>,
    vm: Option<vm_hand::VmHand>,
) -> ManagedError {
    match shutdown_components(browser, vm).await {
        Ok(()) => configuration(message),
        Err(shutdown) => configuration(format!("{message}; hand shutdown also failed: {shutdown}")),
    }
}

async fn with_vm_shutdown(message: String, vm: Option<vm_hand::VmHand>) -> ManagedError {
    match vm {
        Some(vm) => match vm.shutdown().await {
            Ok(()) => configuration(message),
            Err(shutdown) => {
                configuration(format!("{message}; VM shutdown also failed: {shutdown}"))
            }
        },
        None => configuration(message),
    }
}

async fn shutdown_components(
    browser: Option<Browser>,
    vm: Option<vm_hand::VmHand>,
) -> Result<(), ManagedError> {
    let browser = match browser {
        Some(browser) => browser
            .close()
            .await
            .map_err(|error| configuration(format!("failed to close attached browser: {error}"))),
        None => Ok(()),
    };
    let vm = match vm {
        Some(vm) => vm.shutdown().await,
        None => Ok(()),
    };
    match (browser, vm) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(browser), Err(vm)) => Err(configuration(format!("{browser}; {vm}"))),
    }
}

struct AttachedBrowserTool {
    browser: BrowserTool,
}

impl AttachedBrowserTool {
    fn new(browser: Browser) -> Self {
        Self {
            browser: BrowserTool::from_browser(browser),
        }
    }

    #[cfg(test)]
    const fn from_tool(browser: BrowserTool) -> Self {
        Self { browser }
    }
}

#[nanocodex_tools::contract::async_trait]
impl Tool for AttachedBrowserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "browser",
            ATTACHED_BROWSER_DESCRIPTION,
            schema_for::<BrowserAction>(),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        false
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.browser.execute(input, context).await
    }
}

fn configuration(message: impl Into<String>) -> ManagedError {
    ManagedError::Configuration(message.into())
}

#[cfg(test)]
mod tests {
    use nanocodex_browser::BrowserAction;
    use nanocodex_tools::{Tool as _, ToolContext, ToolInput};
    use serde_json::value::to_raw_value;

    use super::{AttachedBrowserTool, BrowserTool};

    #[test]
    fn attached_browser_contract_fits_the_hosted_catalog_bounds() {
        let (browser, _) = BrowserTool::recording();
        let definition = AttachedBrowserTool::from_tool(browser).definition();
        let nanocodex_tools::ToolDefinition::Function {
            description,
            parameters,
            output_schema,
            ..
        } = definition
        else {
            panic!("attached browser must use a function definition");
        };
        assert!(description.len() <= 8 * 1024);
        let parameter_bytes = serde_json::to_vec(parameters.as_value()).unwrap().len();
        assert!(
            parameter_bytes <= 128 * 1024,
            "browser parameters require {parameter_bytes} bytes"
        );
        assert!(output_schema.is_none());
    }

    #[tokio::test]
    async fn attached_browser_forwards_the_full_browser_action() {
        let (browser, recording) = BrowserTool::recording();
        let browser = AttachedBrowserTool::from_tool(browser);
        let output = browser
            .execute(
                ToolInput::Function(
                    to_raw_value(&serde_json::json!({
                        "action": "open",
                        "url": "data:text/plain,residential-egress-proof"
                    }))
                    .unwrap(),
                ),
                ToolContext::new("model", "session", "call", &[], 1_024),
            )
            .await
            .unwrap();
        assert!(output.success);
        assert_eq!(
            recording.actions().unwrap()[0].action,
            BrowserAction::Open {
                url: "data:text/plain,residential-egress-proof".to_owned()
            }
        );
    }
}
