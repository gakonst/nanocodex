use skyre::guardian::Guardian;
use std::{cell::Cell, rc::Rc, time::Duration};
fn fixture() -> (Rc<Cell<u64>>, Guardian) {
    let clock = Rc::new(Cell::new(0));
    let c = clock.clone();
    (
        clock,
        Guardian::with_clock(move || Duration::from_millis(c.get())),
    )
}
#[test]
fn host_guardian_exclusive_owner_token_and_expiry() {
    let (clock, mut g) = fixture();
    assert!(g.acquire("owner", 100).is_err());
    g.set_host_state(false, 1).unwrap();
    let lease = g.acquire("owner", 100).unwrap();
    assert!(g.acquire("other", 100).is_err());
    assert!(g.authorize("other", &lease.lease).is_err());
    assert!(g.authorize("owner", "forged").is_err());
    assert!(g.release("other", &lease.lease).is_err());
    g.authorize("owner", &lease.lease).unwrap();
    clock.set(99);
    g.authorize("owner", &lease.lease).unwrap();
    clock.set(100);
    assert!(g.authorize("owner", &lease.lease).is_err());
    let second = g.acquire("other", 20).unwrap();
    assert_ne!(lease.lease, second.lease);
    assert!(!g.status().to_string().contains(&second.lease));
    assert!(g.release("other", &second.lease).unwrap());
    assert!(!g.release("other", &second.lease).unwrap());
}
#[test]
fn host_guardian_host_revision_lock_and_intervention_revoke() {
    let (_, mut g) = fixture();
    g.set_host_state(false, 1).unwrap();
    let lease = g.acquire("owner", 100).unwrap();
    g.set_host_state(false, 1).unwrap();
    g.authorize("owner", &lease.lease).unwrap();
    assert!(g.set_host_state(true, 1).is_err());
    assert!(g.set_host_state(false, 0).is_err());
    g.set_host_state(true, 2).unwrap();
    assert!(g.authorize("owner", &lease.lease).is_err());
    assert!(g.acquire("owner", 100).is_err());
    g.set_host_state(false, 3).unwrap();
    let lease = g.acquire("owner", 100).unwrap();
    g.intervene(4).unwrap();
    assert!(g.authorize("owner", &lease.lease).is_err());
    assert_eq!(g.acquire("owner", 100).unwrap_err().code, -32010);
    g.set_host_state(false, 5).unwrap();
    assert!(g.acquire("owner", 100).is_err());
    g.resume_after_intervention(6).unwrap();
    let lease = g.acquire("owner", 100).unwrap();
    assert!(g.revoke_owner("owner").unwrap());
    assert!(g.authorize("owner", &lease.lease).is_err());
}
#[test]
fn host_guardian_renew_and_backwards_clock_do_not_resurrect() {
    let (clock, mut g) = fixture();
    g.set_host_state(false, 1).unwrap();
    assert!(g.acquire("owner", 0).is_err());
    assert!(g.acquire("owner", 300001).is_err());
    let lease = g.acquire("owner", 100).unwrap();
    clock.set(90);
    g.renew("owner", &lease.lease, 100).unwrap();
    clock.set(150);
    g.authorize("owner", &lease.lease).unwrap();
    clock.set(190);
    assert!(g.authorize("owner", &lease.lease).is_err());
    clock.set(0);
    assert!(g.authorize("owner", &lease.lease).is_err());
    assert!(!g.revoke_owner("other").unwrap());
}
