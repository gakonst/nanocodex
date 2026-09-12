//! Host-owned asynchronous execution identity, separate from the active request.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
#[derive(Clone)]
pub(super) struct Context {
    pub id: u64,
    pub metadata: Rc<str>,
}
impl Default for Context {
    fn default() -> Self {
        Self {
            id: 0,
            metadata: Rc::from("null"),
        }
    }
}
#[derive(Default)]
pub(super) struct Tasks {
    pub current: RefCell<Context>,
    stack: RefCell<Vec<Context>>,
    pub poisoned: Cell<bool>,
    pub failure: RefCell<Option<String>>,
}
impl Tasks {
    pub fn fail(&self, kind: &str, detail: &str) {
        self.poisoned.set(true);
        self.failure
            .borrow_mut()
            .get_or_insert_with(|| fatal(kind, detail));
    }
    pub fn scope(&self, context: Context) -> Scope<'_> {
        let depth = self.stack.borrow().len();
        self.enter(context);
        Scope {
            tasks: self,
            entered: self.stack.borrow().len() > depth,
        }
    }
    pub fn enter(&self, context: Context) {
        if self.stack.borrow().len() >= 4096 {
            self.poisoned.set(true);
            return;
        }
        self.stack.borrow_mut().push(self.current.replace(context));
    }
    pub fn leave(&self) {
        if let Some(previous) = self.stack.borrow_mut().pop() {
            self.current.replace(previous);
        }
    }
}

/// Installed kernel fatal async error envelope (worker-runtime source pinned).
pub(super) fn fatal(kind: &str, detail: &str) -> String {
    format!(
        "node_repl kernel {kind}: {detail}; kernel reset. Catch or handle async errors (including Promise rejections and EventEmitter 'error' events) to avoid kernel termination."
    )
}

pub(super) fn timeout_result() -> serde_json::Value {
    let message = "js execution timed out; kernel reset, rerun your request";
    serde_json::json!({"error":{"code":-32004,"message":message},"exceptionMessage":message,"outputs":[],"responseMeta":{}})
}

/// Balance native dispatch context even when error propagation exits early.
pub(super) struct Scope<'a> {
    tasks: &'a Tasks,
    entered: bool,
}
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if self.entered {
            self.tasks.leave();
        }
    }
}
