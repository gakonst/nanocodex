use std::{path::PathBuf, process::Command, process::Stdio, time::Duration};

use clap::{Args, Subcommand};
use eyre::{Result, WrapErr, eyre};
use nanocodex_browser::{Browser, BrowserAction, BrowserActionResult, BrowserFrame};
use nanousd::{
    CreateOrderRequest, CreateOrderResponse, CreditsClient, NANOUSD_DECIMALS, Order, OrderStatus,
};
use serde::Deserialize;
use tempo_alloy::accounts::TempoAccountsStore;
use tokio::process::Command as AsyncCommand;

const DEFAULT_CREDITS_API_URL: &str = "https://nanocodex-api.paradigm.xyz";

#[derive(Args)]
pub(crate) struct Credits {
    #[command(subcommand)]
    command: CreditsCommand,

    /// `NanoUSD` credits API endpoint.
    #[arg(
        long,
        global = true,
        env = "NANOCODEX_CREDITS_API_URL",
        default_value = DEFAULT_CREDITS_API_URL
    )]
    api_url: String,

    /// Tempo Wallet state containing the destination account.
    #[arg(long, global = true, env = "NANOCODEX_PROVIDER_TEMPO_WALLET_STORE")]
    wallet_store: Option<PathBuf>,
}

#[derive(Subcommand)]
enum CreditsCommand {
    /// Show the current wallet's `NanoUSD` balance and service configuration.
    Status(OutputArgs),
    /// Purchase a fixed-dollar package of Nanocodex credits.
    Buy(BuyArgs),
    /// Wait for an existing order to be fulfilled.
    Wait(WaitArgs),
}

#[derive(Args)]
struct OutputArgs {
    /// Emit the response as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct BuyArgs {
    /// Package value in whole US dollars, for example `10`, `25`, or `50`.
    dollars: u64,

    /// Print the checkout URL instead of paying with Link CLI or opening a browser.
    #[arg(long)]
    no_open: bool,

    /// Return as soon as the order is created.
    #[arg(long)]
    no_wait: bool,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,

    /// Maximum time to wait for payment and issuance.
    #[arg(long, default_value_t = 600)]
    timeout_seconds: u64,
}

#[derive(Args)]
struct WaitArgs {
    order_id: String,

    /// Order capability returned by `credits buy --json --no-wait`.
    #[arg(long, env = "NANOCODEX_CREDITS_ORDER_TOKEN", hide_env_values = true)]
    order_token: String,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,

    /// Maximum time to wait for fulfillment.
    #[arg(long, default_value_t = 600)]
    timeout_seconds: u64,
}

impl Credits {
    pub(crate) async fn run(self) -> Result<()> {
        let store = self
            .wallet_store
            .map_or_else(TempoAccountsStore::open_default, TempoAccountsStore::open)?;
        let account = store.active_account()?;
        let client = CreditsClient::new(&self.api_url)?;
        match self.command {
            CreditsCommand::Status(args) => status(&client, account, args.json).await,
            CreditsCommand::Buy(args) => buy(&client, account, args).await,
            CreditsCommand::Wait(args) => {
                let order = wait_for_order(
                    &client,
                    &args.order_id,
                    &args.order_token,
                    Duration::from_secs(args.timeout_seconds),
                    !args.json,
                )
                .await?;
                print_order(&order, args.json)
            }
        }
    }
}

async fn status(
    client: &CreditsClient,
    wallet: alloy_primitives::Address,
    json: bool,
) -> Result<()> {
    let (info, balance) = tokio::try_join!(client.info(), client.balance(wallet))?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "service": info,
                "balance": balance,
            }))?
        );
    } else {
        println!("Nanocodex credits\n");
        println!("Available:  ${}", format_units(balance.nanousd_units));
        println!("Currency:   NANOUSD");
        println!("Wallet:     {}", balance.wallet);
        println!("Network:    Tempo mainnet ({})", balance.chain_id);
        println!("Token:      {}", balance.token);
        println!("Onramp:     {}", info.payment_mode);
        let packages = info
            .packages
            .iter()
            .map(|package| format!("${}", package.usd_cents / 100))
            .collect::<Vec<_>>()
            .join(", ");
        println!("Packages:   {packages}");
    }
    Ok(())
}

