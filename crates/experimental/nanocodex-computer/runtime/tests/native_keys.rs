use skyre::native::keys::{
    EventFactory, EventKind, EventSpec, KeyPress, MAX_CHORDS, MAX_KEY_BYTES, MAX_TOKENS,
    ParseError, parse, prepare_events,
};
use skyre::{Error, Result};
use std::{cell::Cell, rc::Rc};

// Static factual decoder table, not an execution of the original service.
// Recorded keysym aliases and Carbon flags were recovered from the installed
// binary and independently checked against raw jump-table and instruction bytes.
#[test]
fn all_210_recovered_native_keysyms_match_their_exact_lowering() {
    let rows: serde_json::Value =
        serde_json::from_str(include_str!("fragments/native-keysym-lowering.json")).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 210);
    let mut counts = [0; 3];
    for row in rows {
        let name = row["name"].as_str().unwrap();
        if !row["supported"].as_bool().unwrap() {
            assert_eq!(
                parse(name),
                Err(ParseError::Unsupported(vec![name.into()])),
                "{name}"
            );
            counts[0] += 1;
            continue;
        }
        let mut flags = 0;
        for carbon in row["carbon_modifiers"].as_array().unwrap() {
            flags |= match carbon.as_u64().unwrap() {
                256 => 1 << 20,
                512 => 1 << 17,
                2048 => 1 << 19,
                4096 => 1 << 18,
                other => panic!("unreviewed Carbon flag: {other}"),
            };
        }
        if row["modifier_only"].as_bool().unwrap() {
            counts[1] += 1;
            assert_eq!(
                parse(name),
                Err(ParseError::NoKey(vec![name.into()])),
                "{name}"
            );
            assert_eq!(
                parse(&format!("{name}+a")),
                Ok(vec![KeyPress { code: 0, flags }]),
                "{name}"
            );
        } else {
            counts[2] += 1;
            assert_eq!(
                parse(name),
                Ok(vec![KeyPress {
                    code: row["key_code"].as_u64().unwrap() as u16,
                    flags
                }]),
                "{name}"
            );
        }
    }
    assert_eq!(counts, [37, 18, 155]);
}

#[test]
fn literal_space_and_plus_grammar_preserves_native_quirks() {
    for text in ["a", "++a", "a++", " a ", "  ++a  "] {
        assert_eq!(parse(text), Ok(vec![KeyPress { code: 0, flags: 0 }]));
    }
    assert_eq!(
        parse("a b"),
        Ok(vec![
            KeyPress { code: 0, flags: 0 },
            KeyPress { code: 11, flags: 0 }
        ])
    );
    assert_eq!(parse(""), Ok(vec![]));
    assert_eq!(parse("   "), Ok(vec![]));
    assert_eq!(parse("+"), Err(ParseError::NoKey(vec![])));
    assert_eq!(
        parse("Control_L + a"),
        Err(ParseError::NoKey(vec!["Control_L".into()]))
    );
    for unknown in [
        "a\tb", "a\nb", "a\rb", "a\u{a0}b", "Control", "Ctrl", "CTRL", "!", "Ａ",
    ] {
        assert_eq!(parse(unknown), Err(ParseError::KeyNotFound(unknown.into())));
    }
}

#[test]
fn separator_literals_follow_original_swift_grapheme_boundaries() {
    let rows: serde_json::Value =
        serde_json::from_str(include_str!("fragments/native-key-unicode-split.json")).unwrap();
    for row in rows.as_array().unwrap() {
        let expected_unknown = row["chords"][0][0].as_str().unwrap();
        assert_eq!(
            parse(row["input"].as_str().unwrap()),
            Err(ParseError::KeyNotFound(expected_unknown.into()))
        );
    }
}

