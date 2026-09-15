use super::*;
pub fn compile<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    source: &str,
) -> Option<v8::Local<'s, v8::Module>> {
    let resource = v8::String::new(scope, name)?;
    let origin = v8::ScriptOrigin::new(
        scope,
        resource.into(),
        0,
        0,
        false,
        -1,
        None,
        false,
        false,
        true,
        None,
    );
    let source = v8::String::new(scope, source)?;
    v8::script_compiler::compile_module(
        scope,
        &mut v8::script_compiler::Source::new(source, Some(&origin)),
    )
}
fn source(name: &str) -> String {
    if name == "skyre:kernel" {
        return kernel::MODULE.into();
    }
    modules::builtin_sources()
        .into_iter()
        .find(|(key, _)| *key == name)
        .unwrap()
        .1
}
fn load<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Module>> {
    let state = scope.get_slot::<Rc<RefCell<State>>>().unwrap().clone();
    if let Some(module) = state.borrow().modules.get(name) {
        return Some(v8::Local::new(scope, module));
    }
    let module = compile(scope, name, &source(name))?;
    state
        .borrow_mut()
        .modules
        .insert(name.into(), v8::Global::new(scope, module));
    Some(module)
}
pub fn resolve<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    attributes: v8::Local<'s, v8::FixedArray>,
    _: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope,context);
    if attributes.length() != 0 {
        return None;
    }
    let name = modules::canonical_module(&specifier.to_rust_string_lossy(scope))?;
    load(scope, &name)
}
pub fn dynamic<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _: v8::Local<'s, v8::Data>,
    _: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let requested = specifier.to_rust_string_lossy(scope);
    if matches!(requested.as_str(), "process" | "node:process") {
        let message = v8::String::new(
            scope,
            &format!("Importing module \"{requested}\" is not allowed in node_repl"),
        )?;
        let error = v8::Exception::error(scope, message);
        resolver.reject(scope, error);
        return Some(resolver.get_promise(scope));
    }
    let Some(name) = modules::canonical_module(&requested).filter(|_| attributes.length() == 0)
    else {
        let message = v8::String::new(scope, &format!("Unsupported module: {requested}"))?;
        let error = v8::Exception::type_error(scope, message);
        resolver.reject(scope, error);
        return Some(resolver.get_promise(scope));
    };
    let module = load(scope, &name)?;
    if module.get_status() == v8::ModuleStatus::Uninstantiated
        && module.instantiate_module(scope, resolve) != Some(true)
    {
        return None;
    }
    if module.get_status() == v8::ModuleStatus::Instantiated {
        module.evaluate(scope)?;
    }
    if module.get_status() == v8::ModuleStatus::Errored {
        resolver.reject(scope, module.get_exception());
    } else {
        resolver.resolve(scope, module.get_module_namespace());
    }
    Some(resolver.get_promise(scope))
}