async fn buy(
    client: &CreditsClient,
    wallet: alloy_primitives::Address,
    args: BuyArgs,
) -> Result<()> {
    let package_cents = args
        .dollars
        .checked_mul(100)
        .ok_or_else(|| eyre!("credit package is too large"))?;
    let created = client
        .create_order(&CreateOrderRequest {
            wallet,
            package_cents,
        })
        .await?;
    if args.json && args.no_wait {
        println!("{}", serde_json::to_string(&created)?);
        return Ok(());
    }
    describe_created(&created, args.json);
    if let Some(checkout_url) = &created.order.checkout_url {
        if args.no_open {
            if !args.json {
                println!("Checkout: {checkout_url}");
            }
        } else if !args.no_wait && link_cli_available().await {
            match pay_checkout_with_link(
                checkout_url,
                created.order.package.usd_cents,
                Duration::from_secs(args.timeout_seconds),
                !args.json,
            )
            .await?
            {
                LinkCheckout::Submitted => {
                    if !args.json {
                        println!("Submitted secure Stripe Checkout with Link.");
                    }
                }
                LinkCheckout::Unsupported => open_checkout(checkout_url, args.json),
            }
        } else {
            open_checkout(checkout_url, args.json);
        }
    }
    if args.no_wait {
        if !args.json {
            println!(
                "Resume with: nanocodex credits wait {} --order-token <token>",
                created.order.id
            );
            println!("Order token: {}", created.order_token);
        }
        return Ok(());
    }
    let order = wait_for_order(
        client,
        &created.order.id,
        &created.order_token,
        Duration::from_secs(args.timeout_seconds),
        !args.json,
    )
    .await?;
    print_order(&order, args.json)
}

