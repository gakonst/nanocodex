//! Native XKeysym grammar recovered from the installed macOS provider.
//! Literal names, key codes and event layout are facts from the sealed decoder
//! audit; this implementation does not load or execute original provider code.
use crate::{Error, Result};

pub const MAX_KEY_BYTES: usize = 16 * 1024;
pub const MAX_CHORDS: usize = 256;
pub const MAX_TOKENS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub code: u16,
    pub flags: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Atom {
    Modifier(u64),
    Key(KeyPress),
    Unsupported,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    KeyNotFound(String),
    Unsupported(Vec<String>),
    MultipleKeys(Vec<String>),
    NoKey(Vec<String>),
    Limit(&'static str),
}

impl From<ParseError> for Error {
    fn from(error: ParseError) -> Self {
        fn chord(case: &str, names: Vec<String>) -> String {
            let values = names
                .iter()
                .map(|name| {
                    let reflected = match name.as_str() {
                        "0" => "zero",
                        "1" => "one",
                        "2" => "two",
                        "3" => "three",
                        "4" => "four",
                        "5" => "five",
                        "6" => "six",
                        "7" => "seven",
                        "8" => "eight",
                        "9" => "nine",
                        other => other,
                    };
                    format!("ComputerUse.XKeysym.{reflected}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{case}(ComputerUse.XKeysymString.KeyPress(keys: [{values}]))")
        }
        let message = match error {
            ParseError::KeyNotFound(token) => format!("keyNotFound({})", debug_string(&token)),
            ParseError::Unsupported(names) => chord("keyPressNotSupportedByMacOS", names),
            ParseError::MultipleKeys(names) => {
                chord("keyPressIncludedMultipleNonModifierKeys", names)
            }
            ParseError::NoKey(names) => chord("keyPressIncludedNoNonModifierKeys", names),
            ParseError::Limit(message) => return Self::invalid(message),
        };
        // Native IPC mapper 100181d9c returns unknownError with
        // String(describing: error); socket handler serializes code -10005.
        Self::action(message)
    }
}

/// Swift reflection escapes ASCII syntax/control scalars and non-ASCII scalars
/// that would join generated quoting syntax into a grapheme. Non-ASCII controls
/// are otherwise preserved, including C1 controls and line separators.
fn debug_string(input: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    fn escaped(c: char, force: bool) -> Option<String> {
        Some(match c {
            '\\' => "\\\\".into(),
            '\'' => "\\'".into(),
            '"' => "\\\"".into(),
            '\0' => "\\0".into(),
            '\n' => "\\n".into(),
            '\r' => "\\r".into(),
            '\t' => "\\t".into(),
            c if c.is_ascii_control() => format!("\\u{{{:02X}}}", c as u32),
            c if force && !c.is_ascii() => {
                if (c as u32) <= 0xffff {
                    format!("\\u{{{:04X}}}", c as u32)
                } else {
                    format!("\\u{{{:08X}}}", c as u32)
                }
            }
            _ => return None,
        })
    }
    let mut result = String::from("\"");
    let mut generated_tail = true;
    for scalar in input.chars() {
        let forced = generated_tail
            && format!("{}{scalar}", result.chars().next_back().unwrap())
                .graphemes(true)
                .count()
                == 1;
        if let Some(value) = escaped(scalar, forced) {
            result.push_str(&value);
            generated_tail = true;
        } else {
            result.push(scalar);
            generated_tail = false;
        }
    }
    let mut suffix = String::from("\"");
    loop {
        let first = suffix.chars().next().unwrap();
        result.push(first);
        if result.graphemes(true).next_back().unwrap().len() == first.len_utf8() {
            result.push_str(&suffix[first.len_utf8()..]);
            return result;
        }
        result.pop();
        let last = result.pop().unwrap();
        let quoted = escaped(last, true).unwrap_or_else(|| last.to_string());
        suffix.insert_str(0, &quoted);
    }
}

// The captured overload is RegexBuilder's String separator overload. Its
// literals match whole graphemes: a '+' joined to a combining scalar is part
// of the token, not a separator. A byte-level str::split would lose that fact.
fn parts<'a>(input: &'a str, separator: &'static str) -> impl Iterator<Item = &'a str> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut start = 0;
    input
        .grapheme_indices(true)
        .chain(std::iter::once((input.len(), separator)))
        .filter_map(move |(index, grapheme)| {
            if grapheme != separator {
                return None;
            }
            let value = &input[start..index];
            start = index + separator.len();
            (!value.is_empty()).then_some(value)
        })
}

pub fn parse(input: &str) -> std::result::Result<Vec<KeyPress>, ParseError> {
    if input.len() > MAX_KEY_BYTES {
        return Err(ParseError::Limit("Key input exceeds 16384 bytes"));
    }
    let mut chords = Vec::new();
    let mut tokens = 0;
    // The original performs a complete lexical pass before lowering any chord.
    for group in parts(input, " ") {
        if chords.len() == MAX_CHORDS {
            return Err(ParseError::Limit("Key sequence exceeds 256 chords"));
        }
        let mut chord = Vec::new();
        for token in parts(group, "+") {
            tokens += 1;
            if tokens > MAX_TOKENS {
                return Err(ParseError::Limit("Key sequence exceeds 4096 tokens"));
            }
            // Swift's cached string switch uses canonical equality. U+212A is
            // the only non-ASCII canonical decomposition into this table's
            // alphabet (Unicode 17 scan, independently confirmed by Swift).
            // Keep failed tokens verbatim, but retain actual enum spellings
            // for errors after successful lookup. No compatibility folding.
            let canonical = if token.contains('\u{212a}') {
                std::borrow::Cow::Owned(token.replace('\u{212a}', "K"))
            } else {
                std::borrow::Cow::Borrowed(token)
            };
            let atom = lookup(&canonical).ok_or_else(|| ParseError::KeyNotFound(token.into()))?;
            chord.push((canonical, atom));
        }
        chords.push(chord);
    }
    let mut output = Vec::with_capacity(chords.len());
    for chord in chords {
        let original = || chord.iter().map(|(name, _)| name.to_string()).collect();
        let mut previous_modifier = true;
        let mut flags = 0;
        let mut key = None;
        for (_, atom) in &chord {
            match atom {
                Atom::Unsupported => return Err(ParseError::Unsupported(original())),
                Atom::Modifier(value) => flags |= value,
                Atom::Key(value) => flags |= value.flags,
            }
            if !previous_modifier {
                return Err(ParseError::MultipleKeys(original()));
            }
            previous_modifier = matches!(atom, Atom::Modifier(_));
            if let Atom::Key(value) = atom {
                key = Some(value.code);
            }
        }
        if previous_modifier {
            return Err(ParseError::NoKey(original()));
        }
        output.push(KeyPress {
            code: key.ok_or_else(|| ParseError::NoKey(original()))?,
            flags,
        });
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    FlagsChanged,
    KeyDown,
    KeyUp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventSpec {
    pub kind: EventKind,
    pub code: Option<u16>,
    pub flags: u64,
}
/// One factory owns one event source for the whole sequence. Allocation and
/// saved-flags sampling are injectable; this trait never posts an event.
pub trait EventFactory {
    type Event;
    fn make(&mut self, event: EventSpec) -> Result<Self::Event>;
    fn saved_flags(&mut self) -> Result<u64>;
}
pub fn prepare_events<F: EventFactory>(
    keys: &[KeyPress],
    factory: &mut F,
) -> Result<Vec<F::Event>> {
    if keys.len() > MAX_CHORDS {
        return Err(Error::invalid("Key sequence exceeds 256 chords"));
    }
    let mut output = Vec::with_capacity(keys.len() * 4);
    for key in keys {
        let event = |kind, flags| EventSpec {
            kind,
            code: if kind == EventKind::FlagsChanged {
                None
            } else {
                Some(key.code)
            },
            flags,
        };
        let target = factory.make(event(EventKind::FlagsChanged, key.flags))?;
        let down = factory.make(event(EventKind::KeyDown, key.flags))?;
        let up = factory.make(event(EventKind::KeyUp, key.flags))?;
        let saved = factory.saved_flags()?;
        let restore = factory.make(event(EventKind::FlagsChanged, saved))?;
        // The original returns ([target, down, restore], [up]) and concatenates
        // the second array after the first. Allocation order differs from order
        // of publication. Return nothing if any later allocation fails.
        output.extend([target, down, restore, up]);
    }
    Ok(output)
}
fn lookup(token: &str) -> Option<Atom> {
    Some(match token {
        "BackSpace" => Atom::Key(KeyPress { code: 51, flags: 0 }),
        "Tab" => Atom::Key(KeyPress { code: 48, flags: 0 }),
        "Linefeed" | "Return" => Atom::Key(KeyPress { code: 36, flags: 0 }),
        "Clear" | "KP_Delete" => Atom::Key(KeyPress { code: 71, flags: 0 }),
        "Escape" => Atom::Key(KeyPress { code: 53, flags: 0 }),
        "Delete" => Atom::Key(KeyPress {
            code: 117,
            flags: 0,
        }),
        "Pause" | "Scroll_Lock" | "Sys_Req" | "Back" | "Select" | "Print" | "Execute"
        | "Insert" | "Undo" | "Redo" | "Find" | "Cancel" | "Break" | "Mode_switch"
        | "script_switch" | "Num_Lock" | "KP_Space" | "KP_Tab" | "KP_F1" | "KP_F2" | "KP_F3"
        | "KP_F4" | "KP_Home" | "KP_Left" | "KP_Up" | "KP_Right" | "KP_Down" | "KP_Prior"
        | "KP_Page_Up" | "KP_Next" | "KP_Page_Down" | "KP_End" | "KP_Begin" | "KP_Insert"
        | "KP_Separator" | "Hyper_L" | "Hyper_R" => Atom::Unsupported,
        "Home" | "Begin" => Atom::Key(KeyPress {
            code: 115,
            flags: 0,
        }),
        "Left" => Atom::Key(KeyPress {
            code: 123,
            flags: 0,
        }),
        "Up" => Atom::Key(KeyPress {
            code: 126,
            flags: 0,
        }),
        "Right" => Atom::Key(KeyPress {
            code: 124,
            flags: 0,
        }),
        "Down" => Atom::Key(KeyPress {
            code: 125,
            flags: 0,
        }),
        "Prior" | "Page_Up" => Atom::Key(KeyPress {
            code: 116,
            flags: 0,
        }),
        "Next" | "Page_Down" => Atom::Key(KeyPress {
            code: 121,
            flags: 0,
        }),
        "End" => Atom::Key(KeyPress {
            code: 119,
            flags: 0,
        }),
        "Menu" => Atom::Key(KeyPress {
            code: 110,
            flags: 0,
        }),
        "Help" => Atom::Key(KeyPress {
            code: 114,
            flags: 0,
        }),
        "KP_Enter" => Atom::Key(KeyPress { code: 76, flags: 0 }),
        "KP_Equal" => Atom::Key(KeyPress { code: 81, flags: 0 }),
        "KP_Multiply" => Atom::Key(KeyPress { code: 67, flags: 0 }),
        "KP_Add" => Atom::Key(KeyPress { code: 69, flags: 0 }),
        "KP_Subtract" => Atom::Key(KeyPress { code: 78, flags: 0 }),
        "KP_Decimal" => Atom::Key(KeyPress { code: 65, flags: 0 }),
        "KP_Divide" => Atom::Key(KeyPress { code: 75, flags: 0 }),
        "KP_0" => Atom::Key(KeyPress { code: 82, flags: 0 }),
        "KP_1" => Atom::Key(KeyPress { code: 83, flags: 0 }),
        "KP_2" => Atom::Key(KeyPress { code: 84, flags: 0 }),
        "KP_3" => Atom::Key(KeyPress { code: 85, flags: 0 }),
        "KP_4" => Atom::Key(KeyPress { code: 86, flags: 0 }),
        "KP_5" => Atom::Key(KeyPress { code: 87, flags: 0 }),
        "KP_6" => Atom::Key(KeyPress { code: 88, flags: 0 }),
        "KP_7" => Atom::Key(KeyPress { code: 89, flags: 0 }),
        "KP_8" => Atom::Key(KeyPress { code: 91, flags: 0 }),
        "KP_9" => Atom::Key(KeyPress { code: 92, flags: 0 }),
        "F1" => Atom::Key(KeyPress {
            code: 122,
            flags: 0,
        }),
        "F2" => Atom::Key(KeyPress {
            code: 120,
            flags: 0,
        }),
        "F3" => Atom::Key(KeyPress { code: 99, flags: 0 }),
        "F4" => Atom::Key(KeyPress {
            code: 118,
            flags: 0,
        }),
        "F5" => Atom::Key(KeyPress { code: 96, flags: 0 }),
        "F6" => Atom::Key(KeyPress { code: 97, flags: 0 }),
        "F7" => Atom::Key(KeyPress { code: 98, flags: 0 }),
        "F8" => Atom::Key(KeyPress {
            code: 100,
            flags: 0,
        }),
        "F9" => Atom::Key(KeyPress {
            code: 101,
            flags: 0,
        }),
        "F10" => Atom::Key(KeyPress {
            code: 109,
            flags: 0,
        }),
        "F11" => Atom::Key(KeyPress {
            code: 103,
            flags: 0,
        }),
        "F12" => Atom::Key(KeyPress {
            code: 111,
            flags: 0,
        }),
        "F13" => Atom::Key(KeyPress {
            code: 105,
            flags: 0,
        }),
        "F14" => Atom::Key(KeyPress {
            code: 107,
            flags: 0,
        }),
        "F15" => Atom::Key(KeyPress {
            code: 113,
            flags: 0,
        }),
        "F16" => Atom::Key(KeyPress {
            code: 106,
            flags: 0,
        }),
        "F17" => Atom::Key(KeyPress { code: 64, flags: 0 }),
        "F18" => Atom::Key(KeyPress { code: 79, flags: 0 }),
        "F19" => Atom::Key(KeyPress { code: 80, flags: 0 }),
        "F20" => Atom::Key(KeyPress { code: 90, flags: 0 }),
        "Shift_L" | "Shift_R" | "shift" => Atom::Modifier(131072),
        "Control_L" | "Control_R" | "ctrl" => Atom::Modifier(262144),
        "Meta_L" | "Meta_R" | "Super_L" | "Super_R" | "super" | "meta" | "command" | "Command"
        | "cmd" => Atom::Modifier(1048576),
        "Alt_L" | "Alt_R" | "alt" => Atom::Modifier(524288),
        "Caps_Lock" | "Shift_Lock" => Atom::Key(KeyPress { code: 57, flags: 0 }),
        "space" => Atom::Key(KeyPress { code: 49, flags: 0 }),
        "exclam" => Atom::Key(KeyPress {
            code: 18,
            flags: 131072,
        }),
        "quotedbl" => Atom::Key(KeyPress {
            code: 39,
            flags: 131072,
        }),
        "numbersign" => Atom::Key(KeyPress {
            code: 20,
            flags: 131072,
        }),
        "dollar" => Atom::Key(KeyPress {
            code: 21,
            flags: 131072,
        }),
        "percent" => Atom::Key(KeyPress {
            code: 23,
            flags: 131072,
        }),
        "ampersand" => Atom::Key(KeyPress {
            code: 26,
            flags: 131072,
        }),
        "apostrophe" => Atom::Key(KeyPress { code: 39, flags: 0 }),
        "parenleft" => Atom::Key(KeyPress {
            code: 25,
            flags: 131072,
        }),
        "parenright" => Atom::Key(KeyPress {
            code: 29,
            flags: 131072,
        }),
        "asterisk" => Atom::Key(KeyPress {
            code: 28,
            flags: 131072,
        }),
        "plus" | "equal" => Atom::Key(KeyPress {
            code: 24,
            flags: 131072,
        }),
        "comma" => Atom::Key(KeyPress { code: 43, flags: 0 }),
        "minus" => Atom::Key(KeyPress { code: 27, flags: 0 }),
        "period" => Atom::Key(KeyPress { code: 47, flags: 0 }),
        "slash" => Atom::Key(KeyPress { code: 44, flags: 0 }),
        "0" => Atom::Key(KeyPress { code: 29, flags: 0 }),
        "1" => Atom::Key(KeyPress { code: 18, flags: 0 }),
        "2" => Atom::Key(KeyPress { code: 19, flags: 0 }),
        "3" => Atom::Key(KeyPress { code: 20, flags: 0 }),
        "4" => Atom::Key(KeyPress { code: 21, flags: 0 }),
        "5" => Atom::Key(KeyPress { code: 23, flags: 0 }),
        "6" => Atom::Key(KeyPress { code: 22, flags: 0 }),
        "7" => Atom::Key(KeyPress { code: 26, flags: 0 }),
        "8" => Atom::Key(KeyPress { code: 28, flags: 0 }),
        "9" => Atom::Key(KeyPress { code: 25, flags: 0 }),
        "colon" => Atom::Key(KeyPress {
            code: 41,
            flags: 131072,
        }),
        "semicolon" => Atom::Key(KeyPress { code: 41, flags: 0 }),
        "less" => Atom::Key(KeyPress {
            code: 43,
            flags: 131072,
        }),
        "greater" => Atom::Key(KeyPress {
            code: 47,
            flags: 131072,
        }),
        "question" => Atom::Key(KeyPress {
            code: 44,
            flags: 131072,
        }),
        "at" => Atom::Key(KeyPress {
            code: 19,
            flags: 131072,
        }),
        "A" => Atom::Key(KeyPress {
            code: 0,
            flags: 131072,
        }),
        "B" => Atom::Key(KeyPress {
            code: 11,
            flags: 131072,
        }),
        "C" => Atom::Key(KeyPress {
            code: 8,
            flags: 131072,
        }),
        "D" => Atom::Key(KeyPress {
            code: 2,
            flags: 131072,
        }),
        "E" => Atom::Key(KeyPress {
            code: 14,
            flags: 131072,
        }),
        "F" => Atom::Key(KeyPress {
            code: 3,
            flags: 131072,
        }),
        "G" => Atom::Key(KeyPress {
            code: 5,
            flags: 131072,
        }),
        "H" => Atom::Key(KeyPress {
            code: 4,
            flags: 131072,
        }),
        "I" => Atom::Key(KeyPress {
            code: 34,
            flags: 131072,
        }),
        "J" => Atom::Key(KeyPress {
            code: 38,
            flags: 131072,
        }),
        "K" => Atom::Key(KeyPress {
            code: 40,
            flags: 131072,
        }),
        "L" => Atom::Key(KeyPress {
            code: 37,
            flags: 131072,
        }),
        "M" => Atom::Key(KeyPress {
            code: 46,
            flags: 131072,
        }),
        "N" => Atom::Key(KeyPress {
            code: 45,
            flags: 131072,
        }),
        "O" => Atom::Key(KeyPress {
            code: 31,
            flags: 131072,
        }),
        "P" => Atom::Key(KeyPress {
            code: 35,
            flags: 131072,
        }),
        "Q" => Atom::Key(KeyPress {
            code: 12,
            flags: 131072,
        }),
        "R" => Atom::Key(KeyPress {
            code: 15,
            flags: 131072,
        }),
        "S" => Atom::Key(KeyPress {
            code: 1,
            flags: 131072,
        }),
        "T" => Atom::Key(KeyPress {
            code: 17,
            flags: 131072,
        }),
        "U" => Atom::Key(KeyPress {
            code: 32,
            flags: 131072,
        }),
        "V" => Atom::Key(KeyPress {
            code: 9,
            flags: 131072,
        }),
        "W" => Atom::Key(KeyPress {
            code: 13,
            flags: 131072,
        }),
        "X" => Atom::Key(KeyPress {
            code: 7,
            flags: 131072,
        }),
        "Y" => Atom::Key(KeyPress {
            code: 16,
            flags: 131072,
        }),
        "Z" => Atom::Key(KeyPress {
            code: 6,
            flags: 131072,
        }),
        "bracketleft" => Atom::Key(KeyPress { code: 33, flags: 0 }),
        "backslash" => Atom::Key(KeyPress { code: 42, flags: 0 }),
        "bracketright" => Atom::Key(KeyPress { code: 30, flags: 0 }),
        "asciicircum" => Atom::Key(KeyPress {
            code: 22,
            flags: 131072,
        }),
        "underscore" => Atom::Key(KeyPress {
            code: 27,
            flags: 131072,
        }),
        "grave" => Atom::Key(KeyPress { code: 50, flags: 0 }),
        "a" => Atom::Key(KeyPress { code: 0, flags: 0 }),
        "b" => Atom::Key(KeyPress { code: 11, flags: 0 }),
        "c" => Atom::Key(KeyPress { code: 8, flags: 0 }),
        "d" => Atom::Key(KeyPress { code: 2, flags: 0 }),
        "e" => Atom::Key(KeyPress { code: 14, flags: 0 }),
        "f" => Atom::Key(KeyPress { code: 3, flags: 0 }),
        "g" => Atom::Key(KeyPress { code: 5, flags: 0 }),
        "h" => Atom::Key(KeyPress { code: 4, flags: 0 }),
        "i" => Atom::Key(KeyPress { code: 34, flags: 0 }),
        "j" => Atom::Key(KeyPress { code: 38, flags: 0 }),
        "k" => Atom::Key(KeyPress { code: 40, flags: 0 }),
        "l" => Atom::Key(KeyPress { code: 37, flags: 0 }),
        "m" => Atom::Key(KeyPress { code: 46, flags: 0 }),
        "n" => Atom::Key(KeyPress { code: 45, flags: 0 }),
        "o" => Atom::Key(KeyPress { code: 31, flags: 0 }),
        "p" => Atom::Key(KeyPress { code: 35, flags: 0 }),
        "q" => Atom::Key(KeyPress { code: 12, flags: 0 }),
        "r" => Atom::Key(KeyPress { code: 15, flags: 0 }),
        "s" => Atom::Key(KeyPress { code: 1, flags: 0 }),
        "t" => Atom::Key(KeyPress { code: 17, flags: 0 }),
        "u" => Atom::Key(KeyPress { code: 32, flags: 0 }),
        "v" => Atom::Key(KeyPress { code: 9, flags: 0 }),
        "w" => Atom::Key(KeyPress { code: 13, flags: 0 }),
        "x" => Atom::Key(KeyPress { code: 7, flags: 0 }),
        "y" => Atom::Key(KeyPress { code: 16, flags: 0 }),
        "z" => Atom::Key(KeyPress { code: 6, flags: 0 }),
        "braceleft" => Atom::Key(KeyPress {
            code: 33,
            flags: 131072,
        }),
        "bar" => Atom::Key(KeyPress {
            code: 42,
            flags: 131072,
        }),
        "braceright" => Atom::Key(KeyPress {
            code: 30,
            flags: 131072,
        }),
        "asciitilde" => Atom::Key(KeyPress {
            code: 50,
            flags: 131072,
        }),
        _ => return None,
    })
}
