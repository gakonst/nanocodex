use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    eips::Encodable2718,
    network::{ReceiptResponse, TransactionBuilder},
    primitives::{Address, B256, U256, keccak256},
    providers::{
        DynProvider, PendingTransactionBuilder, Provider, ProviderBuilder,
        fillers::{FillProvider, TxFiller},
    },
};
use async_trait::async_trait;
use tempo_alloy::{
    TempoNetwork, accounts::TempoAccountsWallet, contracts::precompiles::ITIP20,
    primitives::TempoTxEnvelope, provider::TempoProviderBuilderExt, rpc::TempoTransactionRequest,
};

use crate::db::Fulfillment;

#[derive(Debug, Clone)]
pub(crate) struct PreparedMint {
    pub signed_transaction: Vec<u8>,
    pub transaction_hash: String,
    pub valid_before: u64,
}

#[async_trait]
pub(crate) trait Issuer: Send + Sync {
    async fn balance(&self, wallet: Address) -> Result<u64, IssuerError>;
    async fn prepare(&self, order: &Fulfillment) -> Result<PreparedMint, IssuerError>;
    async fn publish(&self, order: &Fulfillment, mint: &PreparedMint) -> Result<(), IssuerError>;
}

#[derive(Default)]
pub(crate) struct MockIssuer {
    balances: Mutex<HashMap<Address, u64>>,
    fulfilled: Mutex<HashSet<String>>,
}

#[async_trait]
impl Issuer for MockIssuer {
    async fn balance(&self, wallet: Address) -> Result<u64, IssuerError> {
        Ok(*self
            .balances
            .lock()
            .map_err(|_| IssuerError::Poisoned)?
            .get(&wallet)
            .unwrap_or(&0))
    }

    async fn prepare(&self, order: &Fulfillment) -> Result<PreparedMint, IssuerError> {
        Ok(PreparedMint {
            signed_transaction: Vec::new(),
            transaction_hash: order
                .transaction_hash
                .clone()
                .unwrap_or_else(|| format!("{:#x}", keccak256(order.id.as_bytes()))),
            valid_before: u64::MAX,
        })
    }

    async fn publish(&self, order: &Fulfillment, _mint: &PreparedMint) -> Result<(), IssuerError> {
        let mut fulfilled = self.fulfilled.lock().map_err(|_| IssuerError::Poisoned)?;
        if fulfilled.insert(order.id.clone()) {
            let mut balances = self.balances.lock().map_err(|_| IssuerError::Poisoned)?;
            let balance = balances.entry(order.wallet).or_default();
            *balance = balance.saturating_add(order.amount);
        }
        Ok(())
    }
}

#[async_trait]
trait TransactionPreparer: Send + Sync {
    async fn prepare(&self, request: TempoTransactionRequest) -> Result<PreparedMint, IssuerError>;
}

#[async_trait]
impl<F, P> TransactionPreparer for FillProvider<F, P, TempoNetwork>
where
    F: TxFiller<TempoNetwork> + Send + Sync,
    P: Provider<TempoNetwork> + Send + Sync,
{
    async fn prepare(&self, request: TempoTransactionRequest) -> Result<PreparedMint, IssuerError> {
        let envelope = self
            .fill(request)
            .await
            .map_err(alloy_error)?
            .try_into_envelope()
            .map_err(alloy_error)?;
        let TempoTxEnvelope::AA(transaction) = &envelope else {
            return Err(IssuerError::NonTempoTransaction);
        };
        let valid_before = transaction
            .tx()
            .valid_before
            .ok_or(IssuerError::MissingExpiry)?
            .get();
        let signed_transaction = envelope.encoded_2718();
        Ok(PreparedMint {
            transaction_hash: format!("{:#x}", keccak256(&signed_transaction)),
            signed_transaction,
            valid_before,
        })
    }
}

pub(crate) struct AlloyIssuer {
    provider: DynProvider<TempoNetwork>,
    preparer: Arc<dyn TransactionPreparer>,
    token: Address,
    fee_token: Address,
}

impl AlloyIssuer {
    pub fn new(
        rpc_url: &str,
        token: Address,
        fee_token: Address,
        wallet_store: Option<&Path>,
    ) -> Result<Self, IssuerError> {
        nanousd::install_default_rustls_crypto_provider();
        let wallet = wallet_store.map_or_else(
            TempoAccountsWallet::from_default_store,
            TempoAccountsWallet::from_store,
        )?;
        let account = wallet.account();
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .with_expiring_nonces()
            .filler(wallet)
            .connect_http(rpc_url.parse().map_err(alloy_error)?);
        let preparer = Arc::new(provider.clone());
        tracing::info!(%account, "loaded NanoUSD Alloy issuer");
        Ok(Self {
            provider: provider.erased(),
            preparer,
            token,
            fee_token,
        })
    }