#[test]
fn canonical_lookup_matches_swift_without_compatibility_folding_or_error_rewriting() {
    let rows: serde_json::Value =
        serde_json::from_str(include_str!("fragments/native-key-canonical.json")).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 254);
    for row in rows.as_array().unwrap() {
        let input = row["input"].as_str().unwrap();
        match row["lowered"].as_str() {
            Some(canonical) => assert_eq!(parse(input), parse(canonical), "{input}"),
            None => assert_eq!(parse(input), Err(ParseError::KeyNotFound(input.into()))),
        }
    }
    assert_eq!(parse("Super_L+K"), parse("Super_L+K"));
    assert_eq!(parse("ctrl+KP_Add"), parse("ctrl+KP_Add"));
    let multiple: Error = parse("K+a").unwrap_err().into();
    assert_eq!(
        multiple.message,
        "keyPressIncludedMultipleNonModifierKeys(ComputerUse.XKeysymString.KeyPress(keys: [ComputerUse.XKeysym.K, ComputerUse.XKeysym.a]))"
    );
    let unsupported: Error = parse("K+KP_F1").unwrap_err().into();
    assert_eq!(
        unsupported.message,
        "keyPressNotSupportedByMacOS(ComputerUse.XKeysymString.KeyPress(keys: [ComputerUse.XKeysym.K, ComputerUse.XKeysym.KP_F1]))"
    );
    let unknown: Error = parse("a+b Kbad").unwrap_err().into();
    assert_eq!(unknown.message, "keyNotFound(\"Kbad\")");
}

#[test]
fn complete_lexical_pass_and_ordered_chord_failures_match_native() {
    assert_eq!(
        parse("a+b Unknown"),
        Err(ParseError::KeyNotFound("Unknown".into()))
    );
    assert_eq!(
        parse("Pause+a Unknown"),
        Err(ParseError::KeyNotFound("Unknown".into()))
    );
    assert_eq!(
        parse("a+Pause"),
        Err(ParseError::Unsupported(vec!["a".into(), "Pause".into()]))
    );
    assert_eq!(
        parse("a+Control_L"),
        Err(ParseError::MultipleKeys(vec![
            "a".into(),
            "Control_L".into()
        ]))
    );
    assert_eq!(
        parse("a+b+Pause"),
        Err(ParseError::MultipleKeys(vec![
            "a".into(),
            "b".into(),
            "Pause".into()
        ]))
    );
    assert_eq!(
        parse("Shift_L+Control_L"),
        Err(ParseError::NoKey(vec![
            "Shift_L".into(),
            "Control_L".into()
        ]))
    );
}

#[test]
fn shifted_characters_right_modifiers_and_plus_are_distinct() {
    assert_eq!(
        parse("Control_R+A"),
        Ok(vec![KeyPress {
            code: 0,
            flags: (1 << 18) | (1 << 17)
        }])
    );
    assert_eq!(
        parse("Super_R+plus"),
        Ok(vec![KeyPress {
            code: 24,
            flags: (1 << 20) | (1 << 17)
        }])
    );
    // The native table itself maps both names to the shifted ANSI equals key.
    assert_eq!(parse("equal"), parse("plus"));
    assert_eq!(parse("KP_Add"), Ok(vec![KeyPress { code: 69, flags: 0 }]));
    assert_eq!(
        parse("Caps_Lock"),
        Ok(vec![KeyPress { code: 57, flags: 0 }])
    );
    assert_eq!(parse("Shift_L+Shift_R+a"), parse("Shift_L+a"));
}

#[test]
fn parser_error_payloads_match_owned_swift_reflection_oracle() {
    let rows: serde_json::Value =
        serde_json::from_str(include_str!("fragments/native-key-reflection.json")).unwrap();
    for row in rows.as_array().unwrap() {
        let error: Error = if let Some(token) = row["token"].as_str() {
            ParseError::KeyNotFound(token.into()).into()
        } else {
            parse(row["chord"].as_str().unwrap()).unwrap_err().into()
        };
        assert_eq!(error.code, -10005);
        assert_eq!(error.message, row["message"].as_str().unwrap(), "{row}");
    }
}

