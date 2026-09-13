//! Provider-boundary and pure UIA tests. These execute no Windows OS API.
use skyre::{Error, Result, ax::Node, platforms::windows_uia_model::*};
use std::{
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
fn root() -> Root {
    Root { hwnd: 700, pid: 42 }
}
fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        root: root(),
        root_runtime: vec![42, 700],
        process: 42,
        runtime: vec![42, 700, 9],
    }
}
fn node() -> Node {
    Node {
        identity: identity().key(),
        role: "AXTextField".into(),
        title: Some("Owned fixture".into()),
        value: Some("one".into()),
        identifier: Some("owned-field".into()),
        ..Default::default()
    }
}

#[test]
fn uia_runtime_revalidation_rejects_reused_roots_foreign_processes_and_changed_semantics() {
    let original = identity();
    let before = node();
    revalidate(&original, &original, &before, &before, &Operation::Invoke).unwrap();
    for field in 0..5 {
        let mut changed = original.clone();
        match field {
            0 => changed.root.hwnd += 1,
            1 => changed.root.pid += 1,
            2 => changed.root_runtime.push(8),
            3 => changed.process += 1,
            _ => changed.runtime.push(8),
        }
        assert!(revalidate(&original, &changed, &before, &before, &Operation::Invoke).is_err());
    }
    let mut after = before.clone();
    after.value = Some("two".into());
    assert!(revalidate(&original, &original, &before, &after, &Operation::Invoke).is_err());
    revalidate(
        &original,
        &original,
        &before,
        &after,
        &Operation::SetValue("three".into()),
    )
    .unwrap();
    after.title = Some("Different control".into());
    assert!(
        revalidate(
            &original,
            &original,
            &before,
            &after,
            &Operation::SetValue("three".into())
        )
        .is_err()
    );
    let mut disabled = before.clone();
    disabled.enabled = false;
    assert!(
        revalidate(&original, &original, &before, &disabled, &Operation::Point)
            .unwrap_err()
            .message
            .contains("disabled")
    );
    let mut invalid = original;
    invalid.runtime.clear();
    assert!(invalid.validate().is_err());
}

#[test]
fn uia_range_bounds_and_native_pattern_names_are_validated_before_mutation() {
    assert_eq!(range_value("1.25", -2.0, 3.0, false).unwrap(), 1.25);
    for value in ["NaN", "inf", "four", "3.1", "-2.1"] {
        assert!(range_value(value, -2.0, 3.0, false).is_err());
    }
    assert!(range_value("1", 0.0, 2.0, true).is_err());
    assert!(range_value("1", f64::NAN, 2.0, false).is_err());
    assert!(matches!(secondary("AXPress").unwrap(), Operation::Invoke));
    assert!(matches!(
        secondary("AXScrollLeftByPage").unwrap(),
        Operation::Scroll {
            horizontal: -2,
            vertical: 0
        }
    ));
    assert!(matches!(
        secondary("AddToSelection").unwrap(),
        Operation::AddSelection
    ));
    assert!(secondary("Unchecked arbitrary action").is_err());
}

#[test]
fn uia_clickable_point_preserves_absolute_position_and_rejects_post_activation_changes() {
    let frame = [-1000., 100., 600., 400.];
    let screen = [-750., 280.];
    let point = BoundPoint::new(root(), frame, screen).unwrap();
    assert_eq!(point.validate(root(), frame, root().hwnd).unwrap(), screen);
    for changed in [
        [-999., 100., 600., 400.],
        [-1000., 101., 600., 400.],
        [-1000., 100., 601., 400.],
        [-1000., 100., 600., 401.],
        [f64::NAN, 100., 600., 400.],
    ] {
        assert!(point.validate(root(), changed, root().hwnd).is_err());
    }
    for changed in [Root { hwnd: 701, pid: 42 }, Root { hwnd: 700, pid: 43 }] {
        assert!(point.validate(changed, frame, root().hwnd).is_err());
    }
    assert!(point.validate(root(), frame, 701).is_err());
    for invalid in [
        [-1001., 280.],
        [-400., 280.],
        [-750., 500.],
        [f64::INFINITY, 280.],
    ] {
        assert!(BoundPoint::new(root(), frame, invalid).is_err());
    }
}

#[test]
fn uia_character_units_resolve_utf16_boundaries_and_reject_unrepresentable_or_changed_text() {
    let source: Vec<_> = "A🧪e\u{301}Z".encode_utf16().collect();
    let boundaries = [0, 1, 3, 5, 6];
    let resolve = |offset| {
        locate_text_endpoint(&source, offset, |count| {
            let moved = count.min(boundaries.len() - 1);
            Ok((moved, source[..boundaries[moved]].to_vec(), moved))
        })
    };
    assert_eq!(resolve(0).unwrap(), 0);
    assert_eq!(resolve(3).unwrap(), 2);
    assert_eq!(resolve(6).unwrap(), 4);
    assert!(resolve(2).unwrap_err().message.contains("surrogate"));
    assert!(
        resolve(4)
            .unwrap_err()
            .message
            .contains("not an addressable")
    );
    assert!(resolve(7).is_err());
    assert!(
        locate_text_endpoint(&source, 3, |count| Ok((count, vec![b'x' as u16], ())))
            .unwrap_err()
            .message
            .contains("changed")
    );
    assert!(locate_text_endpoint(&source, 3, |count| Ok((count + 1, vec![], ()))).is_err());
}