fn open_checkout(checkout_url: &str, json: bool) {
    if let Err(error) = open_browser(checkout_url) {
        if !json {
            eprintln!(
                "Could not open checkout automatically ({error}). Open this URL:\n{checkout_url}"
            );
        }
    } else if !json {
        println!("Opened secure Stripe Checkout in your browser.");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkCheckout {
    Unsupported,
    Submitted,
}

#[derive(Debug, Deserialize)]
struct LinkAuthStatus {
    authenticated: bool,
}

#[derive(Debug, Deserialize)]
struct LinkSpendRequest {
    id: String,
    status: String,
    link_pay_token: Option<String>,
}

struct LinkCheckoutFrame {
    frame_id: String,
    merchant_account_id: String,
}

async fn link_cli_available() -> bool {
    match AsyncCommand::new("link-cli")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

async fn pay_checkout_with_link(
    checkout_url: &str,
    amount_cents: u64,
    timeout: Duration,
    progress: bool,
) -> Result<LinkCheckout> {
    let browser = match Browser::new() {
        Ok(browser) => browser,
        Err(error) => {
            if progress {
                eprintln!("Link Checkout is unavailable ({error}); falling back to the browser.");
            }
            return Ok(LinkCheckout::Unsupported);
        }
    };
    if let Err(error) = browser
        .execute(BrowserAction::Open {
            url: checkout_url.to_owned(),
        })
        .await
    {
        if progress {
            eprintln!("Link Checkout is unavailable ({error}); falling back to the browser.");
        }
        return Ok(LinkCheckout::Unsupported);
    }
    let frame = match find_link_checkout_frame(&browser).await {
        Ok(Some(frame)) => frame,
        Ok(None) => {
            drop(browser.close().await);
            if progress {
                println!(
                    "Stripe Checkout does not advertise Link agent payments; using the browser."
                );
            }
            return Ok(LinkCheckout::Unsupported);
        }
        Err(error) => {
            drop(browser.close().await);
            if progress {
                eprintln!("Link Checkout is unavailable ({error}); falling back to the browser.");
            }
            return Ok(LinkCheckout::Unsupported);
        }
    };

    let auth: LinkAuthStatus = run_link_cli(
        &["auth", "status", "--format", "json"],
        Duration::from_secs(15),
    )
    .await
    .wrap_err("failed to read Link CLI authentication status")?;
    if !auth.authenticated {
        return Err(eyre!(
            "link-cli is installed but is not authenticated; run `link-cli auth login --client-name Nanocodex` and retry"
        ));
    }

    if progress {
        println!(
            "Requesting approval in Link for ${}…",
            format_cents(amount_cents)
        );
    }
    let amount = amount_cents.to_string();
    let line_item = format!("name:Nanocodex NANOUSD credits,unit_amount:{amount_cents},quantity:1");
    let total = format!("type:total,display_text:Total,amount:{amount_cents}");
    let context = format!(
        "Purchasing ${} of NANOUSD credits for the active Nanocodex Tempo wallet. The user initiated this purchase with `nanocodex credits buy`.",
        format_cents(amount_cents)
    );
    let created: LinkSpendRequest = run_link_cli_owned(
        vec![
            "spend-request".to_owned(),
            "create".to_owned(),
            "--execution-method".to_owned(),
            "link_pay_token".to_owned(),
            "--merchant-account-id".to_owned(),
            frame.merchant_account_id,
            "--amount".to_owned(),
            amount,
            "--context".to_owned(),
            context,
            "--line-item".to_owned(),
            line_item,
            "--total".to_owned(),
            total,
            "--request-approval".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ],
        timeout,
    )
    .await
    .wrap_err("Link did not approve the Nanocodex credits purchase")?;
    if created.status != "approved" {
        return Err(eyre!(
            "Link spend request {} finished with status {}",
            created.id,
            created.status
        ));
    }

    let spend_request_id = created.id;
    let prepare_and_submit = async {
        let retrieved: LinkSpendRequest = run_link_cli_owned(
            vec![
                "spend-request".to_owned(),
                "retrieve".to_owned(),
                spend_request_id.clone(),
                "--include".to_owned(),
                "link_pay_token".to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ],
            Duration::from_secs(15),
        )
        .await
        .wrap_err("failed to retrieve the approved Link Pay Token")?;
        let token = retrieved
            .link_pay_token
            .ok_or_else(|| eyre!("approved Link spend request omitted its Link Pay Token"))?;
        inject_link_pay_token(&browser, &frame.frame_id, &token).await?;
        wait_for_link_card(&browser).await?;
        submit_link_checkout(&browser).await
    }
    .await;
    if let Err(error) = prepare_and_submit {
        let _: Result<LinkSpendRequest> = run_link_cli_owned(
            vec![
                "spend-request".to_owned(),
                "cancel".to_owned(),
                spend_request_id,
                "--format".to_owned(),
                "json".to_owned(),
            ],
            Duration::from_secs(15),
        )
        .await;
        drop(browser.close().await);
        return Err(error);
    }
    wait_for_checkout_navigation(&browser, Duration::from_secs(30)).await;
    drop(browser.close().await);
    Ok(LinkCheckout::Submitted)
}

async fn find_link_checkout_frame(browser: &Browser) -> Result<Option<LinkCheckoutFrame>> {
    const REVEAL: &str = r#"(() => {
        const root = document.querySelector('.AiAgentPaymentSteering');
        if (!root) return false;
        const checkbox = root.querySelector('input[type="checkbox"]');
        if (checkbox && !checkbox.checked) checkbox.click();
        return true;
    })()"#;
    const READ_BINDING: &str = r#"(() => {
        const root = document.querySelector('.AiAgentPaymentSteering');
        const token = document.querySelector('input[name="link_pay_token"]');
        const merchant = root?.matches('[data-stripe-merchant-account]')
            ? root
            : root?.querySelector('[data-stripe-merchant-account]');
        const account = merchant?.getAttribute('data-stripe-merchant-account');
        return token && account ? account : null;
    })()"#;

    for _ in 0..20 {
        let frames = browser_frames(browser).await?;
        for frame in &frames {
            let _ = evaluate_frame(browser, &frame.frame_id, REVEAL).await;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        for frame in browser_frames(browser).await? {
            let Ok(value) = evaluate_frame(browser, &frame.frame_id, READ_BINDING).await else {
                continue;
            };
            if let Some(merchant_account_id) = value.as_str().filter(|value| !value.is_empty()) {
                return Ok(Some(LinkCheckoutFrame {
                    frame_id: frame.frame_id,
                    merchant_account_id: merchant_account_id.to_owned(),
                }));
            }
        }
    }
    Ok(None)
}

