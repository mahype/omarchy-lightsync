//! Safe frame sampling and color processing for entertainment channels.

use std::collections::{BTreeMap, HashSet};

use lightsync_domain::{Brightness, EntertainmentChannel, Intensity, NormalizedPoint, SyncMode};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb8,
    Rgba8,
    Rgbx8,
    Bgra8,
    Bgrx8,
}

#[derive(Debug, Clone, Copy)]
pub struct FrameView<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    stride: i32,
    format: PixelFormat,
}

impl<'a> FrameView<'a> {
    pub fn new(
        data: &'a [u8],
        width: usize,
        height: usize,
        stride: i32,
        format: PixelFormat,
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError::EmptyDimensions);
        }
        let bytes_per_pixel = if format == PixelFormat::Rgb8 { 3 } else { 4 };
        let row_bytes = width
            .checked_mul(bytes_per_pixel)
            .ok_or(FrameError::DimensionsOverflow)?;
        let input_stride = stride;
        let absolute_stride = stride
            .checked_abs()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(FrameError::DimensionsOverflow)?;
        if absolute_stride < row_bytes {
            return Err(FrameError::StrideTooSmall {
                stride: absolute_stride,
                row_bytes,
            });
        }
        let required = (height - 1)
            .checked_mul(absolute_stride)
            .and_then(|offset| offset.checked_add(row_bytes))
            .ok_or(FrameError::DimensionsOverflow)?;
        if data.len() < required {
            return Err(FrameError::BufferTooSmall {
                actual: data.len(),
                required,
            });
        }
        Ok(Self {
            data,
            width,
            height,
            stride: input_stride,
            format,
        })
    }

    pub const fn width(&self) -> usize {
        self.width
    }

    pub const fn height(&self) -> usize {
        self.height
    }

    pub const fn stride(&self) -> i32 {
        self.stride
    }

    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn sample(&self, point: NormalizedPoint, radius: u32) -> LinearRgb {
        let center_x = (point.x() * (self.width - 1) as f32).round() as usize;
        let center_y = (point.y() * (self.height - 1) as f32).round() as usize;
        let radius = radius as usize;
        let min_x = center_x.saturating_sub(radius);
        let max_x = center_x.saturating_add(radius).min(self.width - 1);
        let min_y = center_y.saturating_sub(radius);
        let max_y = center_y.saturating_add(radius).min(self.height - 1);

        let mut total = LinearRgb::BLACK;
        let mut count = 0_u32;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                total += self.pixel_linear(x, y);
                count += 1;
            }
        }
        total / count as f32
    }

    fn pixel_linear(&self, x: usize, y: usize) -> LinearRgb {
        let stride = self.stride.unsigned_abs() as usize;
        let stored_y = if self.stride < 0 {
            self.height - 1 - y
        } else {
            y
        };
        let bytes_per_pixel = if self.format == PixelFormat::Rgb8 {
            3
        } else {
            4
        };
        let offset = stored_y * stride + x * bytes_per_pixel;
        let pixel = &self.data[offset..offset + bytes_per_pixel];
        let (red, green, blue) = match self.format {
            PixelFormat::Rgb8 | PixelFormat::Rgba8 | PixelFormat::Rgbx8 => {
                (pixel[0], pixel[1], pixel[2])
            }
            PixelFormat::Bgra8 | PixelFormat::Bgrx8 => (pixel[2], pixel[1], pixel[0]),
        };
        LinearRgb {
            red: srgb8_to_linear(red),
            green: srgb8_to_linear(green),
            blue: srgb8_to_linear(blue),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameError {
    #[error("frame width and height must be non-zero")]
    EmptyDimensions,
    #[error("frame dimensions overflow addressable memory")]
    DimensionsOverflow,
    #[error("stride {stride} is smaller than packed row size {row_bytes}")]
    StrideTooSmall { stride: usize, row_bytes: usize },
    #[error("frame buffer has {actual} bytes but at least {required} are required")]
    BufferTooSmall { actual: usize, required: usize },
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LinearRgb {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

impl LinearRgb {
    pub const BLACK: Self = Self {
        red: 0.0,
        green: 0.0,
        blue: 0.0,
    };

    fn max_component(self) -> f32 {
        self.red.max(self.green).max(self.blue)
    }

    fn scale(self, factor: f32) -> Self {
        Self {
            red: (self.red * factor).clamp(0.0, 1.0),
            green: (self.green * factor).clamp(0.0, 1.0),
            blue: (self.blue * factor).clamp(0.0, 1.0),
        }
    }
}

impl std::ops::AddAssign for LinearRgb {
    fn add_assign(&mut self, rhs: Self) {
        self.red += rhs.red;
        self.green += rhs.green;
        self.blue += rhs.blue;
    }
}

impl std::ops::Div<f32> for LinearRgb {
    type Output = Self;

    fn div(self, rhs: f32) -> Self::Output {
        Self {
            red: self.red / rhs,
            green: self.green / rhs,
            blue: self.blue / rhs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb16 {
    pub red: u16,
    pub green: u16,
    pub blue: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PipelinePreset {
    /// Exposure compensation in stops.
    pub exposure: f32,
    /// Values at or below this linear-light level become black.
    pub black_cutoff: f32,
    /// Square sampling radius in pixels.
    pub sample_radius: u32,
    /// Per-frame interpolation for increasing components, in `0.0..=1.0`.
    pub attack: f32,
    /// Per-frame interpolation for decreasing components, in `0.0..=1.0`.
    pub release: f32,
}

impl PipelinePreset {
    pub fn for_mode(mode: SyncMode, intensity: Intensity) -> Self {
        let (mode_exposure, black_cutoff, base_radius) = match mode {
            SyncMode::Video => (0.0, 0.006, 2),
            SyncMode::Game => (0.15, 0.003, 1),
            SyncMode::Music => (0.3, 0.01, 3),
            SyncMode::Scene => (-0.1, 0.008, 4),
        };
        let (exposure_delta, attack, release, radius_delta) = match intensity {
            Intensity::Subtle => (-0.25, 0.28, 0.12, 2),
            Intensity::Moderate => (0.0, 0.48, 0.2, 1),
            Intensity::High => (0.2, 0.72, 0.34, 0),
            Intensity::Extreme => (0.4, 1.0, 0.52, 0),
        };
        Self {
            exposure: mode_exposure + exposure_delta,
            black_cutoff,
            sample_radius: base_radius + radius_delta,
            attack,
            release,
        }
    }

    pub fn validate(self) -> Result<Self, ProcessorError> {
        if !self.exposure.is_finite()
            || !self.black_cutoff.is_finite()
            || !(0.0..=1.0).contains(&self.black_cutoff)
            || !self.attack.is_finite()
            || !(0.0..=1.0).contains(&self.attack)
            || !self.release.is_finite()
            || !(0.0..=1.0).contains(&self.release)
        {
            return Err(ProcessorError::InvalidPreset);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelColor {
    pub channel_id: u16,
    pub color: Rgb16,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessorError {
    #[error("channel id {0} appears more than once")]
    DuplicateChannel(u16),
    #[error("pipeline preset contains a non-finite or out-of-range value")]
    InvalidPreset,
}

#[derive(Debug, Default)]
pub struct ColorProcessor {
    channels: Vec<EntertainmentChannel>,
    previous: BTreeMap<u16, LinearRgb>,
}

impl ColorProcessor {
    pub fn new(channels: Vec<EntertainmentChannel>) -> Result<Self, ProcessorError> {
        validate_channels(&channels)?;
        Ok(Self {
            channels,
            previous: BTreeMap::new(),
        })
    }

    pub fn channels(&self) -> &[EntertainmentChannel] {
        &self.channels
    }

    pub fn set_channels(
        &mut self,
        channels: Vec<EntertainmentChannel>,
    ) -> Result<(), ProcessorError> {
        validate_channels(&channels)?;
        let ids: HashSet<_> = channels.iter().map(|channel| channel.id).collect();
        self.previous.retain(|id, _| ids.contains(id));
        self.channels = channels;
        Ok(())
    }

    pub fn reset_smoothing(&mut self) {
        self.previous.clear();
    }

    pub fn process(
        &mut self,
        frame: &FrameView<'_>,
        mode: SyncMode,
        intensity: Intensity,
        brightness: Brightness,
    ) -> Result<Vec<ChannelColor>, ProcessorError> {
        self.process_with_preset(frame, PipelinePreset::for_mode(mode, intensity), brightness)
    }

    pub fn process_with_preset(
        &mut self,
        frame: &FrameView<'_>,
        preset: PipelinePreset,
        brightness: Brightness,
    ) -> Result<Vec<ChannelColor>, ProcessorError> {
        let preset = preset.validate()?;
        let gain = 2.0_f32.powf(preset.exposure) * brightness.factor();
        let mut output = Vec::with_capacity(self.channels.len());
        for channel in &self.channels {
            let sampled = frame.sample(channel.position, preset.sample_radius);
            let mut target = sampled.scale(gain);
            if target.max_component() <= preset.black_cutoff {
                target = LinearRgb::BLACK;
            }
            let previous = self
                .previous
                .get(&channel.id)
                .copied()
                .unwrap_or(LinearRgb::BLACK);
            let smoothed = smooth(previous, target, preset.attack, preset.release);
            self.previous.insert(channel.id, smoothed);
            output.push(ChannelColor {
                channel_id: channel.id,
                color: linear_to_rgb16(smoothed),
            });
        }
        Ok(output)
    }
}

fn validate_channels(channels: &[EntertainmentChannel]) -> Result<(), ProcessorError> {
    let mut ids = HashSet::new();
    for channel in channels {
        if !ids.insert(channel.id) {
            return Err(ProcessorError::DuplicateChannel(channel.id));
        }
    }
    Ok(())
}

fn smooth(previous: LinearRgb, target: LinearRgb, attack: f32, release: f32) -> LinearRgb {
    fn component(previous: f32, target: f32, attack: f32, release: f32) -> f32 {
        let coefficient = if target >= previous { attack } else { release };
        previous + (target - previous) * coefficient
    }
    LinearRgb {
        red: component(previous.red, target.red, attack, release),
        green: component(previous.green, target.green, attack, release),
        blue: component(previous.blue, target.blue, attack, release),
    }
}

fn srgb8_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_rgb16(value: LinearRgb) -> Rgb16 {
    fn component(value: f32) -> u16 {
        let value = value.clamp(0.0, 1.0);
        let srgb = if value <= 0.003_130_8 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        (srgb * f32::from(u16::MAX)).round() as u16
    }
    Rgb16 {
        red: component(value.red),
        green: component(value.green),
        blue: component(value.blue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32) -> NormalizedPoint {
        NormalizedPoint::new(x, y).expect("valid point")
    }

    fn channel(id: u16, x: f32, y: f32) -> EntertainmentChannel {
        EntertainmentChannel {
            id,
            name: None,
            position: point(x, y),
        }
    }

    fn immediate(radius: u32) -> PipelinePreset {
        PipelinePreset {
            exposure: 0.0,
            black_cutoff: 0.0,
            sample_radius: radius,
            attack: 1.0,
            release: 1.0,
        }
    }

    #[test]
    fn reads_rgb_and_bgr_four_channel_formats() {
        let rgb = [255, 128, 0, 7];
        let bgr = [0, 128, 255, 7];
        for format in [PixelFormat::Rgba8, PixelFormat::Rgbx8] {
            let frame = FrameView::new(&rgb, 1, 1, 4, format).expect("RGB frame");
            let color = frame.sample(point(0.0, 0.0), 0);
            assert_eq!(color.red, 1.0);
            assert_eq!(color.blue, 0.0);
        }
        for format in [PixelFormat::Bgra8, PixelFormat::Bgrx8] {
            let frame = FrameView::new(&bgr, 1, 1, 4, format).expect("BGR frame");
            let color = frame.sample(point(0.0, 0.0), 0);
            assert_eq!(color.red, 1.0);
            assert_eq!(color.blue, 0.0);
        }
    }

    #[test]
    fn validates_stride_buffer_and_edge_sampling() {
        assert!(matches!(
            FrameView::new(&[0; 8], 2, 1, 7, PixelFormat::Rgba8),
            Err(FrameError::StrideTooSmall { .. })
        ));
        assert!(matches!(
            FrameView::new(&[0; 11], 1, 2, 8, PixelFormat::Rgba8),
            Err(FrameError::BufferTooSmall { .. })
        ));
        let pixels = [0, 0, 0, 0, 255, 255, 255, 0];
        let frame = FrameView::new(&pixels, 2, 1, 8, PixelFormat::Rgba8).expect("frame");
        assert_eq!(
            frame.sample(point(1.0, 1.0), 99),
            LinearRgb {
                red: 0.5,
                green: 0.5,
                blue: 0.5
            }
        );
    }

    #[test]
    fn brightness_is_applied_in_linear_light() {
        let pixel = [255, 255, 255, 0];
        let frame = FrameView::new(&pixel, 1, 1, 4, PixelFormat::Rgba8).expect("frame");
        let mut processor = ColorProcessor::new(vec![channel(1, 0.0, 0.0)]).expect("processor");
        let colors = processor
            .process_with_preset(
                &frame,
                immediate(0),
                Brightness::new(50).expect("brightness"),
            )
            .expect("process");
        assert!((48_150..=48_250).contains(&colors[0].color.red));
    }

    #[test]
    fn black_cutoff_suppresses_dark_samples() {
        let pixel = [20, 20, 20, 0];
        let frame = FrameView::new(&pixel, 1, 1, 4, PixelFormat::Rgba8).expect("frame");
        let mut processor = ColorProcessor::new(vec![channel(1, 0.0, 0.0)]).expect("processor");
        let colors = processor
            .process_with_preset(
                &frame,
                PipelinePreset {
                    black_cutoff: 0.01,
                    ..immediate(0)
                },
                Brightness::new(100).expect("brightness"),
            )
            .expect("process");
        assert_eq!(
            colors[0].color,
            Rgb16 {
                red: 0,
                green: 0,
                blue: 0
            }
        );
    }

    #[test]
    fn attack_and_release_smooth_in_linear_light() {
        let white = [255, 255, 255, 0];
        let black = [0, 0, 0, 0];
        let white_frame = FrameView::new(&white, 1, 1, 4, PixelFormat::Rgba8).expect("frame");
        let black_frame = FrameView::new(&black, 1, 1, 4, PixelFormat::Rgba8).expect("frame");
        let preset = PipelinePreset {
            attack: 0.5,
            release: 0.25,
            ..immediate(0)
        };
        let mut processor = ColorProcessor::new(vec![channel(1, 0.0, 0.0)]).expect("processor");
        let first = processor
            .process_with_preset(
                &white_frame,
                preset,
                Brightness::new(100).expect("brightness"),
            )
            .expect("white");
        let second = processor
            .process_with_preset(
                &black_frame,
                preset,
                Brightness::new(100).expect("brightness"),
            )
            .expect("black");
        assert!((48_150..=48_250).contains(&first[0].color.red));
        assert!((42_300..=42_500).contains(&second[0].color.red));
    }

    #[test]
    fn maps_channel_positions_and_preserves_channel_order() {
        let pixels = [255, 0, 0, 0, 0, 0, 255, 0];
        let frame = FrameView::new(&pixels, 2, 1, 8, PixelFormat::Rgba8).expect("frame");
        let mut processor = ColorProcessor::new(vec![channel(9, 1.0, 0.0), channel(3, 0.0, 0.0)])
            .expect("processor");
        let colors = processor
            .process_with_preset(
                &frame,
                immediate(0),
                Brightness::new(100).expect("brightness"),
            )
            .expect("process");
        assert_eq!(
            colors[0],
            ChannelColor {
                channel_id: 9,
                color: Rgb16 {
                    red: 0,
                    green: 0,
                    blue: u16::MAX
                }
            }
        );
        assert_eq!(
            colors[1],
            ChannelColor {
                channel_id: 3,
                color: Rgb16 {
                    red: u16::MAX,
                    green: 0,
                    blue: 0
                }
            }
        );
    }
}
