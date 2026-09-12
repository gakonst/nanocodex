//! Rust-only, per-provider-call deadline control. No handle enters JavaScript.
use crate::{Error, Result};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
struct State {
    depth: u32,
    change: Box<dyn Fn(bool) -> Result<()> + Send + Sync>,
    proof: Option<super::DrainProof>,
    continuation: Option<String>,
}
/// Authority to exclude a trusted human approval wait from the current call's
/// execution deadline. A clone becomes inert as soon as that call completes.
#[derive(Clone)]
pub struct ProviderControl(Arc<Shared>);
struct Shared {
    live: AtomicBool,
    state: Mutex<State>,
    execution_check: Option<Arc<dyn Fn() -> Result<()> + Send + Sync>>,
    activation_model: crate::browser_activation::Model,
}
/// A read-only native execution probe, not authority to act or suspend time.
/// Only a runtime-installed validator can provide this capability. Its clone
/// remains tied to the originating provider call's existing lifetime.
#[derive(Clone)]
pub struct ExecutionValidity(Arc<Shared>);
impl ExecutionValidity {
    pub fn validate(&self) -> Result<()> {
        if !self.0.live.load(Ordering::Acquire) {
            return Err(Error::action("Provider call is no longer active"));
        }
        let check = self
            .0
            .execution_check
            .as_ref()
            .ok_or_else(|| Error::action("Native execution validation is unavailable"))?;
        // Never hold the suspension/proof mutex across a native validator.
        let result = check();
        if !self.0.live.load(Ordering::Acquire) {
            return Err(Error::action("Provider call is no longer active"));
        }
        result
    }
}
impl ProviderControl {
    // Production runtimes install their current execution validator explicitly.
    // Keep the legacy liveness-only constructor for native contract fixtures.
    #[cfg(test)]
    pub(crate) fn new(change: impl Fn(bool) -> Result<()> + Send + Sync + 'static) -> Self {
        Self::new_with_execution_check(change, None)
    }
    /// The native caller supplies a current cancellation/deadline probe.
    /// Existing constructors intentionally provide none until their clock and
    /// cancellation owner is explicitly wired; liveness alone is insufficient.
    #[cfg(test)]
    pub(crate) fn new_with_execution_check(
        change: impl Fn(bool) -> Result<()> + Send + Sync + 'static,
        execution_check: Option<Arc<dyn Fn() -> Result<()> + Send + Sync>>,
    ) -> Self {
        Self::new_with_activation_model(change, execution_check, Default::default())
    }
    pub(crate) fn new_with_activation_model(
        change: impl Fn(bool) -> Result<()> + Send + Sync + 'static,
        execution_check: Option<Arc<dyn Fn() -> Result<()> + Send + Sync>>,
        activation_model: crate::browser_activation::Model,
    ) -> Self {
        Self(Arc::new(Shared {
            live: AtomicBool::new(true),
            execution_check,
            activation_model,
            state: Mutex::new(State {
                depth: 0,
                proof: None,
                continuation: None,
                change: Box::new(change),
            }),
        }))
    }
    pub(crate) fn activation_model(&self) -> Result<&crate::browser_activation::Model> {
        self.validate()?;
        Ok(&self.0.activation_model)
    }
    pub fn execution_validity(&self) -> Option<ExecutionValidity> {
        (self.is_active() && self.0.execution_check.is_some())
            .then(|| ExecutionValidity(self.0.clone()))
    }
    pub(crate) fn set_drain_proof(&self, proof: Option<super::DrainProof>) -> Result<()> {
        self.validate()?;
        if let Some(proof) = &proof {
            proof.validate()?;
        }
        self.0.state.lock().unwrap().proof = proof;
        Ok(())
    }
    pub(crate) fn drain_proof(&self) -> Option<super::DrainProof> {
        self.0.state.lock().unwrap().proof.clone()
    }
    pub(crate) fn bind_continuation(&self, id: String) -> Result<()> {
        self.validate()?;
        if id.is_empty() || id.len() > 256 {
            return Err(Error::invalid("Invalid native continuation"));
        }
        self.0.state.lock().unwrap().continuation = Some(id);
        Ok(())
    }
    pub(crate) fn continuation(&self) -> Option<String> {
        self.0.state.lock().unwrap().continuation.clone()
    }
    /// Nonblocking validity probe, including while a resume acknowledgement is pending.
    pub fn is_active(&self) -> bool {
        self.0.live.load(Ordering::Acquire)
    }
    pub fn validate(&self) -> Result<()> {
        if self.is_active() {
            Ok(())
        } else {
            Err(Error::action("Provider call is no longer active"))
        }
    }
    pub fn suspend(&self) -> Result<ProviderSuspension> {
        self.change(true)?;
        Ok(ProviderSuspension {
            control: Some(self.clone()),
            began: Instant::now(),
        })
    }
    pub(crate) fn change(&self, start: bool) -> Result<()> {
        let mut state = self.0.state.lock().unwrap();
        if !self.is_active() {
            return Err(Error::action("Provider call is no longer active"));
        }
        if start {
            let depth = state
                .depth
                .checked_add(1)
                .ok_or_else(|| Error::action("Provider suspension depth exceeded"))?;
            if state.depth == 0 {
                (state.change)(true)?;
            }
            state.depth = depth;
        } else if state.depth > 0 {
            state.depth -= 1;
            if state.depth == 0 {
                (state.change)(false)?;
            }
        }
        Ok(())
    }
    pub(crate) fn close(&self) -> Result<()> {
        if !self.0.live.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let mut state = self.0.state.lock().unwrap();
        if std::mem::take(&mut state.depth) > 0 {
            (state.change)(false)?;
        }
        Ok(())
    }
    pub(crate) fn lifetime(&self) -> ProviderLifetime {
        ProviderLifetime(self.clone())
    }
}
/// A trusted approval interval. Explicit resume reports the measured interval;
/// Drop resumes on early return, cancellation, or unwind.
pub struct ProviderSuspension {
    control: Option<ProviderControl>,
    began: Instant,
}
impl ProviderSuspension {
    pub fn elapsed(&self) -> Duration {
        self.began.elapsed()
    }
    pub fn resume(mut self) -> Result<Duration> {
        self.control.take().unwrap().change(false)?;
        Ok(self.began.elapsed())
    }
}
impl Drop for ProviderSuspension {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            let _ = control.change(false);
        }
    }
}
pub(crate) struct ProviderLifetime(ProviderControl);
impl ProviderLifetime {
    pub fn finish(self) -> Result<()> {
        self.0.close()
    }
}
impl Drop for ProviderLifetime {
    fn drop(&mut self) {
        let _ = self.0.close();
    }
}

