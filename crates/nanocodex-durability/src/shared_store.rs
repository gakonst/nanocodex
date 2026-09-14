use std::future::Future;

use tokio::sync::{mpsc, oneshot};

#[cfg(not(target_family = "wasm"))]
use crate::Error;
use crate::{OwnedState, OwnerId, OwnerToken, StateStore, StoreError, StoreFuture};

const COMMAND_CAPACITY: usize = 64;

enum Command {
    ReadRecords {
        state_id: String,
        keys: Vec<String>,
        result: oneshot::Sender<Result<Vec<Option<String>>, StoreError>>,
    },
    ReadRecord {
        state_id: String,
        key: String,
        result: oneshot::Sender<Result<Option<String>, StoreError>>,
    },
    Acquire {
        state_id: String,
        owner_id: OwnerId,
        result: oneshot::Sender<Result<OwnedState, StoreError>>,
    },
    Replace {
        state_id: String,
        owner: OwnerToken,
        expected_revision: u64,
        payload: String,
        records: Vec<crate::StoreRecord>,
        result: oneshot::Sender<Result<u64, StoreError>>,
    },
}

/// Cloneable serialized access to one caller-owned store.
///
/// Each durable agent still owns an independent state driver and fencing token.
/// This handle only lets those drivers address their own state IDs through a
/// backend whose public contract requires exclusive mutable access.
#[derive(Clone)]
pub(crate) struct SharedStore {
    commands: mpsc::Sender<Command>,
}

impl SharedStore {
    pub(crate) fn new<S>(mut store: S) -> crate::Result<Self>
    where
        S: StateStore + 'static,
    {
        let (commands, mut receiver) = mpsc::channel(COMMAND_CAPACITY);
        spawn_driver(async move {
            while let Some(command) = receiver.recv().await {
                match command {
                    Command::ReadRecords {
                        state_id,
                        keys,
                        result,
                    } => {
                        drop(result.send(store.read_records(&state_id, &keys).await));
                    }
                    Command::ReadRecord {
                        state_id,
                        key,
                        result,
                    } => {
                        drop(result.send(store.read_record(&state_id, &key).await));
                    }

                    Command::Acquire {
                        state_id,
                        owner_id,
                        result,
                    } => {
                        drop(result.send(store.acquire(&state_id, owner_id).await));
                    }
                    Command::Replace {
                        state_id,
                        owner,
                        expected_revision,
                        payload,
                        records: new_records,
                        result,
                    } => {
                        drop(
                            result.send(
                                store
                                    .replace(
                                        &state_id,
                                        &owner,
                                        expected_revision,
                                        &payload,
                                        &new_records,
                                    )
                                    .await,
                            ),
                        );
                    }
                }
            }
        })?;
        Ok(Self { commands })
    }
}

impl StateStore for SharedStore {
    fn read_records<'a>(
        &'a mut self,
        state_id: &'a str,
        keys: &'a [String],
    ) -> StoreFuture<'a, Result<Vec<Option<String>>, StoreError>> {
        Box::pin(async move {
            let (result, receiver) = oneshot::channel();
            self.commands
                .send(Command::ReadRecords {
                    state_id: state_id.to_owned(),
                    keys: keys.to_vec(),
                    result,
                })
                .await
                .map_err(|_| stopped())?;
            receiver.await.map_err(|_| stopped())?
        })
    }

    fn read_record<'a>(
        &'a mut self,
        state_id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        Box::pin(async move {
            let (result, receiver) = oneshot::channel();
            self.commands
                .send(Command::ReadRecord {
                    state_id: state_id.to_owned(),
                    key: key.to_owned(),
                    result,
                })
                .await
                .map_err(|_| stopped())?;
            receiver.await.map_err(|_| stopped())?
        })
    }

    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        Box::pin(async move {
            let (result, receiver) = oneshot::channel();
            self.commands
                .send(Command::Acquire {
                    state_id: state_id.to_owned(),
                    owner_id,
                    result,
                })
                .await
                .map_err(|_| stopped())?;
            receiver.await.map_err(|_| stopped())?
        })
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
        records: &'a [crate::StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let (result, receiver) = oneshot::channel();
            self.commands
                .send(Command::Replace {
                    state_id: state_id.to_owned(),
                    owner: owner.clone(),
                    expected_revision,
                    payload: payload.to_owned(),
                    records: records.to_vec(),
                    result,
                })
                .await
                .map_err(|_| stopped())?;
            receiver.await.map_err(|_| stopped())?
        })
    }
}

fn stopped() -> StoreError {
    StoreError::Backend("shared durability store stopped".to_owned())
}

#[cfg(not(target_family = "wasm"))]
fn spawn_driver(driver: impl Future<Output = ()> + Send + 'static) -> crate::Result<()> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::RuntimeUnavailable)?;
    drop(runtime.spawn(driver));
    Ok(())
}

#[cfg(target_family = "wasm")]
fn spawn_driver(driver: impl Future<Output = ()> + 'static) -> crate::Result<()> {
    wasm_bindgen_futures::spawn_local(driver);
    Ok(())
}
