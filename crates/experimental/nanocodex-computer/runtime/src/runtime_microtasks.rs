//! Bounded native microtasks retain their originating execution context.
use super::tasks;
use rquickjs::{Ctx, Function, Persistent, prelude::Func};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
struct Task {
    function: Persistent<Function<'static>>,
    context: tasks::Context,
}
#[derive(Default)]
pub(super) struct Queue {
    next: Cell<u32>,
    pending: RefCell<BTreeMap<u32, Task>>,
}
impl Queue {
    pub fn is_empty(&self) -> bool {
        self.pending.borrow().is_empty()
    }
    pub fn clear(&self) {
        self.pending.borrow_mut().clear();
    }
}
pub(super) fn install(
    ctx: &Ctx<'_>,
    queue: Rc<Queue>,
    tasks: Rc<tasks::Tasks>,
) -> rquickjs::Result<()> {
    ctx.globals().set(
        "__skyre_microtask",
        Func::from(move |function: Function<'_>| -> rquickjs::Result<()> {
            let ctx = function.ctx().clone();
            if queue.pending.borrow().len() >= 1024 {
                return Err(rquickjs::Exception::throw_message(
                    &ctx,
                    "Microtask budget exceeded",
                ));
            }
            let id = queue.next.get().checked_add(1).ok_or_else(|| {
                rquickjs::Exception::throw_message(&ctx, "Microtask ID exhausted")
            })?;
            queue.next.set(id);
            queue.pending.borrow_mut().insert(
                id,
                Task {
                    function: Persistent::save(&ctx, function),
                    context: tasks.current.borrow().clone(),
                },
            );
            let pending = queue.clone();
            let origin = tasks.clone();
            let job = Function::new(ctx.clone(), move |ctx: Ctx<'_>| -> rquickjs::Result<()> {
                let task = pending.pending.borrow_mut().remove(&id);
                let Some(task) = task else {
                    return Ok(());
                };
                if origin.poisoned.get() {
                    return Ok(());
                }
                let _scope = origin.scope(task.context);
                if task
                    .function
                    .restore(&ctx)
                    .and_then(|function| function.call::<_, ()>(()))
                    .is_err()
                {
                    let exception = ctx.catch();
                    let detail = exception
                        .as_object()
                        .and_then(|o| o.get::<_, String>("message").ok())
                        .or_else(|| exception.as_string().and_then(|s| s.to_string().ok()))
                        .unwrap_or_else(|| "Uncaught microtask exception".into());
                    origin.fail("uncaught exception", &detail);
                }
                Ok(())
            })?;
            if let Err(error) = job.defer(()) {
                queue.pending.borrow_mut().remove(&id);
                return Err(error);
            }
            Ok(())
        }),
    )
}
