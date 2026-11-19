//! Validate the narrow display geometry used by emulator multi-touch.
//!
//! QEMU's TouchEventSender scales a panel coordinate `p` on an axis of size `D`
//! to `floor(32767 * p / D)`. Android 15 rotates these discrete raw coordinates
//! around 32767, then scales by the rotated display size / 32768. Consequently a
//! reflected logical coordinate `p` maps to `D - p`, including the endpoint `D`;
//! `D - 1 - p` would introduce an extra pixel of displacement.
//!
//! Sources (read together, including the discrete/continuous distinction):
//! - QEMU 35.6.3, TouchEventSender.cpp::scaleAxis and doSend:
//!   https://github.com/aosp-mirror/platform_external_qemu/blob/ae9d18d2b6261179fbd57fffec720a04f7bfb053/android/android-grpc/services/emulator-controller/server/src/android/emulation/control/keyboard/TouchEventSender.cpp
//! - Android 15 r1, TouchInputMapper.cpp::computeInputTransforms:
//!   https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-15.0.0_r1/services/inputflinger/reader/mapper/TouchInputMapper.cpp
//! - Android 15 r1, ui::Transform rotation matrices:
//!   https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-15.0.0_r1/libs/ui/Transform.cpp
//! - Android 15 r1, DisplayDevice.cpp::getFrontEndInfo sends the inverse display
//!   transform to input; in a full-panel viewport it cancels InputReader's final
//!   undo-rotation, leaving the raw-to-rotated-display transform above:
//!   https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-15.0.0_r1/services/surfaceflinger/DisplayDevice.cpp
//!
//! For the identity-calibration source model and dimensions at most 4096, the
//! integer inverse below has less than 0.25 pixel of raw-axis quantization error
//! per axis. This is not a claim of exact runtime inversion: `dumpsys input`
//! rounds affine coefficients to three decimal places. We require its reported
//! identity, qualify the stock emulator with real destination assertions, and
//! leave custom calibrated devices unqualified. Missing or visibly different
//! geometry is rejected before a touch can be submitted.

const MAX_PANEL_DIMENSION: u32 = 4096;
const MAX_INPUT_DIAGNOSTICS_BYTES: usize = 2 * 1024 * 1024;
const MAX_WM_DIAGNOSTICS_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogicalTouchGeometry {
    panel_width: u32,
    panel_height: u32,
    rotation: u8,
}

impl LogicalTouchGeometry {
    /// `panel_size` must come from the selected emulator's default display
    /// configuration, not a screenshot or an assumed AVD profile.
    pub(crate) fn from_diagnostics(
        panel_size: (u32, u32),
        wm_size: &str,
        input: &str,
    ) -> Result<Self, &'static str> {
        if panel_size.0 == 0
            || panel_size.1 == 0
            || panel_size.0 > MAX_PANEL_DIMENSION
            || panel_size.1 > MAX_PANEL_DIMENSION
        {
            return Err(
                "multi-touch requires a native panel between 1 and 4096 pixels on each axis",
            );
        }
        if input.len() > MAX_INPUT_DIAGNOSTICS_BYTES || wm_size.len() > MAX_WM_DIAGNOSTICS_BYTES {
            return Err("multi-touch geometry diagnostics exceed the supported size");
        }
        validate_wm_size(wm_size, panel_size)?;

        let lines: Vec<_> = input.lines().collect();
        let mut geometry = None;
        for line in &lines {
            let line = line.trim();
            if !line.starts_with("Viewport ") || integer_field(line, "displayId") != Some(0) {
                continue;
            }
            let viewport_geometry = validate_viewport(line, panel_size)?;
            if geometry.is_some_and(|previous| previous != viewport_geometry) {
                return Err("multi-touch requires a consistent default-display viewport");
            }
            geometry = Some(viewport_geometry);
        }
        let geometry =
            geometry.ok_or("multi-touch requires the complete default-display viewport")?;

