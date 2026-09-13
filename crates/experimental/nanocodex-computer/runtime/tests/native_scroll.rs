use skyre::native::scroll::page_deltas;

#[test]
fn original_scroll_page_geometry_minimum_axis_rounding_and_direction() {
    // Recovered handler 0x10014e59c/0x10014ead0, arithmetic 0x10015422c;
    // its LLVM FRINTA and returned x0/x1 are sealed with the native evidence.
    let frame = [-1024.0, 37.0, 800.0, 600.0];
    assert_eq!(page_deltas("up", 1.0, frame).unwrap(), [0, 600]);
    assert_eq!(page_deltas("down", 1.0, frame).unwrap(), [0, -600]);
    assert_eq!(page_deltas("left", 1.0, frame).unwrap(), [-800, 0]);
    assert_eq!(page_deltas("right", 1.0, frame).unwrap(), [800, 0]);
    let small = [0.0, 0.0, 20.0, 40.0];
    assert_eq!(page_deltas("up", 0.125, small).unwrap(), [0, 13]);
    assert_eq!(page_deltas("left", 0.125, small).unwrap(), [-13, 0]);
    assert_eq!(page_deltas("right", 0.001, small).unwrap(), [0, 0]);
    assert_eq!(page_deltas("down", 0.5, frame).unwrap(), [0, -300]);
}

#[test]
fn original_scroll_integer_boundaries_fail_instead_of_saturating_or_posting() {
    let frame = [0.0, 0.0, 100.0, 100.0];
    assert_eq!(
        page_deltas("up", i32::MAX as f64 / 100.0, frame).unwrap(),
        [0, i32::MAX]
    );
    assert_eq!(
        page_deltas("down", -(i32::MIN as f64) / 100.0, frame).unwrap(),
        [0, i32::MIN]
    );
    assert!(page_deltas("up", -(i32::MIN as f64) / 100.0, frame).is_err());
    for value in [0.0, -1.0, f64::INFINITY, f64::NAN, f64::MAX] {
        assert!(page_deltas("up", value, frame).is_err());
    }
    assert!(page_deltas("north", 1.0, frame).is_err());
    assert!(page_deltas("up", 1.0, [0.0, 0.0, 1.0, f64::NAN]).is_err());
}