async fn inject_link_pay_token(browser: &Browser, frame_id: &str, token: &str) -> Result<()> {
    let token = serde_json::to_string(token)?;
    let expression = format!(
        r#"((token) => {{
            const input = document.querySelector('input[name="link_pay_token"]');
            if (!input) return false;
            const setter = Object.getOwnPropertyDescriptor(
                window.HTMLInputElement.prototype,
                'value'
            )?.set;
            if (!setter) return false;
            setter.call(input, token);
            input.dispatchEvent(new Event('input', {{ bubbles: true }}));
            return true;
        }})({token})"#
    );
    let injected = evaluate_frame(browser, frame_id, &expression).await?;
    if injected == serde_json::Value::Bool(true) {
        Ok(())
    } else {
        Err(eyre!("Stripe Checkout removed its Link Pay Token input"))
    }
}

async fn wait_for_link_card(browser: &Browser) -> Result<()> {
    const CARD_INPUT_COUNT: &str = r#"document.querySelectorAll(
        'input[autocomplete="cc-number"], input[name="cardnumber"], input[name="cardNumber"], input[data-elements-stable-field-name="cardNumber"]'
    ).length"#;
    let initial = count_across_frames(browser, CARD_INPUT_COUNT).await?;
    if initial == 0 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        return Ok(());
    }
    for _ in 0..40 {
        if count_across_frames(browser, CARD_INPUT_COUNT).await? == 0 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(eyre!(
        "Stripe Checkout did not exchange the approved Link Pay Token for a saved payment method"
    ))
}

async fn submit_link_checkout(browser: &Browser) -> Result<()> {
    const SUBMIT: &str = r#"(() => {
        const visible = (element) => {
            const style = getComputedStyle(element);
            const box = element.getBoundingClientRect();
            return style.visibility !== 'hidden' && style.display !== 'none'
                && box.width > 0 && box.height > 0;
        };
        const buttons = [...document.querySelectorAll('button')].filter(
            (button) => !button.disabled && visible(button)
        );
        const button = buttons.find((candidate) =>
            candidate.type === 'submit'
            && /pay|purchase|buy|subscribe|complete|place order/i.test(candidate.textContent || '')
        ) || buttons.find((candidate) => candidate.type === 'submit');
        if (!button) return false;
        button.click();
        return true;
    })()"#;
    for frame in browser_frames(browser).await? {
        if evaluate_frame(browser, &frame.frame_id, SUBMIT).await? == serde_json::Value::Bool(true)
        {
            return Ok(());
        }
    }
    Err(eyre!(
        "Stripe Checkout did not expose an enabled payment button"
    ))
}