struct ThreadProvider {
    _not_send: Rc<()>,
    trace: Arc<Mutex<Vec<(String, thread::ThreadId)>>>,
    done: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
    mutations: Arc<AtomicUsize>,
    fail: Option<Error>,
}
impl ThreadProvider {
    fn log(&self, text: &str) {
        self.trace
            .lock()
            .unwrap()
            .push((text.into(), thread::current().id()));
    }
}
impl Provider for ThreadProvider {
    fn observe(&mut self, target: Root, ctx: &RequestContext) -> Result<Node> {
        assert_eq!(target, root());
        ctx.check()?;
        self.log("observe");
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        Ok(node())
    }
    fn perform(
        &mut self,
        target: Root,
        id: &str,
        _: Operation,
        ctx: &RequestContext,
    ) -> Result<Option<[f64; 2]>> {
        assert_eq!(target, root());
        assert_eq!(id, identity().key());
        self.log("prepare");
        if let Some(release) = self.release.take() {
            release.recv().unwrap();
        }
        ctx.check()?;
        self.mutations.fetch_add(1, Ordering::SeqCst);
        self.log("mutate");
        Ok(None)
    }
    fn clear(&mut self) -> Result<()> {
        self.log("clear");
        Ok(())
    }
}
impl Drop for ThreadProvider {
    fn drop(&mut self) {
        self.log("drop");
        let _ = self.done.send(());
    }
}

#[test]
fn uia_worker_owns_non_send_provider_through_cleanup_on_one_thread() {
    let trace = Arc::new(Mutex::new(vec![]));
    let worker_trace = trace.clone();
    let mutations = Arc::new(AtomicUsize::new(0));
    let count = mutations.clone();
    let (done, finished) = mpsc::channel();
    let mut worker = Worker::spawn(
        move || {
            worker_trace
                .lock()
                .unwrap()
                .push(("init".into(), thread::current().id()));
            Ok(Box::new(ThreadProvider {
                _not_send: Rc::new(()),
                trace: worker_trace,
                done,
                release: None,
                mutations: count,
                fail: None,
            }))
        },
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(worker.observe(root()).unwrap().title, node().title);
    worker
        .perform(root(), &identity().key(), Operation::Invoke)
        .unwrap();
    worker.clear().unwrap();
    drop(worker);
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
    let trace = trace.lock().unwrap();
    let id = trace[0].1;
    assert_ne!(id, thread::current().id());
    assert!(trace.iter().all(|(_, t)| *t == id));
    assert_eq!(
        trace.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(),
        [
            "init", "observe", "prepare", "mutate", "clear", "clear", "drop"
        ]
    );
    assert_eq!(mutations.load(Ordering::SeqCst), 1);
}

#[test]
fn uia_timeout_disables_worker_and_expired_prepared_action_never_mutates() {
    let trace = Arc::new(Mutex::new(vec![]));
    let (done, finished) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let mutations = Arc::new(AtomicUsize::new(0));
    let count = mutations.clone();
    let mut worker = Worker::spawn(
        move || {
            Ok(Box::new(ThreadProvider {
                _not_send: Rc::new(()),
                trace,
                done,
                release: Some(wait),
                mutations: count,
                fail: None,
            }))
        },
        Duration::from_millis(100),
    )
    .unwrap();
    assert_eq!(
        worker
            .perform(root(), &identity().key(), Operation::Invoke)
            .unwrap_err()
            .code,
        -32008
    );
    assert!(
        worker
            .observe(root())
            .unwrap_err()
            .message
            .contains("unavailable")
    );
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
}

#[test]
fn uia_initialization_and_provider_failures_are_preserved_not_empty_successes() {
    let failed = Worker::spawn(
        || Err(Error::new(-32003, "owned initialization failure")),
        Duration::from_secs(2),
    );
    assert_eq!(failed.err().unwrap().code, -32003);
    let trace = Arc::new(Mutex::new(vec![]));
    let (done, finished) = mpsc::channel();
    let mut worker = Worker::spawn(
        move || {
            Ok(Box::new(ThreadProvider {
                _not_send: Rc::new(()),
                trace,
                done,
                release: None,
                mutations: Arc::new(AtomicUsize::new(0)),
                fail: Some(Error::new(-32123, "owned provider read failure")),
            }))
        },
        Duration::from_secs(2),
    )
    .unwrap();
    let error = worker.observe(root()).unwrap_err();
    assert_eq!(error.code, -32123);
    assert_eq!(error.message, "owned provider read failure");
    assert_eq!(worker.observe(root()).unwrap_err().code, -32123);
    drop(worker);
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[test]
fn uia_provider_returned_timeout_preserves_error_and_disables_worker_before_reuse() {
    let trace = Arc::new(Mutex::new(vec![]));
    let observed_trace = trace.clone();
    let (done, finished) = mpsc::channel();
    let mut worker = Worker::spawn(
        move || {
            Ok(Box::new(ThreadProvider {
                _not_send: Rc::new(()),
                trace,
                done,
                release: None,
                mutations: Arc::new(AtomicUsize::new(0)),
                // Deterministically exercise the received-error path without
                // racing the host timer. RequestContext expiry uses this code.
                fail: Some(Error::new(-32008, "owned provider deadline expired")),
            }))
        },
        Duration::from_secs(2),
    )
    .unwrap();
    let error = worker.observe(root()).unwrap_err();
    assert_eq!(error.code, -32008);
    assert_eq!(error.message, "owned provider deadline expired");
    assert!(
        worker
            .observe(root())
            .unwrap_err()
            .message
            .contains("unavailable")
    );
    assert!(
        worker
            .perform(root(), &identity().key(), Operation::Invoke)
            .unwrap_err()
            .message
            .contains("unavailable")
    );
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        observed_trace
            .lock()
            .unwrap()
            .iter()
            .filter(|(event, _)| event == "observe")
            .count(),
        1
    );
}
