use super::*;
fn raise(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    let Some(text) = v8::String::new(scope, message) else {
        return;
    };
    let exception = v8::Exception::error(scope, text);
    scope.throw_exception(exception);
}
fn return_text(scope: &mut v8::PinScope<'_, '_>, out: &mut v8::ReturnValue, text: &str) {
    if let Some(value) = v8::String::new(scope, text) {
        out.set(value.into());
    }
}
fn callback(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut out: v8::ReturnValue,
) {
    let name = args.data().to_rust_string_lossy(scope);
    let state = scope.get_slot::<Rc<RefCell<State>>>().unwrap().clone();
    let text =
        |index: i32, scope: &mut v8::PinScope<'_, '_>| args.get(index).to_rust_string_lossy(scope);
    match name.as_str() {
        "__skyre_request_meta" => {
            return_text(
                scope,
                &mut out,
                &state.borrow().tasks.current.borrow().metadata,
            );
        }
        "__skyre_cell_active" => out.set_bool({
            let state = state.borrow();
            state.active
                && !state.tasks.poisoned.get()
                && state.tasks.current.borrow().id == state.cell
        }),
        "__skyre_cell_id" => out.set_double(state.borrow().tasks.current.borrow().id as f64),
        "__skyre_fs" | "__skyre_codec" => {
            let input = text(0, scope);
            let result = if name == "__skyre_fs" {
                state.borrow_mut().files.call(&input)
            } else {
                state.borrow_mut().codecs.call(&input)
            };
            return_text(scope, &mut out, &result);
        }
        "__skyre_form_decode" => {
            let input = text(0, scope);
            let result = serde_json::to_string(
                &url::form_urlencoded::parse(input.as_bytes()).collect::<Vec<_>>(),
            )
            .unwrap();
            return_text(scope, &mut out, &result);
        }
        "__skyre_url_parse" | "__skyre_read_image_file" | "__skyre_image_data_url" => {
            let result = helpers::helper_response(match name.as_str() {
                "__skyre_url_parse" => helpers::parse_url(&text(0, scope)),
                "__skyre_read_image_file" => helpers::read_image_file(&text(0, scope)),
                "__skyre_image_data_url" => helpers::image_data_url(&text(0, scope)),
                _ => unreachable!(),
            });
            return_text(scope, &mut out, &result);
        }
        "__skyre_operation_register" => {
            let request = tasks::request_tag(scope, &state, args.get(0));
            let cell = state.borrow().tasks.current.borrow().id;
            let result = state.borrow_mut().drain.register(cell, request);
            match result {
                Ok(id) => out.set_uint32(id),
                Err(error) => raise(scope, &error.message),
            }
        }
        "__skyre_operation_finish" => {
            if let Some(id) = args.get(0).uint32_value(scope) {
                state.borrow_mut().drain.complete(id);
            }
        }
        "__skyre_operation_derive" => {
            if let Some(id) = tasks::request_tag(scope, &state, args.get(0)) {
                tasks::tag_request(scope, &state, args.get(1), id);
            }
        }
        "__skyre_rpc" => {
            let method = text(0, scope);
            let input = text(1, scope);
            {
                let state = state.borrow();
                if !state.active
                    || state.tasks.poisoned.get()
                    || state.tasks.current.borrow().id != state.cell
                {
                    raise(scope, "node_repl exec context not found");
                    return;
                }
            }
            let Some(resolver) = v8::PromiseResolver::new(scope) else {
                return;
            };
            let promise = resolver.get_promise(scope);
            let request = match state.borrow_mut().drain.request() {
                Ok(id) => id,
                Err(error) => {
                    raise(scope, &error.message);
                    return;
                }
            };
            if tasks::tag_request(scope, &state, promise.into(), request).is_none() {
                raise(scope, "Cannot tag native continuation");
                return;
            }
            let context = state.borrow().tasks.current.borrow().clone();
            let suspended = args.get(2).boolean_value(scope);
            let resolver = v8::Global::new(scope, resolver);
            let result = state
                .borrow_mut()
                .pending_rpc
                .push(method, input, resolver, context, suspended, request);
            match result {
                Ok(()) => out.set(promise.into()),
                Err(error) => raise(scope, &error.message),
            }
        }
        "__skyre_write" => {
            let text = text(0, scope);
            let channel = args.get(1).to_rust_string_lossy(scope);
            let kind = if args.get(2).is_undefined() {
                if channel == "image" {
                    "image"
                } else if channel == "output" {
                    "write"
                } else {
                    "named"
                }
                .into()
            } else {
                args.get(2).to_rust_string_lossy(scope)
            };
            let mut state = state.borrow_mut();
            if !state.active
                || state.tasks.poisoned.get()
                || state.tasks.current.borrow().id != state.cell
            {
                if kind == "line" {
                    return;
                }
                raise(scope, "node_repl exec context not found");
                return;
            }
            if state.writes >= 256
                || text.len()
                    + state
                        .outputs
                        .iter()
                        .map(|value| value.to_string().len())
                        .sum::<usize>()
                    > 4 * 1024 * 1024
            {
                raise(scope, "Cell output budget exceeded (256 items / 4 MiB)");
                return;
            }
            state.writes += 1;
            let value: Value = serde_json::from_str(&text).unwrap_or(json!(text));
            let named = kind == "named";
            if kind != "image"
                && value.is_string()
                && let Some(previous) = state.outputs.iter_mut().find(|v| {
                    v["channel"] == channel && v["named"] == named && v["value"].is_string()
                })
            {
                previous["value"] = json!(format!(
                    "{}{}",
                    previous["value"].as_str().unwrap(),
                    value.as_str().unwrap()
                ));
                previous["kind"] = json!(kind);
            } else {
                state
                    .outputs
                    .push(json!({"channel":channel,"value":value,"named":named,"kind":kind}));
            }
        }
        "__skyre_event_now" => {
            out.set_double(state.borrow().event_clock_origin.elapsed().as_secs_f64() * 1000.0);
        }
        "__skyre_timer_schedule" => {
            let Ok(function) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
                raise(scope, "Timeout callback must be a function");
                return;
            };
            let delay = args.get(1).number_value(scope).unwrap_or(f64::NAN);
            if !delay.is_finite()
                || !(0.0..=2147483647.0).contains(&delay)
                || state.borrow().timers.len() >= 1024
            {
                raise(scope, "Invalid timeout or too many timers");
                return;
            }
            let function = v8::Global::new(scope, function);
            let mut state = state.borrow_mut();
            let Some(id) = state.next_timer.checked_add(1) else {
                raise(scope, "Timer ID exhausted");
                return;
            };
            state.next_timer = id;
            let context = state.tasks.current.borrow().clone();
            state.timers.insert(
                id,
                super::Timer {
                    immediate: args.get(2).uint32_value(scope) == Some(1),
                    when: Instant::now() + Duration::from_secs_f64(delay / 1000.0),
                    function,
                    context,
                },
            );
            out.set_uint32(id);
        }
        "__skyre_timer_clear" => {
            let id = args.get(0).uint32_value(scope).unwrap_or(0);
            state.borrow_mut().timers.remove(&id);
        }
        "__skyre_timer_refresh" => {
            let id = args.get(0).uint32_value(scope).unwrap_or(0);
            let delay = args.get(1).number_value(scope).unwrap_or(f64::NAN);
            let mut state = state.borrow_mut();
            if delay.is_finite()
                && (0.0..=2147483647.0).contains(&delay)
                && let Some(timer) = state.timers.get_mut(&id)
            {
                timer.when = Instant::now() + Duration::from_secs_f64(delay / 1000.0);
                out.set_bool(true);
            } else {
                out.set_bool(false);
            }
        }
        "__skyre_microtask" => {
            let Ok(function) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
                raise(scope, "Microtask callback must be a function");
                return;
            };
            let mut state = state.borrow_mut();
            if state.microtasks.len() >= 1024 {
                raise(scope, "Microtask budget exceeded");
                return;
            }
            let Some(id) = state.next_microtask.checked_add(1) else {
                raise(scope, "Microtask ID exhausted");
                return;
            };
            let data = v8::Integer::new_from_unsigned(scope, id);
            let Some(job) = v8::Function::builder(microtask)
                .data(data.into())
                .build(scope)
            else {
                return;
            };
            let context = state.tasks.current.borrow().clone();
            state.next_microtask = id;
            state.microtasks.insert(
                id,
                super::Microtask {
                    function: v8::Global::new(scope, function),
                    context,
                },
            );
            scope.enqueue_microtask(job);
        }
        "__skyre_suspend_timeout" => {
            let mut state = state.borrow_mut();
            if !state.active
                || state.tasks.poisoned.get()
                || state.tasks.current.borrow().id != state.cell
            {
                raise(scope, "node_repl exec context not found");
                return;
            }
            let mut clock = state.control.clock.lock().unwrap();
            let start = args.get(0).boolean_value(scope);
            if start {
                if clock.suspended == 0 {
                    clock.suspended_at = Some(Instant::now());
                }
                let Some(depth) = clock.suspended.checked_add(1) else {
                    raise(scope, "Timeout suspension depth exceeded");
                    return;
                };
                clock.suspended = depth;
            } else if clock.suspended > 0 {
                clock.suspended -= 1;
                if clock.suspended == 0 {
                    let elapsed = clock.suspended_at.take().unwrap().elapsed();
                    clock.deadline = clock
                        .deadline
                        .and_then(|deadline| deadline.checked_add(elapsed));
                }
            }
            drop(clock);
            if let Some(observer) = &mut state.timeout_observer
                && let Err(error) = observer(start)
            {
                raise(scope, &error.message);
            }
        }
        "__skyre_response_meta" => {
            let input = text(0, scope);
            let mut state = state.borrow_mut();
            if !state.active
                || state.tasks.poisoned.get()
                || state.tasks.current.borrow().id != state.cell
            {
                raise(scope, "node_repl exec context not found");
                return;
            }
            let Ok(Value::Object(meta)) = serde_json::from_str(&input) else {
                raise(scope, "Response metadata must be an object");
                return;
            };
            let mut merged = state.meta.clone();
            merged.extend(meta);
            if serde_json::to_vec(&merged).unwrap().len() > 65536 {
                raise(scope, "Response metadata exceeds 64 KiB");
                return;
            }
            state.meta = merged;
        }
        _ => raise(scope, "Unknown Rust host callback"),
    }
}
fn microtask(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _out: v8::ReturnValue,
) {
    let state = scope.get_slot::<Rc<RefCell<State>>>().unwrap().clone();
    let id = args.data().uint32_value(scope).unwrap_or(0);
    let pending = state.borrow_mut().microtasks.remove(&id);
    let Some(pending) = pending else {
        return;
    };
    let tasks = state.borrow().tasks.clone();
    if tasks.poisoned.get() {
        return;
    }
    let _origin = tasks.scope(pending.context);
    v8::tc_scope!(let scope,scope);
    let function = v8::Local::new(scope, &pending.function);
    let undefined = v8::undefined(scope);
    if function.call(scope, undefined.into(), &[]).is_none() && !scope.is_execution_terminating() {
        let error = exception(scope);
        let detail = state
            .borrow_mut()
            .exception_message
            .take()
            .unwrap_or(error.message);
        tasks.fail("uncaught exception", &detail);
        scope.terminate_execution();
    }
}
pub fn install(scope: &mut v8::PinScope<'_, '_>, options: &HostOptions) -> Result<()> {
    for name in [
        "__skyre_request_meta",
        "__skyre_cell_active",
        "__skyre_cell_id",
        "__skyre_fs",
        "__skyre_codec",
        "__skyre_form_decode",
        "__skyre_url_parse",
        "__skyre_read_image_file",
        "__skyre_image_data_url",
        "__skyre_rpc",
        "__skyre_operation_register",
        "__skyre_operation_finish",
        "__skyre_operation_derive",
        "__skyre_write",
        "__skyre_event_now",
        "__skyre_timer_schedule",
        "__skyre_timer_clear",
        "__skyre_timer_refresh",
        "__skyre_microtask",
        "__skyre_suspend_timeout",
        "__skyre_response_meta",
    ] {
        let key = v8::String::new(scope, name).unwrap();
        let function = v8::Function::builder(callback)
            .data(key.into())
            .build(scope)
            .unwrap();
        let global = scope.get_current_context().global(scope);
        global.set(scope, key.into(), function.into());
    }
    let options = options.snapshot().to_string();
    for (name, value) in [
        ("__skyre_host_options", options),
        (
            "__skyre_runtime_platform",
            if cfg!(target_os = "macos") {
                "mac"
            } else if cfg!(windows) {
                "windows"
            } else {
                "linux"
            }
            .into(),
        ),
    ] {
        let key = v8::String::new(scope, name).unwrap();
        let value = v8::String::new(scope, &value).unwrap();
        let global = scope.get_current_context().global(scope);
        global.set(scope, key.into(), value.into());
    }
    for source in [
        "delete globalThis.WebAssembly;",
        include_str!("../node_repl.js"),
        include_str!("../runtime_url_search_params.js"),
        include_str!("../runtime_modules.js"),
        include_str!("../runtime_timers.js"),
        include_str!("../browser_facade.js"),
        include_str!("../sky_facade.js"),
        include_str!("../cua_docs.js"),
        include_str!("../facade.js"),
    ] {
        v8::tc_scope!(let scope,scope);
        if eval(scope, source).is_none() {
            return Err(exception(scope));
        }
    }
    Ok(())
}
