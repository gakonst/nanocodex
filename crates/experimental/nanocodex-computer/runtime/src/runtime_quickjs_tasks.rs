//! Engine-native Promise hooks retain task identity without patching Promise.
use super::tasks::{Context as TaskContext, Tasks};
use rquickjs::{
    Context, Function, Object, Persistent, Runtime, Value, prelude::This, promise::PromiseHookType,
};
use std::{cell::RefCell, collections::HashSet, rc::Rc};
pub(super) type Rejections = Rc<RefCell<HashSet<Persistent<Value<'static>>>>>;
pub(super) fn install(
    runtime: &Runtime,
    context: &Context,
    tasks: Rc<Tasks>,
) -> rquickjs::Result<Rejections> {
    let (map, get, set) = context.with(|ctx| {
        let map = ctx.eval::<Object, _>("new WeakMap()")?;
        let get = ctx.eval::<Function, _>("WeakMap.prototype.get")?;
        let set = ctx.eval::<Function, _>("WeakMap.prototype.set")?;
        Ok::<_, rquickjs::Error>((
            Persistent::save(&ctx, map),
            Persistent::save(&ctx, get),
            Persistent::save(&ctx, set),
        ))
    })?;
    let hook_tasks = tasks.clone();
    runtime.set_promise_hook(Some(Box::new(move |ctx, kind, promise, _| {
        let result = (|| -> rquickjs::Result<()> {
            match kind {
                PromiseHookType::Init => {
                    let task = hook_tasks.current.borrow().clone();
                    let record = Object::new(ctx.clone())?;
                    record.set_prototype(None)?;
                    record.set("id", task.id as f64)?;
                    record.set("metadata", task.metadata.as_ref())?;
                    set.clone().restore(&ctx)?.call::<_, Value>((
                        This(map.clone().restore(&ctx)?),
                        promise,
                        record,
                    ))?;
                }
                PromiseHookType::Before => {
                    let record = get
                        .clone()
                        .restore(&ctx)?
                        .call::<_, Value>((This(map.clone().restore(&ctx)?), promise))?;
                    let task = match record.as_object() {
                        Some(record) => TaskContext {
                            id: record.get::<_, f64>("id")? as u64,
                            metadata: Rc::from(record.get::<_, String>("metadata")?),
                        },
                        None => TaskContext::default(),
                    };
                    hook_tasks.enter(task);
                }
                PromiseHookType::After => hook_tasks.leave(),
                PromiseHookType::Resolve => {}
            }
            Ok(())
        })();
        if result.is_err() {
            hook_tasks.poisoned.set(true);
            let _ = ctx.catch();
        }
    })));
    let rejected: Rejections = Rc::new(RefCell::new(HashSet::new()));
    let records = rejected.clone();
    runtime.set_host_promise_rejection_tracker(Some(Box::new(move |ctx, promise, _, handled| {
        let promise = Persistent::save(&ctx, promise);
        if handled {
            records.borrow_mut().remove(&promise);
        } else {
            if records.borrow().len() >= 1024 {
                tasks.poisoned.set(true);
            } else {
                records.borrow_mut().insert(promise);
            }
        }
    })));
    Ok(rejected)
}
