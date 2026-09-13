//! Original arithmetic from immutable native-screenshot-encoding and
//! native-screenshot-coordinates audits. No desktop or original service runs.
use skyre::native::screenshot::{
    Configuration, Displays, Encoding, Geometry, Publication, Screen, normalize_size,
};
fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}
fn displays() -> Displays {
    Displays(vec![Screen {
        frame: [0., 0., 1920., 1080.],
        backing_scale: 2.,
    }])
}

#[test]
fn captured_default_is_normalized_jpeg_point_eight_without_public_options() {
    let default = Configuration::default();
    assert!(default.normalize_to_points);
    assert_eq!(default.encoding, Encoding::Jpeg { quality: Some(0.8) });
    for q in [0., -0.1, 1.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            Configuration::from_flags(true, true, q).encoding,
            Encoding::Jpeg { quality: None }
        );
    }
    assert_eq!(
        Configuration::from_flags(false, true, 1.).encoding,
        Encoding::Jpeg { quality: Some(1.) }
    );
    assert_eq!(
        Configuration::from_flags(false, false, f64::NAN).encoding,
        Encoding::Png
    );
}
#[test]
fn original_sizing_normalizes_then_limits_long_and_short_edges_without_upscaling() {
    for (pixels, scale, normalize, expected) in [
        ([1600., 1200.], 2., true, [2., 800., 600.]),
        ([1600., 1200.], 2., false, [1.5625, 1024., 768.]),
        ([4096., 1024.], 1., true, [2., 2048., 512.]),
        ([3840., 2160.], 2., true, [2.8125, 1365.3333333333333, 768.]),
        ([200., 100.], 1., false, [1., 200., 100.]),
        ([800., 4000.], 1., true, [1.953125, 409.6, 2048.]),
    ] {
        let result = normalize_size(pixels, scale, normalize).unwrap();
        for (actual, expected) in [result.effective_scale, result.width, result.height]
            .into_iter()
            .zip(expected)
        {
            near(actual, expected);
        }
    }
}
#[test]
fn nonpositive_original_size_is_one_by_one_but_capture_refuses_invalid_window() {
    for pixels in [[0., 9.], [-1., 9.], [9., 0.]] {
        let size = normalize_size(pixels, 2., true).unwrap();
        assert_eq!(
            [size.effective_scale, size.width, size.height],
            [2., 1., 1.]
        );
    }
    assert!(Geometry::new([0., 0., 0., 20.], 2., Configuration::default()).is_err());
    for (pixels, scale) in [
        ([f64::NAN, 1.], 1.),
        ([1., f64::INFINITY], 1.),
        ([1., 1.], 0.),
        ([1., 1.], f64::NAN),
    ] {
        assert!(normalize_size(pixels, scale, true).is_err());
    }
}
#[test]
fn unrounded_ratio_survives_ceil_and_both_drag_endpoints_scale_once() {
    let g = Geometry::new([100., 200., 1920., 1080.], 2., Configuration::default()).unwrap();
    assert_eq!(g.pixels, [1366, 768]);
    near(g.input_scale, 1.40625);
    assert_ne!(g.input_scale, 1920. / 1366.);
    assert_eq!(g.screen_point([10.9, 20.9]).unwrap(), [114.0625, 228.125]);
    assert_eq!(
        g.screen_point([1365.9, 767.9]).unwrap(),
        [2019.53125, 1278.59375]
    );
    assert!(g.screen_point([1366., 0.]).is_err());
    assert!(g.screen_point([0., 768.]).is_err());
}
#[test]
fn fractional_coordinates_use_original_toward_zero_integer_conversion() {
    let g = Geometry::new(
        [-100., 40., 100., 80.],
        2.,
        Configuration::from_flags(false, true, 0.8),
    )
    .unwrap();
    near(g.input_scale, 0.5);
    assert_eq!(g.screen_point([99.9, 10.1]).unwrap(), [-50.5, 45.]);
    assert_eq!(g.screen_point([-0.9, -0.1]).unwrap(), [-100., 40.]);
    for point in [
        [-1., 0.],
        [f64::NAN, 0.],
        [f64::INFINITY, 0.],
        [9_223_372_036_854_775_808., 0.],
        [-9_223_372_036_854_775_808., 0.],
    ] {
        assert!(g.screen_point(point).is_err());
    }
}
#[test]
fn screenshot_scale_does_not_rescale_native_scroll_distance() {
    let g = Geometry::new([100., 200., 1920., 1080.], 2., Configuration::default()).unwrap();
    assert_eq!(g.screen_point([100., 100.]).unwrap(), [240.625, 340.625]);
    assert_eq!(
        skyre::native::scroll::page_deltas("down", 1., g.frame).unwrap(),
        [0, -1080]
    );
}
#[test]
fn display_selection_uses_first_maximum_area_and_cocoa_vertical_conversion() {
    let d = Displays(vec![
        Screen {
            frame: [0., 0., 100., 100.],
            backing_scale: 1.,
        },
        Screen {
            frame: [100., 0., 100., 100.],
            backing_scale: 2.,
        },
        Screen {
            frame: [0., 100., 100., 100.],
            backing_scale: 3.,
        },
    ]);
    assert_eq!(d.scale_for_window([75., 20., 50., 20.]).unwrap(), 1.);
    assert_eq!(d.scale_for_window([90., 20., 50., 20.]).unwrap(), 2.);
    assert_eq!(d.scale_for_window([10., -90., 20., 20.]).unwrap(), 3.);
    assert_eq!(d.scale_for_window([1000., 1000., 20., 20.]).unwrap(), 1.);
    assert_eq!(
        Displays(vec![])
            .scale_for_window([0., 0., 20., 20.])
            .unwrap(),
        2.
    );
}
#[test]
fn display_metadata_rejects_invalid_scale_and_bounded_inventory() {
    let bad = Screen {
        frame: [0., 0., 100., 100.],
        backing_scale: f64::NAN,
    };
    assert!(
        Displays(vec![bad.clone()])
            .scale_for_window([0., 0., 20., 20.])
            .is_err()
    );
    assert!(
        Displays(vec![bad; 65])
            .scale_for_window([0., 0., 20., 20.])
            .is_err()
    );
}
#[test]
fn only_current_observation_receipt_can_publish_geometry() {
    let d = displays();
    let frame = [0., 0., 1920., 1080.];
    let g = Geometry::new(frame, 2., Configuration::default()).unwrap();
    let mut state = Publication::default();
    assert!(state.current(frame, &d).is_err());
    let obsolete = state.begin();
    let current = state.begin();
    assert!(state.commit(obsolete, g, d.clone()).is_err());
    assert!(state.current(frame, &d).is_err());
    state.commit(current, g, d.clone()).unwrap();
    assert_eq!(state.current(frame, &d).unwrap(), g);
    state.invalidate();
    assert!(state.current(frame, &d).is_err());
    assert!(state.commit(current, g, d).is_err());
}
#[test]
fn publication_rejects_window_move_and_same_frame_display_scale_change() {
    let d = displays();
    let frame = [0., 0., 1920., 1080.];
    let g = Geometry::new(frame, 2., Configuration::default()).unwrap();
    let mut state = Publication::default();
    let receipt = state.begin();
    state.commit(receipt, g, d.clone()).unwrap();
    assert!(state.current([1., 0., 1920., 1080.], &d).is_err());
    let mut altered = d;
    altered.0[0].backing_scale = 1.;
    assert!(state.current(frame, &altered).is_err());
}