        let mut matching_mappers = 0;
        for (index, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("Touch Input Mapper (mode - ") {
                continue;
            }
            let block = indented_block(&lines, index);
            let default_viewports: Vec<_> = block
                .iter()
                .filter(|line| {
                    line.trim_start().starts_with("Viewport ")
                        && integer_field(line, "displayId") == Some(0)
                })
                .collect();
            if default_viewports.is_empty() {
                continue;
            }
            if default_viewports.len() != 1 || line.trim() != "Touch Input Mapper (mode - DIRECT):"
            {
                return Err("multi-touch requires one direct default-display touch mapper");
            }
            validate_touch_mapper(block)?;
            matching_mappers += 1;
        }
        if matching_mappers != 1 {
            return Err("multi-touch requires one fully described default-display touch mapper");
        }
        Ok(geometry)
    }

    pub(crate) fn rotation(self) -> u8 {
        self.rotation
    }

    pub(crate) fn logical_size(self) -> (u32, u32) {
        if self.rotation.is_multiple_of(2) {
            (self.panel_width, self.panel_height)
        } else {
            (self.panel_height, self.panel_width)
        }
    }

    /// The returned panel endpoint may equal its axis extent. QEMU maps that
    /// inclusive endpoint to EVDEV 32767, which Android treats as in range.
    pub(crate) fn panel_point(self, x: u32, y: u32) -> Result<(u32, u32), &'static str> {
        let (width, height) = self.logical_size();
        if x >= width || y >= height {
            return Err("multi-touch coordinate is outside the validated logical display");
        }
        Ok(match self.rotation {
            0 => (x, y),
            1 => (self.panel_width - y, x),
            2 => (self.panel_width - x, self.panel_height - y),
            3 => (y, self.panel_height - x),
            _ => return Err("multi-touch display rotation is unsupported"),
        })
    }
}

fn validate_wm_size(output: &str, panel_size: (u32, u32)) -> Result<(), &'static str> {
    if output
        .lines()
        .any(|line| line.trim_start().starts_with("Override size:"))
    {
        return Err("multi-touch does not support wm size overrides");
    }
    let mut physical = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Physical size:"));
    let size = physical
        .next()
        .and_then(|size| size.trim().split_once('x'))
        .and_then(|(width, height)| {
            Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
        });
    if physical.next().is_some() || size != Some(panel_size) {
        return Err("multi-touch requires matching emulator and Android native display dimensions");
    }
    Ok(())
}

fn validate_viewport(
    line: &str,
    panel_size: (u32, u32),
) -> Result<LogicalTouchGeometry, &'static str> {
    if !line.starts_with("Viewport INTERNAL:") || array_field::<1>(line, "isActive") != Some([1]) {
        return Err("multi-touch requires an active internal default-display viewport");
    }
    let rotation = integer_field(line, "orientation")
        .filter(|rotation| (0..=3).contains(rotation))
        .ok_or("multi-touch requires an observed display rotation from 0 through 3")?
        as u8;
    let geometry = LogicalTouchGeometry {
        panel_width: panel_size.0,
        panel_height: panel_size.1,
        rotation,
    };
    let (width, height) = geometry.logical_size();
    let frame = [0, 0, width as i32, height as i32];
    if array_field::<4>(line, "logicalFrame") != Some(frame)
        || array_field::<4>(line, "physicalFrame") != Some(frame)
        || array_field::<2>(line, "deviceSize") != Some([width as i32, height as i32])
    {
        return Err(
            "multi-touch requires an unscaled full-panel viewport without cropping or offsets",
        );
    }
    Ok(geometry)
}