    async fn receipt(&self, transaction_hash: &str) -> Result<Option<bool>, IssuerError> {
        let hash: B256 = transaction_hash.parse().map_err(alloy_error)?;
        self.provider
            .get_transaction_receipt(hash)
            .await
            .map(|receipt| receipt.map(|receipt| receipt.status()))
            .map_err(alloy_error)
    }
}

#[async_trait]
impl Issuer for AlloyIssuer {
    async fn balance(&self, wallet: Address) -> Result<u64, IssuerError> {
        let balance = ITIP20::new(self.token, &self.provider)
            .balanceOf(wallet)
            .call()
            .await
            .map_err(alloy_error)?;
        u64::try_from(balance).map_err(|_| IssuerError::BalanceOverflow)
    }

    async fn prepare(&self, order: &Fulfillment) -> Result<PreparedMint, IssuerError> {
        if let (Some(signed_transaction), Some(transaction_hash), Some(valid_before)) = (
            &order.signed_transaction,
            &order.transaction_hash,
            order.valid_before,
        ) && (self.receipt(transaction_hash).await?.is_some()
            || valid_before > unix_time()?.saturating_add(1))
        {
            return Ok(PreparedMint {
                signed_transaction: signed_transaction.clone(),
                transaction_hash: transaction_hash.clone(),
                valid_before,
            });
        }

        let request = ITIP20::new(self.token, &self.provider)
            .mint(order.wallet, U256::from(order.amount))
            .into_transaction_request()
            .with_chain_id(nanousd::TEMPO_MAINNET_CHAIN_ID)
            .with_fee_token(self.fee_token);
        self.preparer.prepare(request).await
    }

    async fn publish(&self, _order: &Fulfillment, mint: &PreparedMint) -> Result<(), IssuerError> {
        if self.receipt(&mint.transaction_hash).await?.is_none()
            && let Err(error) = self
                .provider
                .send_raw_transaction(&mint.signed_transaction)
                .await
            && self.receipt(&mint.transaction_hash).await?.is_none()
        {
            return Err(alloy_error(error));
        }
        let hash: B256 = mint.transaction_hash.parse().map_err(alloy_error)?;
        let receipt = PendingTransactionBuilder::new(self.provider.root().clone(), hash)
            .get_receipt()
            .await
            .map_err(alloy_error)?;
        if receipt.status() {
            Ok(())
        } else {
            Err(IssuerError::TransactionReverted(
                mint.transaction_hash.clone(),
            ))
        }
    }
}

fn unix_time() -> Result<u64, IssuerError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| IssuerError::Clock)
}

fn alloy_error(error: impl std::fmt::Display) -> IssuerError {
    IssuerError::Alloy(error.to_string())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum IssuerError {
    #[error("failed to load the Tempo issuer wallet: {0}")]
    Wallet(#[from] tempo_alloy::accounts::TempoAccountsError),
    #[error("Tempo Alloy operation failed: {0}")]
    Alloy(String),
    #[error("NanoUSD balance does not fit in the API representation")]
    BalanceOverflow,
    #[error("Alloy prepared a non-Tempo transaction")]
    NonTempoTransaction,
    #[error("Alloy prepared a Tempo transaction without an expiry")]
    MissingExpiry,
    #[error("Tempo mint transaction {0} reverted")]
    TransactionReverted(String),
    #[error("system clock is before the Unix epoch")]
    Clock,
    #[error("mock issuer balance lock was poisoned")]
    Poisoned,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use alloy::primitives::Address;
    use serde_json::json;

    use super::AlloyIssuer;

    const ROOT: &str = "0x1111111111111111111111111111111111111111";
    const TOKEN: &str = "0x2222222222222222222222222222222222222222";
    const FEE_TOKEN: &str = "0x3333333333333333333333333333333333333333";

    #[test]
    fn construction_keeps_scoped_accounts_keys_lazy() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store.json");
        fs::write(
            &store,
            serde_json::to_vec(&json!({
                "tempo-cli.store": {
                    "state": {
                        "activeAccount": 0,
                        "chainId": nanousd::TEMPO_MAINNET_CHAIN_ID,
                        "accounts": [{ "address": ROOT }],
                        "accessKeys": [{
                            "access": ROOT,
                            "address": "0x4444444444444444444444444444444444444444",
                            "chainId": nanousd::TEMPO_MAINNET_CHAIN_ID,
                            "keyType": "p256",
                            "privateKey": format!("0x{}", "01".repeat(32)),
                            "scopes": [{
                                "address": TOKEN,
                                "selector": "mint(address,uint256)",
                            }],
                        }],
                    },
                },
            }))
            .unwrap(),
        )
        .unwrap();

        let issuer = AlloyIssuer::new(
            "http://127.0.0.1:1",
            TOKEN.parse::<Address>().unwrap(),
            FEE_TOKEN.parse::<Address>().unwrap(),
            Some(&store),
        );

        assert!(issuer.is_ok());
    }
}
