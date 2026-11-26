//! Absolute Oklab, OkLCh, and RGB profile colors on the engine's 8-bit sRGB paint surface.
use super::{Color, calc::number_end, functional_color_components, hue_degrees};

fn component(token: &str, percent_scale: f64) -> Option<f64> {
    if token == "none" {
        return Some(0.);
    }
    let (raw, scale) = token
        .strip_suffix('%')
        .map_or((token, 1.), |n| (n, percent_scale));
    if number_end(raw)? != raw.len() {
        return None;
    }
    let number = raw.parse::<f64>().ok()? * scale;
    number.is_finite().then_some(number)
}

fn hue(token: &str) -> Option<f64> {
    if token == "none" {
        return Some(0.);
    }
    let end = number_end(token)?;
    if !matches!(&token[end..], "" | "deg" | "grad" | "rad" | "turn") {
        return None;
    }
    hue_degrees(token).map(f64::to_radians)
}

pub(super) fn parse(source: &str) -> Option<Color> {
    let (name, body) = source.split_once('(')?;
    let body = body.strip_suffix(')')?;
    if body.contains(',') {
        return None;
    }
    let (space, channels) = if name == "color" {
        body.trim_ascii_start()
            .split_once(|c: char| c.is_ascii_whitespace())?
    } else {
        (name, body)
    };
    let ([first, second, third], alpha) = functional_color_components(channels, false)?;
    let alpha = component(alpha.unwrap_or("1"), 0.01)?.clamp(0., 1.);
    let linear = match (name, space) {
        ("color", "srgb" | "srgb-linear" | "display-p3" | "display-p3-linear") => {
            let channels = [first, second, third].map(|c| component(c, 0.01));
            let [r, g, b] = channels;
            profile_to_srgb(space, [r?, g?, b?])
        }
        ("oklch", _) => map_to_srgb(
            component(first, 0.01)?.clamp(0., 1.),
            component(second, 0.004)?.max(0.),
            hue(third)?,
        ),
        ("oklab", _) => {
            let a = component(second, 0.004)?;
            let b = component(third, 0.004)?;
            map_to_srgb(
                component(first, 0.01)?.clamp(0., 1.),
                a.hypot(b),
                b.atan2(a),
            )
        }
        _ => return None,
    };
    Some(linear_srgb_to_color(linear, (alpha * 255.).round() as u8))
}

pub(super) fn encoded_srgb_to_color(rgb: [f64; 3], alpha: u8) -> Color {
    linear_srgb_to_color(profile_to_srgb("srgb", rgb), alpha)
}

fn linear_srgb_to_color(linear: [f64; 3], alpha: u8) -> Color {
    let [r, g, b] = linear.map(|linear| {
        let encoded = if linear <= 0.0031308 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1. / 2.4) - 0.055
        };
        (encoded.clamp(0., 1.) * 255. + 1e-12).round() as u8
    });
    Color(r, g, b, alpha)
}

fn profile_to_srgb(space: &str, rgb: [f64; 3]) -> [f64; 3] {
    let linear_profile = space.ends_with("-linear");
    // Normalize before exponentiating extremely large, finite inputs. The
    // primary conversion is linear and Oklab's cone response is a cube root,
    // so we can restore the magnitude after conversion without overflowing.
    let peak = rgb.into_iter().map(f64::abs).fold(1., f64::max);
    let scale = if peak > 1e50 { peak } else { 1. };
    let [r, g, b] = rgb.map(|v| {
        if linear_profile {
            v / scale
        } else if v.abs() <= 0.04045 {
            v / 12.92 / scale.powf(2.4)
        } else {
            (((v.abs() + 0.055) / 1.055 / scale).powf(2.4)).copysign(v)
        }
    });
    // CSS Color 4, linear Display-P3 to XYZ-D65 then XYZ-D65 to linear sRGB.
    let rgb = if space.starts_with("display-p3") {
        [
            1.22494017628056 * r - 0.22494017628056 * g,
            -0.0420569547096882 * r + 1.0420569547096882 * g,
            -0.0196375545903344 * r - 0.0786360455506318 * g + 1.0982736001409662 * b,
        ]
    } else {
        [r, g, b]
    };
    if scale == 1. && rgb.iter().all(|v| (0.0..=1.0).contains(v)) {
        return rgb;
    }
    let [l, a, b] = oklab(rgb);
    let magnitude = if linear_profile {
        scale.cbrt()
    } else {
        scale.powf(0.8)
    };
    map_to_srgb(
        (l * magnitude).clamp(0., 1.),
        a.hypot(b) * magnitude,
        b.atan2(a),
    )
}

// Björn Ottosson's D65 linear-sRGB ↔ Oklab matrices.
// https://bottosson.github.io/posts/oklab/#converting-from-linear-srgb-to-oklab
fn linear_rgb([lightness, a, b]: [f64; 3]) -> [f64; 3] {
    let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

fn oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

// CSS Color 4, "Binary Search Gamut Mapping with Local MINDE". Clipping in
// linear light is equivalent to clipping encoded sRGB, whose transfer is monotonic.
fn map_to_srgb(lightness: f64, chroma: f64, angle: f64) -> [f64; 3] {
    if lightness == 0. || lightness == 1. {
        return [lightness; 3];
    }
    const JND: f64 = 0.02;
    const EPSILON: f64 = 0.0001;
    let (sin, cos) = angle.sin_cos();
    let at = |c| [lightness, c * cos, c * sin];
    let delta = |c: [f64; 3], clipped| {
        let [l, a, b] = oklab(clipped);
        ((c[0] - l).powi(2) + (c[1] - a).powi(2) + (c[2] - b).powi(2)).sqrt()
    };

    // For any clipped sRGB color, the cube-root cone responses are in [0,1],
    // hence |a| < 2.43 and |b| < 0.81 by the matrix above. Chroma >= 3
    // cannot be within one JND of the gamut. This also bounds the search at
    // 15 bisections for arbitrarily large authored numbers.
    let mut max = chroma.min(3.);
    let lab = at(max);
    let rgb = linear_rgb(lab);
    let mut clipped = rgb.map(|v| v.clamp(0., 1.));
    if rgb == clipped || delta(lab, clipped) < JND {
        return clipped;
    }
    let mut min = 0.;
    let mut min_in_gamut = true;
    while max - min > EPSILON {
        let mid = (max + min) * 0.5;
        let lab = at(mid);
        let rgb = linear_rgb(lab);
        if min_in_gamut && rgb.iter().all(|v| (0.0..=1.0).contains(v)) {
            min = mid;
            continue;
        }
        clipped = rgb.map(|v| v.clamp(0., 1.));
        let difference = delta(lab, clipped);
        if difference < JND {
            if JND - difference < EPSILON {
                return clipped;
            }
            min_in_gamut = false;
            min = mid;
        } else {
            max = mid;
        }
    }
    clipped
}