fn validate_touch_mapper(lines: &[&str]) -> Result<(), &'static str> {
    let parameters = section(lines, "Parameters:")
        .ok_or("multi-touch requires explicit touch-mapper parameters")?;
    if entry(parameters, "DeviceType:") != Some("TOUCH_SCREEN")
        || entry(parameters, "GestureMode:") != Some("MULTI_TOUCH")
        || entry(parameters, "OrientationAware:") != Some("true")
        // Android 15 dumpParameters uses ftl::enum_string, which prints the
        // actual ui::Rotation enumerator "Rotation0". Accept the equivalent
        // ui::toCString alias as well, but never infer zero from missing data.
        || !matches!(entry(parameters, "Orientation:"), Some("Rotation0" | "ROTATION_0"))
    {
        return Err(
            "multi-touch requires an orientation-aware touchscreen with zero input rotation",
        );
    }
    let axes = section(lines, "Raw Touch Axes:")
        .ok_or("multi-touch requires explicit raw touch-axis ranges")?;
    for axis in ["X:", "Y:"] {
        let axis = entry(axes, axis).ok_or("multi-touch requires both raw touch-axis ranges")?;
        if integer_field(axis, "min") != Some(0) || integer_field(axis, "max") != Some(32767) {
            return Err("multi-touch requires raw touch axes spanning 0 through 32767");
        }
    }
    let affine = section(lines, "Affine Transformation:")
        .ok_or("multi-touch requires reported touch calibration")?;
    // Android prints these values with only three decimal places. This guard
    // detects reported custom calibration; real fixture checks qualify stock
    // images. Do not interpret it as a full-precision calibration measurement.
    for (key, expected) in [
        ("X scale:", 1.0),
        ("X ymix:", 0.0),
        ("X offset:", 0.0),
        ("Y xmix:", 0.0),
        ("Y scale:", 1.0),
        ("Y offset:", 0.0),
    ] {
        if entry(affine, key).and_then(|value| value.parse::<f64>().ok()) != Some(expected) {
            return Err("multi-touch requires reported identity touch calibration");
        }
    }
    Ok(())
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn indented_block<'a>(lines: &'a [&'a str], header: usize) -> &'a [&'a str] {
    let indent = indentation(lines[header]);
    let tail = &lines[header + 1..];
    let end = tail
        .iter()
        .position(|line| !line.trim().is_empty() && indentation(line) <= indent)
        .unwrap_or(tail.len());
    &tail[..end]
}

fn section<'a>(lines: &'a [&'a str], header: &str) -> Option<&'a [&'a str]> {
    let mut matches = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == header);
    let (index, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(indented_block(lines, index))
}

fn entry<'a>(lines: &[&'a str], key: &str) -> Option<&'a str> {
    let mut matches = lines
        .iter()
        .filter_map(|line| line.trim().strip_prefix(key));
    let value = matches.next()?.trim();
    matches.next().is_none().then_some(value)
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let key = format!("{key}=");
    let mut matches = line.match_indices(&key).filter(|(index, _)| {
        *index == 0
            || line[..*index]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_whitespace() || ch == ',')
    });
    let (index, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(line[index + key.len()..].trim_start())
}

fn integer_field(line: &str, key: &str) -> Option<i32> {
    field(line, key)?
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .next()?
        .parse()
        .ok()
}

