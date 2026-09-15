use skyre::{
    Error,
    peer::{Identity, Policy},
};
fn identity() -> Identity {
    Identity {
        team_id: "OWNTEAM".into(),
        signing_identifier: "org.skyre.own".into(),
    }
}
fn policy() -> Policy {
    Policy {
        team_ids: vec!["OWNTEAM".into()],
        signing_identifiers: vec!["org.skyre.own".into()],
    }
}
#[test]
fn all_three_generations_must_match_exactly() {
    let p = policy();
    assert_eq!(
        p.decide([Some(identity()), Some(identity()), Some(identity())])["authorized"],
        true
    );
    for depth in 0..3 {
        let mut values = [Some(identity()), Some(identity()), Some(identity())];
        values[depth]
            .as_mut()
            .unwrap()
            .signing_identifier
            .push_str(".suffix");
        assert_eq!(
            p.decide(values)["reason"],
            "untrusted-code-signing-identity"
        );
        let mut values = [Some(identity()), Some(identity()), Some(identity())];
        values[depth] = None;
        assert_eq!(p.decide(values)["reason"], "missing-code-signing-identity");
    }
}
#[test]
fn later_identity_read_error_precedes_earlier_absence() {
    let mut seen = vec![];
    let result = policy().authorize_with(|depth| {
        seen.push(depth);
        if depth == 2 {
            Err(Error::action("fixture read failure"))
        } else {
            Ok(None)
        }
    });
    assert!(result.is_err());
    assert_eq!(seen, vec![0, 1, 2]);
}
#[cfg(target_os = "macos")]
#[test]
fn real_socket_audit_token_rejects_unsigned_test_process() {
    use std::os::{fd::AsRawFd, unix::net::UnixStream};
    let (socket, _other) = UnixStream::pair().unwrap();
    match policy().authorize_socket(socket.as_raw_fd()) {
        Ok(decision) => assert_eq!(decision["authorized"], false),
        Err(error) => assert!(
            error.message.contains("signing")
                || error.message.contains("Socket")
                || error.code == -32000
        ),
    }
    assert!(policy().authorize_socket(-1).is_err());
}
