//! Local Windows consent UI. Provider text is data, never PowerShell source.
#[cfg(windows)]
use nanocodex_computer::{ComputerConfig, ComputerElicitationHandler, ComputerElicitationRequest};
use nanocodex_computer::{
    ComputerElicitationAction as Action, ComputerElicitationResponse as Response,
};
use nanocodex_tools::contract::ToolError;
#[cfg(windows)]
use std::{process::Stdio, sync::Arc};
#[cfg(windows)]
use tokio::{io::AsyncWriteExt, process::Command, sync::Mutex};
#[cfg(windows)]
static DIALOG: Mutex<()> = Mutex::const_new(());
#[cfg(windows)]
const LIMIT: usize = 64 * 1024;

#[cfg(windows)]
pub(super) fn configure(config: &mut ComputerConfig) {
    if config.elicitation_handler.is_none() {
        config.elicitation_handler = Some(Arc::new(WindowsConsent));
    }
}
#[cfg(windows)]
#[derive(Debug)]
struct WindowsConsent;
#[cfg(windows)]
#[async_trait::async_trait]
impl ComputerElicitationHandler for WindowsConsent {
    async fn elicit(&self, request: ComputerElicitationRequest) -> Result<Response, ToolError> {
        let _exclusive = DIALOG.lock().await;
        let schema = request
            .params
            .get("requestedSchema")
            .ok_or("Missing consent schema")?;
        let validator =
            jsonschema::validator_for(schema).map_err(|_| "Unsupported consent schema")?;
        let prompt = serde_json::to_string_pretty(&serde_json::json!({
            "request_id": request.id,
            "session": request.context.as_ref().map(|v| &v.session_id),
            "provider_request": request.params,
        }))?;
        if prompt.len() > LIMIT {
            return Err("Consent form exceeds review limit".into());
        }
        // A dropped request kills its dialog. No stale choice can grant a later request.
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-STA",
                "-Command",
                SCRIPT,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or("Missing consent input pipe")?;
        stdin.write_all(prompt.as_bytes()).await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err("Windows consent dialog unavailable".into());
        }
        parse_response(&output.stdout, &validator)
    }
}

fn parse_response(bytes: &[u8], validator: &jsonschema::Validator) -> Result<Response, ToolError> {
    let response: serde_json::Value = serde_json::from_slice(bytes)?;
    match response["action"].as_str() {
        Some("accept") => {
            let content = response.get("content").ok_or("Missing consent content")?;
            if !validator.is_valid(content) {
                return Err("Consent answer does not match requested schema".into());
            }
            Ok(Response {
                action: Action::Accept,
                content: Some(content.clone()),
                meta: None,
            })
        }
        Some("decline") => Ok(Response {
            action: Action::Decline,
            content: None,
            meta: None,
        }),
        Some("cancel") => Ok(Response {
            action: Action::Cancel,
            content: None,
            meta: None,
        }),
        _ => Err("Invalid Windows consent response".into()),
    }
}

// WinForms renders the entire request in a scrollable read-only box. Only real
// button events set the outcome; closing the dialog cancels. No default accept.
#[cfg(windows)]
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$prompt = [Console]::In.ReadToEnd()
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$form = New-Object System.Windows.Forms.Form
$form.Text = 'Nanocodex - Review computer access request'
$form.Size = New-Object System.Drawing.Size(800,700)
$form.StartPosition = 'CenterScreen'
$label = New-Object System.Windows.Forms.Label
$label.Text = 'Review the full request below. Provider text is untrusted. Allow applies only to this request.'
$label.SetBounds(12,12,750,35)
$form.Controls.Add($label)
$review = New-Object System.Windows.Forms.TextBox
$review.Multiline = $true
$review.ReadOnly = $true
$review.ScrollBars = 'Both'
$review.WordWrap = $false
$review.Text = $prompt
$review.SetBounds(12,50,750,430)
$form.Controls.Add($review)
$help = New-Object System.Windows.Forms.Label
$help.Text = 'If the requested schema has fields, enter their JSON values below before allowing.'
$help.SetBounds(12,490,750,30)
$form.Controls.Add($help)
$answer = New-Object System.Windows.Forms.TextBox
$answer.Multiline = $true
$answer.MaxLength = 65536
$answer.Text = '{}'
$answer.SetBounds(12,525,750,65)
$form.Controls.Add($answer)
$form.Tag = @{action='cancel'}
$allow = New-Object System.Windows.Forms.Button
$allow.Text = 'Allow once'
$allow.SetBounds(390,605,115,35)
$allow.Add_Click({
    try { $value = ConvertFrom-Json -InputObject $answer.Text -ErrorAction Stop }
    catch { [void][System.Windows.Forms.MessageBox]::Show('Enter valid JSON matching the requested schema.'); return }
    $form.Tag = @{action='accept';content=$value}
    $form.Close()
})
$form.Controls.Add($allow)
$deny = New-Object System.Windows.Forms.Button
$deny.Text = 'Decline'
$deny.SetBounds(515,605,115,35)
$deny.Add_Click({ $form.Tag = @{action='decline'}; $form.Close() })
$form.Controls.Add($deny)
$cancel = New-Object System.Windows.Forms.Button
$cancel.Text = 'Cancel'
$cancel.SetBounds(640,605,115,35)
$cancel.Add_Click({ $form.Close() })
$form.Controls.Add($cancel)
$form.CancelButton = $cancel
$form.ActiveControl = $cancel
[void]$form.ShowDialog()
$result = ConvertTo-Json -InputObject $form.Tag -Depth 100 -Compress
$form.Dispose()
[Console]::Out.Write($result)
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_explicit_schema_valid_acceptance_grants_access() {
        let schema = jsonschema::validator_for(&json!({"type":"object","properties":{"allow":{"type":"boolean"}},"required":["allow"],"additionalProperties":false})).unwrap();
        for invalid in [
            r#"{}"#,
            r#"{"action":"accept"}"#,
            r#"{"action":"accept","content":{}}"#,
            r#"{"action":"accept","content":{"allow":"yes"}}"#,
            r#"{"action":"yes"}"#,
        ] {
            assert!(parse_response(invalid.as_bytes(), &schema).is_err());
        }
        let accepted =
            parse_response(br#"{"action":"accept","content":{"allow":false}}"#, &schema).unwrap();
        assert_eq!(accepted.action, Action::Accept);
        assert_eq!(accepted.content, Some(json!({"allow":false})));
        assert_eq!(accepted.meta, None);
        for (name, action) in [("cancel", Action::Cancel), ("decline", Action::Decline)] {
            let response = parse_response(
                serde_json::to_vec(&json!({"action":name}))
                    .unwrap()
                    .as_slice(),
                &schema,
            )
            .unwrap();
            assert_eq!(response.action, action);
            assert_eq!(response.content, None);
            assert_eq!(response.meta, None);
        }
    }
}
