//! Inert provider boundaries for the original conditional-restore contract and
//! the replacement's stronger expected-generation checks. No native clipboard.
use skyre::{
    Error, Result,
    clipboard::{Item, Pasteboard, Transaction},
};
use std::{cell::RefCell, rc::Rc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Entry,
    Snapshot,
    Prepared,
    Receipt,
}
#[derive(Default)]
struct State {
    count: i64,
    items: Vec<Item>,
    attempts: usize,
    writes: usize,
    expected: Vec<i64>,
    inject: Option<(usize, Stage)>,
    fail: Option<usize>,
    consumed: bool,
}
fn items(value: &str) -> Vec<Item> {
    vec![
        Item::from([
            ("public.utf8-plain-text".into(), value.as_bytes().to_vec()),
            ("org.skyre.synthetic".into(), vec![0, 255, 10]),
        ]),
        Item::new(),
    ]
}
struct Board(Rc<RefCell<State>>);
impl Board {
    fn new(inject: Option<(usize, Stage)>) -> (Self, Rc<RefCell<State>>) {
        let state = Rc::new(RefCell::new(State {
            count: 10,
            items: items("saved"),
            inject,
            ..Default::default()
        }));
        (Self(state.clone()), state)
    }
    fn boundary(&self, stage: Stage) {
        let mut state = self.0.borrow_mut();
        if state.inject == Some((state.attempts, stage)) {
            state.inject = None;
            state.count += 1;
            state.items = items("competitor");
        }
    }
}
impl Pasteboard for Board {
    fn generation(&self) -> i64 {
        self.0.borrow().count
    }
    fn snapshot(&self) -> Result<Vec<Item>> {
        Ok(self.0.borrow().items.clone())
    }
    fn install(&mut self, value: &[Item], expected: i64) -> Result<Option<i64>> {
        {
            let mut state = self.0.borrow_mut();
            state.attempts += 1;
            state.expected.push(expected);
        }
        self.boundary(Stage::Entry);
        if self.generation() != expected {
            return Ok(None);
        }
        let _backup = self.snapshot()?;
        self.boundary(Stage::Snapshot);
        let prepared = value.to_vec();
        self.boundary(Stage::Prepared);
        if self.generation() != expected {
            return Ok(None);
        }
        let receipt;
        {
            let mut state = self.0.borrow_mut();
            state.count += 1;
            receipt = state.count;
            state.items = prepared;
            state.writes += 1;
            if state.fail == Some(state.attempts) {
                return Err(Error::action("injected ambiguous write failure"));
            }
        }
        // The acquired receipt stays fixed even if another process publishes
        // before the caller receives it. The caller must not adopt that writer.
        self.boundary(Stage::Receipt);
        Ok(Some(receipt))
    }
    fn consumed(&self) -> bool {
        self.0.borrow().consumed
    }
    fn poll(&mut self, _: Duration) {}
}

#[test]
fn clipboard_begin_preserves_writer_between_snapshot_and_provider_commit() {
    for stage in [Stage::Entry, Stage::Snapshot, Stage::Prepared] {
        let (mut board, state) = Board::new(Some((1, stage)));
        assert!(Transaction::begin(&mut board, &items("temporary")).is_err());
        let state = state.borrow();
        assert_eq!(state.items, items("competitor"), "{stage:?}");
        assert_eq!(state.writes, 0);
        assert_eq!(state.attempts, 1);
        assert_eq!(state.expected, [10]);
    }
}
#[test]
fn clipboard_finish_and_drop_keep_the_original_expected_generation() {
    for stage in [Stage::Entry, Stage::Snapshot, Stage::Prepared] {
        for finish in [true, false] {
            let (mut board, state) = Board::new(Some((2, stage)));
            let tx = Transaction::begin(&mut board, &items("temporary")).unwrap();
            if finish {
                assert!(!tx.finish().unwrap(), "{stage:?}");
            } else {
                drop(tx);
            }
            let state = state.borrow();
            assert_eq!(state.items, items("competitor"), "{stage:?}");
            assert_eq!(state.expected, [10, 11]);
            assert_eq!(state.writes, 1);
            assert_eq!(state.attempts, 2, "no destructive retry");
        }
    }
}
#[test]
fn clipboard_begin_uses_the_write_receipt_instead_of_adopting_a_later_writer() {
    let (mut board, state) = Board::new(Some((1, Stage::Receipt)));
    let error = Transaction::begin(&mut board, &items("temporary"))
        .err()
        .expect("newer writer cannot become the transaction owner");
    assert!(error.message.contains("after installation"));
    let state = state.borrow();
    assert_eq!(state.items, items("competitor"));
    assert_eq!(state.count, 12);
    assert_eq!(state.attempts, 1);
}
#[test]
fn clipboard_failed_finish_never_retries_a_potentially_committed_write() {
    let (mut board, state) = Board::new(None);
    state.borrow_mut().fail = Some(2);
    let tx = Transaction::begin(&mut board, &items("temporary")).unwrap();
    assert!(tx.finish().is_err());
    assert_eq!(state.borrow().attempts, 2);
    assert_eq!(state.borrow().writes, 2);
}
#[test]
fn clipboard_timeout_cleanup_restores_owned_data_and_preserves_intervening_writer() {
    for stage in [None, Some((2, Stage::Entry))] {
        let (mut board, state) = Board::new(stage);
        let mut tx = Transaction::begin(&mut board, &items("temporary")).unwrap();
        assert!(tx.wait(Duration::ZERO).is_err());
        drop(tx);
        let state = state.borrow();
        assert_eq!(
            state.items,
            items(if stage.is_none() {
                "saved"
            } else {
                "competitor"
            })
        );
        assert_eq!(state.attempts, 2);
    }
}
#[test]
fn clipboard_success_restores_every_saved_representation_with_a_single_receipt() {
    let (mut board, state) = Board::new(None);
    let mut tx = Transaction::begin(&mut board, &items("temporary")).unwrap();
    state.borrow_mut().consumed = true;
    tx.wait(Duration::ZERO).unwrap();
    assert!(tx.finish().unwrap());
    let state = state.borrow();
    assert_eq!(state.items, items("saved"));
    assert_eq!(state.expected, [10, 11]);
    assert_eq!(state.count, 12);
    assert_eq!(state.writes, 2);
}