#[cfg(test)]
mod execution_validity_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn native_execution_validity_is_absent_from_liveness_only_controls() {
        let control = ProviderControl::new(|_| Ok(()));
        control.validate().unwrap();
        assert!(control.execution_validity().is_none());
        let lifetime = control.lifetime();
        lifetime.finish().unwrap();
        assert!(!control.is_active());
        assert!(control.execution_validity().is_none());
    }

    #[test]
    fn native_execution_validity_probes_never_change_suspension_or_drain_state() {
        let changes = Arc::new(Mutex::new(Vec::new()));
        let changed = changes.clone();
        let checks = Arc::new(AtomicUsize::new(0));
        let checked = checks.clone();
        let control = ProviderControl::new_with_execution_check(
            move |start| {
                changed.lock().unwrap().push(start);
                Ok(())
            },
            Some(Arc::new(move || {
                checked.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })),
        );
        control
            .bind_continuation("native-existing-continuation".into())
            .unwrap();
        let validity = control.execution_validity().unwrap();
        validity.validate().unwrap();
        validity.validate().unwrap();
        assert_eq!(checks.load(Ordering::Acquire), 2);
        assert!(changes.lock().unwrap().is_empty());
        assert_eq!(
            control.continuation().as_deref(),
            Some("native-existing-continuation")
        );
        assert!(control.drain_proof().is_none());
        assert_eq!(control.0.state.lock().unwrap().depth, 0);
        // The preexisting explicit suspension API remains independently usable.
        control.suspend().unwrap().resume().unwrap();
        assert_eq!(*changes.lock().unwrap(), [true, false]);
        assert_eq!(checks.load(Ordering::Acquire), 2);
    }

    #[test]
    fn native_execution_validity_reads_current_validator_and_cannot_outlive_call() {
        let healthy = Arc::new(AtomicBool::new(true));
        let current = healthy.clone();
        let control = ProviderControl::new_with_execution_check(
            |_| Ok(()),
            Some(Arc::new(move || {
                if current.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(Error::new(-32800, "Owned native execution expired"))
                }
            })),
        );
        let validity = control.execution_validity().unwrap();
        validity.validate().unwrap();
        healthy.store(false, Ordering::Release);
        assert_eq!(
            validity.validate().unwrap_err().message,
            "Owned native execution expired"
        );
        healthy.store(true, Ordering::Release);
        validity.validate().unwrap();
        control.lifetime().finish().unwrap();
        assert_eq!(
            validity.validate().unwrap_err().message,
            "Provider call is no longer active"
        );
        assert!(control.execution_validity().is_none());
    }

    #[test]
    fn native_execution_validity_rechecks_lifetime_after_unlocked_validator() {
        let holder: Arc<Mutex<Option<ProviderControl>>> = Arc::new(Mutex::new(None));
        let callback_owner = holder.clone();
        let control = ProviderControl::new_with_execution_check(
            |_| Ok(()),
            Some(Arc::new(move || {
                let owner = callback_owner.lock().unwrap().take().unwrap();
                // Fail explicitly if validation ever holds the suspension lock;
                // this regression must not hang while attempting close().
                drop(
                    owner
                        .0
                        .state
                        .try_lock()
                        .map_err(|_| Error::action("Suspension state was locked"))?,
                );
                owner.lifetime().finish()?;
                Ok(())
            })),
        );
        *holder.lock().unwrap() = Some(control.clone());
        let validity = control.execution_validity().unwrap();
        assert_eq!(
            validity.validate().unwrap_err().message,
            "Provider call is no longer active"
        );
        assert!(!control.is_active());
    }
}
