//! The curves against hyprutils' own algorithm, and the tree against
//! Hyprland's own inheritance.

use compositor_config::{NoSources, Raw, parse};

use super::{Bezier, Curves, Moving, Settings, Tree, read};

fn lines(text: &str) -> Vec<Raw> {
    parse("test", text, &mut NoSources).config.animations
}

/// The straight line is the one case with an answer anybody can check.
#[test]
fn linear_is_the_straight_line() {
    let linear = Bezier::linear();
    for at in [0.0_f32, 0.1, 0.25, 0.5, 0.75, 0.9] {
        let got = linear.y_for_x(at);
        assert!(
            (got - at).abs() < 0.01,
            "linear at {at} gave {got}, not {at}"
        );
    }
    assert_eq!(linear.y_for_x(1.0), 1.0);
    assert_eq!(linear.y_for_x(0.0), 0.0);
    // Past either end is that end, which is what keeps a late tick from
    // overshooting.
    assert_eq!(linear.y_for_x(-1.0), 0.0);
    assert_eq!(linear.y_for_x(2.0), 1.0);
}

/// Hyprland's `default` starts fast: a quarter of the time is well over a
/// quarter of the distance. Its control points are `DEFAULTBEZIERPOINTS`.
#[test]
fn the_default_curve_starts_fast_and_eases_out() {
    let curve = Bezier::default_curve();
    assert_eq!(curve.control(), &[(0.0, 0.75), (0.15, 1.0)]);

    // A quarter of the time in it is five sixths of the way there. The
    // number is worked out from the control points by hand: `x = 0.25` is
    // `t ≈ 0.57`, and `y` there is `2.25t(1-t)² + 3t²(1-t) + t³ ≈ 0.841`.
    let quarter = curve.y_for_x(0.25);
    assert!(
        (quarter - 0.843).abs() < 0.01,
        "a quarter of the way in it is at {quarter}"
    );
    let half = curve.y_for_x(0.5);
    assert!(half > quarter && half < 1.0, "half way it is at {half}");
    assert!(curve.y_for_x(0.9) > half);
    // It rises the whole way: a curve that went backwards would be a window
    // that slid the wrong way in the middle of a move.
    let mut last = 0.0;
    for step in 0..=100u8 {
        let at = curve.y_for_x(f32::from(step) / 100.0);
        assert!(at >= last - 0.001, "it went backwards at {step}");
        last = at;
    }
}

/// A curve is only as good as its ends: whatever the control points, it goes
/// from nothing to everything.
#[test]
fn every_curve_starts_at_nothing_and_ends_at_everything() {
    for (first, second) in [
        ((0.0, 0.0), (1.0, 1.0)),
        ((0.05, 0.9), (0.1, 1.05)),
        ((0.65, 0.0), (0.35, 1.0)),
        ((0.0, 1.0), (1.0, 0.0)),
    ] {
        let curve = Bezier::new(first, second);
        assert_eq!(curve.y_for_x(0.0), 0.0, "{first:?} {second:?}");
        assert_eq!(curve.y_for_x(1.0), 1.0, "{first:?} {second:?}");
        assert!(curve.y_for_x(0.5).is_finite());
    }
}

/// An overshooting curve -- a control point above one -- goes past its goal
/// and comes back, which is what `0.1, 1.05` in half the configurations on
/// the internet is for.
#[test]
fn a_curve_may_overshoot() {
    let curve = Bezier::new((0.05, 0.9), (0.1, 1.05));
    let most = (0..=100u8)
        .map(|step| curve.y_for_x(f32::from(step) / 100.0))
        .fold(0.0_f32, f32::max);
    assert!(most > 1.0, "it never went past its goal: {most}");
    assert_eq!(curve.y_for_x(1.0), 1.0, "and it came back");
}

// -- The tree ---------------------------------------------------------------

#[test]
fn a_node_takes_its_parents_settings_until_it_is_set() {
    let mut tree = Tree::new();
    // Nothing set: everything is `global`'s, which is on, speed 8, default.
    let windows = tree.get("windowsMove");
    assert_eq!(windows, Settings::default());
    assert!((tree.duration("windowsMove") - 800.0).abs() < f32::EPSILON);

    // Setting the parent sets the child.
    assert!(tree.set(
        "windows",
        Settings {
            enabled: true,
            speed: 3.0,
            curve: "linear".to_owned(),
            style: String::new(),
        }
    ));
    assert_eq!(tree.get("windowsMove").curve, "linear");
    assert!((tree.duration("windowsMove") - 300.0).abs() < f32::EPSILON);
    // And a sibling of that parent is untouched.
    assert_eq!(tree.get("fadeIn").curve, "default");

    // Setting the child overrides the parent for it alone.
    assert!(tree.set(
        "windowsMove",
        Settings {
            enabled: true,
            speed: 1.0,
            curve: "default".to_owned(),
            style: String::new(),
        }
    ));
    assert!((tree.duration("windowsMove") - 100.0).abs() < f32::EPSILON);
    assert!((tree.duration("windowsIn") - 300.0).abs() < f32::EPSILON);
}

