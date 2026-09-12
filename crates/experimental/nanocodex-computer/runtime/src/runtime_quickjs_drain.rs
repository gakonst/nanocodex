//! Explicit native Promise tags, scoped to a live cell. No JS key or hook
//! ancestry supplies authority. Weak callback captures avoid a context cycle.
use super::{drain::Registry, tasks::Tasks};
use rquickjs::{
    Ctx, Exception, Persistent, Value,
    prelude::{Func, Rest},
};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
#[derive(Default)]
pub(super) struct Bridge {
    pub registry: RefCell<Registry>,
    tags: RefCell<HashMap<Persistent<Value<'static>>, u32>>,
}
impl Bridge {
    pub fn tag(&self, value: Value<'_>, id: u32) {
        let ctx = value.ctx().clone();
        self.tags
            .borrow_mut()
            .insert(Persistent::save(&ctx, value), id);
    }
    fn read(&self, value: Value<'_>) -> Option<u32> {
        let ctx = value.ctx().clone();
        self.tags
            .borrow()
            .get(&Persistent::save(&ctx, value))
            .copied()
            .filter(|id| self.registry.borrow().contains(*id))
    }
    pub fn collect(&self) {
        self.tags
            .borrow_mut()
            .retain(|_, id| self.registry.borrow().contains(*id));
    }
    pub fn finish(&self) {
        self.registry.borrow_mut().finish();
        self.tags.borrow_mut().clear();
    }
    pub fn install(self: &Rc<Self>, ctx: &Ctx<'_>, tasks: Rc<Tasks>) -> rquickjs::Result<()> {
        let weak = Rc::downgrade(self);
        ctx.globals().set(
            "__skyre_operation_register",
            Func::from(move |value: Value<'_>| -> rquickjs::Result<u32> {
                let ctx = value.ctx().clone();
                let bridge = weak
                    .upgrade()
                    .ok_or_else(|| Exception::throw_message(&ctx, "Native drain owner ended"))?;
                let request = bridge.read(value);
                let result = bridge
                    .registry
                    .borrow_mut()
                    .register(tasks.current.borrow().id, request);
                result.map_err(|error| Exception::throw_message(&ctx, &error.message))
            }),
        )?;
        let weak = Rc::downgrade(self);
        ctx.globals().set(
            "__skyre_operation_finish",
            Func::from(move |id: u32| {
                if let Some(bridge) = weak.upgrade() {
                    bridge.registry.borrow_mut().complete(id);
                    bridge.collect();
                }
            }),
        )?;
        let weak = Rc::downgrade(self);
        ctx.globals().set(
            "__skyre_operation_derive",
            Func::from(move |values: Rest<Value<'_>>| -> rquickjs::Result<()> {
                if values.len() == 2
                    && let Some(bridge) = weak.upgrade()
                    && let Some(id) = bridge.read(values[0].clone())
                {
                    bridge.tag(values[1].clone(), id);
                }
                Ok(())
            }),
        )?;
        Ok(())
    }
}
