//! V8 Promise hooks preserve the originating execution without a JS-visible key.
use super::{super::tasks::Context as TaskContext, State};
use std::{cell::RefCell, rc::Rc};
pub(super) extern "C" fn promise_hook(
    kind: v8::PromiseHookType,
    promise: v8::Local<v8::Promise>,
    _: v8::Local<v8::Value>,
) {
    v8::callback_scope!(unsafe scope,promise);
    let Some(state) = scope.get_slot::<Rc<RefCell<State>>>().cloned() else {
        return;
    };
    let tasks = state.borrow().tasks.clone();
    if kind == v8::PromiseHookType::After {
        tasks.leave();
        return;
    }
    if kind == v8::PromiseHookType::Resolve {
        return;
    }
    let Some(key) = state
        .borrow()
        .promise_key
        .as_ref()
        .map(|key| v8::Local::new(scope, key))
    else {
        return;
    };
    let object: v8::Local<v8::Object> = promise.into();
    match kind {
        v8::PromiseHookType::Init => {
            let task = tasks.current.borrow().clone();
            let id = v8::Number::new(scope, task.id as f64);
            let Some(metadata) = v8::String::new(scope, &task.metadata) else {
                tasks.poisoned.set(true);
                return;
            };
            // Every native slot is an own data property from creation. Missing
            // indices would consult mutable JavaScript prototype properties.
            let request = v8::Integer::new_from_unsigned(scope, 0);
            let record =
                v8::Array::new_with_elements(scope, &[id.into(), metadata.into(), request.into()]);
            if object.set_private(scope, key, record.into()) != Some(true) {
                tasks.poisoned.set(true);
            }
        }
        v8::PromiseHookType::Before => {
            let record = object
                .get_private(scope, key)
                .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok());
            let context = record
                .and_then(|record| {
                    let id = record.get_index(scope, 0)?.number_value(scope)? as u64;
                    let metadata = record
                        .get_index(scope, 1)?
                        .to_string(scope)?
                        .to_rust_string_lossy(scope);
                    Some(TaskContext {
                        id,
                        metadata: Rc::from(metadata),
                    })
                })
                .unwrap_or_default();
            tasks.enter(context);
        }
        _ => {}
    }
}
pub(super) extern "C" fn rejected(message: v8::PromiseRejectMessage) {
    v8::callback_scope!(unsafe scope,&message);
    let Some(state) = scope.get_slot::<Rc<RefCell<State>>>().cloned() else {
        return;
    };
    let promise = message.get_promise();
    let mut state = state.borrow_mut();
    match message.get_event() {
        v8::PromiseRejectEvent::PromiseHandlerAddedAfterReject => {
            state.rejections.retain(|previous| {
                !v8::Local::<v8::Value>::from(v8::Local::new(scope, previous))
                    .strict_equals(promise.into())
            });
        }
        v8::PromiseRejectEvent::PromiseRejectWithNoHandler => {
            if state.rejections.len() >= 1024 {
                state.tasks.poisoned.set(true);
            } else {
                state.rejections.push(v8::Global::new(scope, promise));
            }
        }
        _ => {}
    }
}

// Explicit trusted wrapper derivation only. Hooks above never copy request tags.
pub(super) fn request_tag(
    scope: &mut v8::PinScope<'_, '_>,
    state: &Rc<RefCell<State>>,
    value: v8::Local<v8::Value>,
) -> Option<u32> {
    let promise = v8::Local::<v8::Promise>::try_from(value).ok()?;
    let key = state
        .borrow()
        .promise_key
        .as_ref()
        .map(|key| v8::Local::new(scope, key))?;
    let object: v8::Local<v8::Object> = promise.into();
    let record = v8::Local::<v8::Array>::try_from(object.get_private(scope, key)?).ok()?;
    let id = v8::Local::<v8::Uint32>::try_from(record.get_index(scope, 2)?)
        .ok()?
        .value();
    state.borrow().drain.contains(id).then_some(id)
}
pub(super) fn tag_request(
    scope: &mut v8::PinScope<'_, '_>,
    state: &Rc<RefCell<State>>,
    value: v8::Local<v8::Value>,
    id: u32,
) -> Option<()> {
    let promise = v8::Local::<v8::Promise>::try_from(value).ok()?;
    let key = state
        .borrow()
        .promise_key
        .as_ref()
        .map(|key| v8::Local::new(scope, key))?;
    let object: v8::Local<v8::Object> = promise.into();
    let record = v8::Local::<v8::Array>::try_from(object.get_private(scope, key)?).ok()?;
    let id = v8::Integer::new_from_unsigned(scope, id);
    (record.set_index(scope, 2, id.into()) == Some(true)).then_some(())
}