async fn wait_for_checkout_navigation(browser: &Browser, timeout: Duration) {
    let started = tokio::time::Instant::now();
    while started.elapsed() < timeout {
        if let Ok(BrowserActionResult::Url { url, .. }) =
            browser.execute(BrowserAction::GetUrl).await
            && url.contains("/v1/credits/checkout/complete")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn count_across_frames(browser: &Browser, expression: &str) -> Result<u64> {
    let mut count = 0_u64;
    for frame in browser_frames(browser).await? {
        if let Ok(value) = evaluate_frame(browser, &frame.frame_id, expression).await {
            count = count.saturating_add(value.as_u64().unwrap_or_default());
        }
    }
    Ok(count)
}

async fn browser_frames(browser: &Browser) -> Result<Vec<BrowserFrame>> {
    match browser.execute(BrowserAction::ListFrames).await? {
        BrowserActionResult::Frames { frames, .. } => Ok(frames),
        _ => Err(eyre!("browser returned an invalid frame result")),
    }
}

async fn evaluate_frame(
    browser: &Browser,
    frame_id: &str,
    expression: &str,
) -> Result<serde_json::Value> {
    match browser
        .execute(BrowserAction::EvaluateFrame {
            frame_id: frame_id.to_owned(),
            expression: expression.to_owned(),
        })
        .await?
    {
        BrowserActionResult::FrameEvaluation { value, .. } => Ok(value),
        _ => Err(eyre!("browser returned an invalid frame evaluation")),
    }
}

async fn run_link_cli<T: for<'de> Deserialize<'de>>(args: &[&str], timeout: Duration) -> Result<T> {
    run_link_cli_owned(args.iter().map(|arg| (*arg).to_owned()).collect(), timeout).await
}

async fn run_link_cli_owned<T: for<'de> Deserialize<'de>>(
    args: Vec<String>,
    timeout: Duration,
) -> Result<T> {
    let output = tokio::time::timeout(
        timeout,
        AsyncCommand::new("link-cli")
            .args(&args)
            .env("NO_UPDATE_NOTIFIER", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| eyre!("link-cli timed out after {} seconds", timeout.as_secs()))??;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        return Err(eyre!(
            "link-cli exited with {}{}",
            output.status,
            if message.is_empty() {
                String::new()
            } else {
                format!(": {message}")
            }
        ));
    }
    serde_json::from_slice(&output.stdout).wrap_err("link-cli returned invalid JSON")
}

fn format_cents(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

fn describe_created(created: &CreateOrderResponse, json: bool) {
    if !json {
        println!(
            "Order {} created for ${} NANOUSD → {}",
            created.order.id,
            format_units(created.order.package.nanousd_units),
            created.order.wallet
        );
        if created.order.checkout_url.is_none() {
            println!("Mock payment accepted; waiting for Tempo issuance…");
        }
    }
}

async fn wait_for_order(
    client: &CreditsClient,
    id: &str,
    token: &str,
    timeout: Duration,
    progress: bool,
) -> Result<Order> {
    let started = tokio::time::Instant::now();
    let mut previous = None;
    loop {
        let order = client.order(id, token).await?;
        if previous != Some(order.status) && progress {
            println!("Order status: {}", status_label(order.status));
        }
        previous = Some(order.status);
        if order.status == OrderStatus::Fulfilled {
            return Ok(order);
        }
        if order.status == OrderStatus::Expired {
            return Err(eyre!("NanoUSD order {id} expired before payment"));
        }
        if started.elapsed() >= timeout {
            return Err(eyre!(
                "timed out waiting for NanoUSD order {id}; it remains safe to resume with `nanocodex credits wait`"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn print_order(order: &Order, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(order)?);
    } else {
        println!(
            "✓ ${} NANOUSD issued to {}",
            format_units(order.package.nanousd_units),
            order.wallet
        );
        if let Some(transaction_hash) = &order.transaction_hash {
            println!("Tempo transaction: {transaction_hash}");
        }
    }
    Ok(())
}

fn format_units(units: u64) -> String {
    let scale = 10_u64.pow(NANOUSD_DECIMALS);
    format!("{}.{:02}", units / scale, (units % scale) / 10_000)
}

const fn status_label(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Created => "created",
        OrderStatus::AwaitingPayment => "waiting for payment",
        OrderStatus::Paid => "paid",
        OrderStatus::Fulfilling => "issuing NANOUSD",
        OrderStatus::Fulfilled => "fulfilled",
        OrderStatus::Failed => "issuance retry scheduled",
        OrderStatus::Expired => "expired",
    }
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "automatic browser launch is unsupported on this platform",
    ));
    command.arg(url);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "browser launcher exited with {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        credits: Credits,
    }

    #[test]
    fn credits_api_url_is_explicitly_configurable() {
        let cli = TestCli::try_parse_from([
            "nanocodex-credits",
            "--api-url",
            "https://credits.example.test",
            "status",
        ])
        .unwrap();

        assert_eq!(cli.credits.api_url, "https://credits.example.test");
    }

    #[test]
    fn credits_api_url_defaults_to_hosted_service() {
        let cli = TestCli::try_parse_from(["nanocodex-credits", "status"]).unwrap();

        assert_eq!(cli.credits.api_url, DEFAULT_CREDITS_API_URL);
    }
}