fn array_field<const N: usize>(line: &str, key: &str) -> Option<[i32; N]> {
    let value = field(line, key)?.strip_prefix('[')?.split_once(']')?.0;
    let values: Vec<i32> = value
        .split(',')
        .map(|value| value.trim().parse())
        .collect::<Result<_, _>>()
        .ok()?;
    values.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostics(panel_size: (u32, u32), rotation: u8) -> String {
        let (width, height) = if rotation.is_multiple_of(2) {
            panel_size
        } else {
            (panel_size.1, panel_size.0)
        };
        let viewport = format!(
            "Viewport INTERNAL: displayId=0, uniqueId=local:0, port=0, orientation={rotation}, logicalFrame=[0, 0, {width}, {height}], physicalFrame=[0, 0, {width}, {height}], deviceSize=[{width}, {height}], isActive=[1]"
        );
        format!(
            "Input Reader State:\n  {viewport}\n  Device 7: virtio_input_multi_touch_1\n    Touch Input Mapper (mode - DIRECT):\n      Parameters:\n        GestureMode: MULTI_TOUCH\n        DeviceType: TOUCH_SCREEN\n        OrientationAware: true\n        Orientation: Rotation0\n      Raw Touch Axes:\n        X: min=0, max=32767, flat=0, fuzz=0, resolution=0\n        Y: min=0, max=32767, flat=0, fuzz=0, resolution=0\n      Affine Transformation:\n        X scale: 1.000\n        X ymix: 0.000\n        X offset: 0.000\n        Y xmix: 0.000\n        Y scale: 1.000\n        Y offset: 0.000\n      {viewport}\n      DisplayBounds: {width}x{height}\n"
        )
    }

    fn geometry(panel_size: (u32, u32), rotation: u8) -> LogicalTouchGeometry {
        LogicalTouchGeometry::from_diagnostics(
            panel_size,
            &format!("Physical size: {}x{}\n", panel_size.0, panel_size.1),
            &diagnostics(panel_size, rotation),
        )
        .unwrap()
    }

    #[test]
    fn maps_all_rotations_and_reflected_zero_edges() {
        let size = (1080, 2400);
        let cases = [
            (0, (23, 71), (23, 71), (0, 0)),
            (1, (23, 71), (1009, 23), (1080, 0)),
            (2, (23, 71), (1057, 2329), (1080, 2400)),
            (3, (23, 71), (71, 2377), (0, 2400)),
        ];
        for (rotation, logical, panel, origin) in cases {
            let geometry = geometry(size, rotation);
            assert_eq!(geometry.rotation(), rotation);
            assert_eq!(geometry.panel_point(logical.0, logical.1), Ok(panel));
            assert_eq!(geometry.panel_point(0, 0), Ok(origin));
            let (width, height) = geometry.logical_size();
            assert!(geometry.panel_point(width, 0).is_err());
            assert!(geometry.panel_point(0, height).is_err());
            assert!(geometry.panel_point(width - 1, height - 1).is_ok());
        }
    }

    /// Independent forward model from QEMU's integer scaleAxis and Android's
    /// discrete rotation plus raw-range scaling. Numerators use denominator
    /// 32768 so the assertion does not hide rounding errors in floating point.
    fn source_forward_numerators(geometry: LogicalTouchGeometry, x: u32, y: u32) -> (i64, i64) {
        let width = i64::from(geometry.panel_width);
        let height = i64::from(geometry.panel_height);
        let raw_x = 32767 * i64::from(x) / width;
        let raw_y = 32767 * i64::from(y) / height;
        match geometry.rotation {
            0 => (width * raw_x, height * raw_y),
            1 => (height * raw_y, width * (32767 - raw_x)),
            2 => (width * (32767 - raw_x), height * (32767 - raw_y)),
            3 => (height * (32767 - raw_y), width * raw_x),
            _ => panic!("test rotation must be supported"),
        }
    }

    #[test]
    fn source_model_round_trips_every_axis_coordinate_with_subpixel_error() {
        // Both natural orientations, awkward non-divisor sizes, and the full
        // accepted dimension limit exercise quantization as well as rotation.
        for size in [
            (1080, 2400),
            (2400, 1080),
            (997, 2003),
            (4096, 4095),
            (1, 1),
        ] {
            for rotation in 0..=3 {
                let geometry = geometry(size, rotation);
                let (width, height) = geometry.logical_size();
                let samples = (0..width)
                    .map(|x| (x, height / 2))
                    .chain((0..height).map(|y| (width / 2, y)))
                    .chain([(0, 0), (width - 1, height - 1)]);
                for (x, y) in samples {
                    let panel = geometry.panel_point(x, y).unwrap();
                    assert!(panel.0 <= size.0 && panel.1 <= size.1);
                    let actual = source_forward_numerators(geometry, panel.0, panel.1);
                    assert!((actual.0 - i64::from(x) * 32768).abs() < 8192);
                    assert!((actual.1 - i64::from(y) * 32768).abs() < 8192);
                    assert_eq!((actual.0 + 16384) / 32768, i64::from(x));
                    assert_eq!((actual.1 + 16384) / 32768, i64::from(y));
                }
            }
        }
    }

    #[test]
    fn requires_explicit_zero_input_orientation_in_known_source_spellings() {
        // Android 15 ui/Rotation.h names the enumerator Rotation0; ROTATION_0
        // is its constexpr alias, used by ui::toCString instead of reflection.
        // https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-15.0.0_r1/libs/ui/include/ui/Rotation.h
        // https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-15.0.0_r1/include/ftl/enum.h
        let input = diagnostics((1080, 2400), 1);
        let parse = |input: &str| {
            LogicalTouchGeometry::from_diagnostics((1080, 2400), "Physical size: 1080x2400", input)
        };
        assert!(parse(&input).is_ok());
        assert!(parse(&input.replace("Orientation: Rotation0", "Orientation: ROTATION_0")).is_ok());
        assert!(parse(&input.replace("Orientation: Rotation0", "")).is_err());
        for value in [
            "",
            "0",
            "Rotation90",
            "Rotation180",
            "Rotation270",
            "ROTATION_90",
            "ROTATION_180",
            "ROTATION_270",
            "ROTATION_1",
            "unknown",
        ] {
            assert!(
                parse(&input.replace("Orientation: Rotation0", &format!("Orientation: {value}")))
                    .is_err(),
                "accepted unsupported input orientation {value:?}",
            );
        }
    }

    #[test]
    fn rejects_unqualified_geometry_and_missing_mapper_facts() {
        let input = diagnostics((1080, 2400), 1);
        let wm = "Physical size: 1080x2400\n";
        let parse = |input: &str| LogicalTouchGeometry::from_diagnostics((1080, 2400), wm, input);
        assert!(parse(&input).is_ok());
        for (before, after) in [
            (
                "physicalFrame=[0, 0, 2400, 1080]",
                "physicalFrame=[1, 0, 2400, 1080]",
            ),
            (
                "logicalFrame=[0, 0, 2400, 1080]",
                "logicalFrame=[0, 0, 1200, 540]",
            ),
            ("deviceSize=[2400, 1080]", "deviceSize=[1080, 2400]"),
            ("isActive=[1]", "isActive=[0]"),
            ("orientation=1", "orientation=4"),
            ("Viewport INTERNAL:", "Viewport EXTERNAL:"),
            ("OrientationAware: true", "OrientationAware: false"),
            ("Orientation: Rotation0", "Orientation: ROTATION_90"),
            ("GestureMode: MULTI_TOUCH", "GestureMode: SINGLE_TOUCH"),
            ("DeviceType: TOUCH_SCREEN", "DeviceType: POINTER"),
            ("mode - DIRECT", "mode - POINTER"),
            ("min=0, max=32767", "min=0, max=65535"),
            ("X scale: 1.000", "X scale: 0.950"),
            ("Y offset: 0.000", "Y offset: 1.000"),
            ("Y xmix: 0.000", "Y xmix: NaN"),
            ("X ymix: 0.000", ""),
            ("Raw Touch Axes:", "Missing Axes:"),
            ("Affine Transformation:", "Missing Calibration:"),
        ] {
            assert!(
                parse(&input.replace(before, after)).is_err(),
                "accepted {after}"
            );
        }
        assert!(
            LogicalTouchGeometry::from_diagnostics(
                (1080, 2400),
                &format!("{wm}Override size: 1080x2400\n"),
                &input
            )
            .is_err()
        );
        assert!(
            LogicalTouchGeometry::from_diagnostics((1080, 2400), "Physical size: 720x1600", &input)
                .is_err()
        );
        assert!(LogicalTouchGeometry::from_diagnostics((4097, 1080), wm, &input).is_err());
        assert!(LogicalTouchGeometry::from_diagnostics((0, 2400), wm, &input).is_err());
        assert!(parse("").is_err());
        assert!(parse(&input.replace("displayId=0", "displayId=10")).is_err());
        assert!(parse(&format!("{input}\n{input}")).is_err());
        assert!(parse(&input.replacen("orientation=1", "orientation=3", 1)).is_err());
        assert!(parse(&input.replace("orientation=1", "orientation=1, orientation=1")).is_err());
    }
}
