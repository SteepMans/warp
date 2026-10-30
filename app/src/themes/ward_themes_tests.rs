use warp_core::ui::color::contrast::relative_luminance;

use super::*;

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

fn color(value: u32) -> ColorU {
    ColorU::from_u32(value)
}

fn contrast(a: u32, b: u32) -> f32 {
    let la = relative_luminance(color(a)) + 0.05;
    let lb = relative_luminance(color(b)) + 0.05;
    la.max(lb) / la.min(lb)
}

/// HSV hue in degrees.
fn hue(value: u32) -> f32 {
    let c = color(value);
    let (r, g, b) = (c.r as f32, c.g as f32, c.b as f32);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if delta == 0. {
        return 0.;
    }
    let h = if max == r {
        60. * (((g - b) / delta).rem_euclid(6.))
    } else if max == g {
        60. * ((b - r) / delta + 2.)
    } else {
        60. * ((r - g) / delta + 4.)
    };
    h.rem_euclid(360.)
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.;
    d.min(360. - d)
}

#[test]
fn ward_theme_names_are_grouped_and_match_kind() {
    for (kind, palette) in WARD_PALETTES.iter() {
        assert!(palette.name.starts_with("Ward "), "{}", palette.name);
        assert_eq!(kind.to_string(), palette.name);
        assert_eq!(palette.theme().name().as_deref(), Some(palette.name));
    }
}

#[test]
fn ward_themes_include_light_and_dark() {
    let light = WARD_PALETTES.iter().filter(|(_, p)| p.light).count();
    assert!(light >= 2);
    assert!(WARD_PALETTES.len() - light >= 2);
}

#[test]
fn ward_themes_have_readable_contrast() {
    for (_, palette) in WARD_PALETTES.iter() {
        for &bg in palette.background {
            let fg = contrast(palette.foreground, bg);
            assert!(fg >= 4.5, "{}: foreground contrast {fg:.2}", palette.name);

            for &accent in palette.accent {
                let c = contrast(accent, bg);
                assert!(c >= 3., "{}: accent contrast {c:.2}", palette.name);
            }
            let cursor = contrast(palette.cursor, bg);
            assert!(
                cursor >= 3.,
                "{}: cursor contrast {cursor:.2}",
                palette.name
            );

            // Colors used for text: every ANSI color except the one that is meant to blend
            // with the background (normal black on dark themes, white on light themes).
            for (set, colors) in [("normal", &palette.normal), ("bright", &palette.bright)] {
                for (index, &value) in colors.iter().enumerate() {
                    let background_like = if palette.light {
                        index == 7
                    } else {
                        index == 0 && set == "normal"
                    };
                    if background_like {
                        continue;
                    }
                    let c = contrast(value, bg);
                    assert!(
                        c >= 3.,
                        "{}: {set} {} contrast {c:.2}",
                        palette.name,
                        ANSI_NAMES[index]
                    );
                }
            }
        }
    }
}

#[test]
fn ward_theme_ansi_hues_are_distinguishable() {
    for (_, palette) in WARD_PALETTES.iter() {
        for i in 1..7 {
            for j in (i + 1)..7 {
                let d = hue_distance(hue(palette.normal[i]), hue(palette.normal[j]));
                assert!(
                    d >= 20.,
                    "{}: {} and {} hues are {d:.1} degrees apart",
                    palette.name,
                    ANSI_NAMES[i],
                    ANSI_NAMES[j]
                );
            }
        }
    }
}

/// Ward agent states use ANSI yellow for "waiting" and ANSI red for "error".
#[test]
fn ward_theme_agent_status_colors_stay_recognizable() {
    for (_, palette) in WARD_PALETTES.iter() {
        let red = hue(palette.normal[1]);
        assert!(
            hue_distance(red, 0.) <= 20.,
            "{}: red hue {red:.1}",
            palette.name
        );
        let yellow = hue(palette.normal[3]);
        assert!(
            (30. ..=65.).contains(&yellow),
            "{}: yellow hue {yellow:.1}",
            palette.name
        );
    }
}
