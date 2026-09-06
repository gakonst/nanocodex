use crate::{
    NanocodexError, Result,
    execution::{
        ExecutionAdmission, ExecutionFuture, ExecutionOutput, ExecutionPolicy,
        ExecutionStepAdmission,
    },
    session::SessionSnapshot,
};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
pub(super) struct Journal {
    entries: Mutex<HashMap<String, Entry>>,
    pub(super) lose_ack: AtomicBool,
}

struct Entry {
    kind: String,
    input: String,
    output: Option<String>,
}

impl Journal {
    pub(super) fn input(&self, id: &str) -> String {
        self.entries.lock().unwrap()[id].input.clone()
    }
}

fn interrupted() -> NanocodexError {
    NanocodexError::InvalidExecutionPolicy("simulated journal interruption".into())
}

impl ExecutionPolicy for Journal {
    fn retained_step_input<'a>(
        &'a self,
        _: String,
        id: String,
        kind: String,
    ) -> ExecutionFuture<'a, Result<Option<String>>> {
        Box::pin(async move {
            Ok(self.entries.lock().unwrap().get(&id).map(|entry| {
                assert_eq!(entry.kind, kind);
                entry.input.clone()
            }))
        })
    }
    fn begin_step<'a>(
        &'a self,
        _: String,
        id: String,
        kind: String,
        input: String,
    ) -> ExecutionFuture<'a, Result<ExecutionStepAdmission>> {
        Box::pin(async move {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries.entry(id).or_insert_with(|| Entry {
                kind: kind.clone(),
                input: input.clone(),
                output: None,
            });
            assert_eq!(entry.kind, kind);
            assert_eq!(entry.input, input);
            if kind == "model_call" {
                return Err(interrupted());
            }
            Ok(match &entry.output {
                Some(output) => ExecutionStepAdmission::Replay(output.clone()),
                None => ExecutionStepAdmission::Execute,
            })
        })
    }
    fn complete_step<'a>(
        &'a self,
        _: String,
        id: String,
        output: String,
    ) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async move {
            self.entries.lock().unwrap().get_mut(&id).unwrap().output = Some(output);
            if self.lose_ack.swap(false, Ordering::AcqRel) {
                return Err(interrupted());
            }
            Ok(())
        })
    }
    fn admit<'a>(
        &'a self,
        _: String,
        _: String,
    ) -> ExecutionFuture<'a, Result<ExecutionAdmission>> {
        unreachable!()
    }
    fn admit_automatic<'a>(
        &'a self,
        _: String,
        _: String,
    ) -> ExecutionFuture<'a, Result<(String, ExecutionAdmission)>> {
        unreachable!()
    }
    fn release<'a>(&'a self, _: String) -> ExecutionFuture<'a, ()> {
        unreachable!()
    }
    fn begin_attempt<'a>(&'a self, _: String) -> ExecutionFuture<'a, Result<()>> {
        unreachable!()
    }
    fn complete<'a>(
        &'a self,
        _: String,
        _: SessionSnapshot,
        _: ExecutionOutput,
    ) -> ExecutionFuture<'a, Result<()>> {
        unreachable!()
    }
    fn fail_attempt<'a>(&'a self, _: String, _: String) -> ExecutionFuture<'a, Result<()>> {
        unreachable!()
    }
    fn fail<'a>(
        &'a self,
        _: String,
        _: SessionSnapshot,
        _: String,
    ) -> ExecutionFuture<'a, Result<()>> {
        unreachable!()
    }
}