#[test]
fn a_node_that_is_off_takes_no_time_at_all() {
    let mut tree = Tree::new();
    assert!(tree.set(
        "windows",
        Settings {
            enabled: false,
            ..Settings::default()
        }
    ));
    assert_eq!(tree.duration("windowsMove"), 0.0);
    assert!(!tree.get("windowsMove").enabled);
}

#[test]
fn a_name_the_tree_does_not_have_is_refused() {
    let mut tree = Tree::new();
    assert!(!tree.set("nosuchthing", Settings::default()));
    assert!(Tree::has("specialWorkspaceIn"));
    assert!(!Tree::has("specialWorkspaceSideways"));
}

// -- Reading the configuration ----------------------------------------------

#[test]
fn a_configuration_declares_curves_and_sets_nodes() {
    let (curves, tree, said) = read(&lines(
        "bezier = overshot, 0.05, 0.9, 0.1, 1.05\n\
         animation = windows, 1, 7, overshot\n\
         animation = windowsOut, 1, 4, default, popin 80%\n\
         animation = fade, 0, 1, default\n",
    ));
    assert_eq!(said, Vec::new(), "{said:?}");
    assert!(curves.has("overshot"));
    assert_eq!(curves.len(), 3, "the two built in, and one declared");

    assert_eq!(tree.get("windowsMove").curve, "overshot");
    assert!((tree.duration("windowsMove") - 700.0).abs() < f32::EPSILON);
    assert_eq!(tree.get("windowsOut").style, "popin 80%");
    assert!((tree.duration("windowsOut") - 400.0).abs() < f32::EPSILON);
    assert_eq!(tree.duration("fadeIn"), 0.0, "fade was turned off");
}

/// Hyprland reports a bad line and applies the rest of the file; a
/// compositor that stopped at the first typo would be one nobody could
/// configure.
#[test]
fn a_line_that_cannot_be_used_is_reported_and_the_rest_applies() {
    let (curves, tree, said) = read(&lines(
        "animation = nosuchthing, 1, 5, default\n\
         animation = windows, 2, 5, default\n\
         animation = windowsIn, 1, 0, default\n\
         animation = windowsOut, 1, 5, nosuchcurve\n\
         bezier = short, 0.1, 0.2\n\
         bezier = wrong, a, b, c, d\n\
         animation = workspaces, 1, 2, linear\n",
    ));
    let reasons: Vec<&str> = said.iter().map(|one| one.reason.as_str()).collect();
    assert_eq!(
        reasons,
        [
            "no such animation",
            "invalid animation on/off state",
            "invalid speed",
            "no such bezier",
            "a bezier is a name and four numbers",
            "a bezier's points are numbers",
        ]
    );
    // The good line at the end still applied.
    assert_eq!(tree.get("workspacesIn").curve, "linear");
    assert!((tree.duration("workspacesIn") - 200.0).abs() < f32::EPSILON);
    assert_eq!(curves.len(), 2, "neither bad bezier was added");
}

/// A curve nothing declared is the default one rather than no animation:
/// `CAnimationManager::getBezier` falls back to it.
#[test]
fn an_unknown_curve_is_the_default_one() {
    let curves = Curves::new();
    assert_eq!(
        curves.get("nosuchcurve").control(),
        Bezier::default_curve().control()
    );
}

// -- A value on its way -----------------------------------------------------

#[test]
fn a_value_moves_along_its_curve_and_arrives() {
    let linear = Bezier::linear();
    let moving = Moving::still(100.0).towards(200.0, 1_000, 400.0, &linear);
    assert_eq!(moving.at(1_000, &linear), 100.0);
    let half = moving.at(1_200, &linear);
    assert!((half - 150.0).abs() < 2.0, "half way it was at {half}");
    assert_eq!(moving.at(1_400, &linear), 200.0);
    assert_eq!(moving.at(9_999, &linear), 200.0, "and it stays there");
    assert!(moving.finished(1_400));
    assert!(!moving.finished(1_399));
    assert_eq!(moving.goal(), 200.0);
}

/// A window asked to move again half-way through a move carries on from
/// where it is, rather than jumping to where it was going.
#[test]
fn a_new_goal_starts_from_where_the_value_is() {
    let linear = Bezier::linear();
    let moving = Moving::still(0.0).towards(100.0, 0, 1_000.0, &linear);
    let again = moving.towards(0.0, 500, 1_000.0, &linear);
    let at = again.at(500, &linear);
    assert!((at - 50.0).abs() < 2.0, "it jumped to {at}");
    assert_eq!(again.goal(), 0.0);
}

/// A duration of zero is a node that is off: the value is simply there.
#[test]
fn no_duration_warps() {
    let linear = Bezier::linear();
    let moving = Moving::still(0.0).towards(100.0, 0, 0.0, &linear);
    assert_eq!(moving.at(0, &linear), 100.0);
    assert!(moving.finished(0));
}