#[derive(Debug)]
struct OwnedEvent {
    spec: EventSpec,
    live: Rc<Cell<usize>>,
}
impl Drop for OwnedEvent {
    fn drop(&mut self) {
        self.live.set(self.live.get() - 1);
    }
}
#[derive(Debug, PartialEq)]
enum Call {
    Make(EventSpec),
    Sample,
}
struct Factory {
    calls: Vec<Call>,
    live: Rc<Cell<usize>>,
    fail_at: Option<usize>,
    samples: usize,
}
impl Factory {
    fn new(fail_at: Option<usize>) -> Self {
        Self {
            calls: vec![],
            live: Rc::new(Cell::new(0)),
            fail_at,
            samples: 0,
        }
    }
    fn check(&self) -> Result<()> {
        if self.fail_at == Some(self.calls.len() - 1) {
            Err(Error::action("injected construction failure"))
        } else {
            Ok(())
        }
    }
}
impl EventFactory for Factory {
    type Event = OwnedEvent;
    fn make(&mut self, spec: EventSpec) -> Result<OwnedEvent> {
        self.calls.push(Call::Make(spec));
        self.check()?;
        self.live.set(self.live.get() + 1);
        Ok(OwnedEvent {
            spec,
            live: self.live.clone(),
        })
    }
    fn saved_flags(&mut self) -> Result<u64> {
        self.calls.push(Call::Sample);
        self.check()?;
        self.samples += 1;
        Ok(0x80000000 + self.samples as u64)
    }
}

#[test]
fn event_construction_samples_each_chord_and_publishes_recovered_order() {
    let keys = parse("Control_R+a Super_R+B").unwrap();
    let mut factory = Factory::new(None);
    let events = prepare_events(&keys, &mut factory).unwrap();
    assert_eq!(events.len(), 8);
    for (index, key) in keys.iter().enumerate() {
        let target = EventSpec {
            kind: EventKind::FlagsChanged,
            code: None,
            flags: key.flags,
        };
        let down = EventSpec {
            kind: EventKind::KeyDown,
            code: Some(key.code),
            flags: key.flags,
        };
        let up = EventSpec {
            kind: EventKind::KeyUp,
            code: Some(key.code),
            flags: key.flags,
        };
        let restore = EventSpec {
            kind: EventKind::FlagsChanged,
            code: None,
            flags: 0x80000001 + index as u64,
        };
        assert_eq!(
            factory.calls[index * 5..index * 5 + 5],
            [
                Call::Make(target),
                Call::Make(down),
                Call::Make(up),
                Call::Sample,
                Call::Make(restore)
            ]
        );
        assert_eq!(
            events[index * 4..index * 4 + 4]
                .iter()
                .map(|event| event.spec)
                .collect::<Vec<_>>(),
            [target, down, restore, up]
        );
    }
    assert_eq!(factory.live.get(), 8);
    drop(events);
    assert_eq!(factory.live.get(), 0);
}

#[test]
fn construction_failure_drops_every_partial_event_without_publication() {
    let keys = parse("ctrl+a alt+b").unwrap();
    for index in 0..10 {
        let mut factory = Factory::new(Some(index));
        assert_eq!(
            prepare_events(&keys, &mut factory).unwrap_err().message,
            "injected construction failure"
        );
        assert_eq!(factory.calls.len(), index + 1);
        assert_eq!(factory.live.get(), 0, "allocation/sample failure {index}");
    }
}

#[test]
fn replacement_resource_bounds_apply_before_native_event_allocation() {
    assert!(matches!(
        parse(&"a".repeat(MAX_KEY_BYTES + 1)),
        Err(ParseError::Limit(_))
    ));
    assert_eq!(
        parse(&vec!["a"; MAX_CHORDS].join(" ")).unwrap().len(),
        MAX_CHORDS
    );
    assert!(matches!(
        parse(&vec!["a"; MAX_CHORDS + 1].join(" ")),
        Err(ParseError::Limit(_))
    ));
    // 4096 one-character tokens fit in the separate byte limit.
    assert!(matches!(
        parse(&vec!["a"; MAX_TOKENS + 1].join("+")),
        Err(ParseError::Limit(_))
    ));
    let mut factory = Factory::new(None);
    assert!(
        prepare_events(
            &vec![KeyPress { code: 0, flags: 0 }; MAX_CHORDS + 1],
            &mut factory
        )
        .is_err()
    );
    assert!(factory.calls.is_empty());
}
