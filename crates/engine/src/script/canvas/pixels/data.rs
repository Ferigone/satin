//! Branded ImageData and its native typed-array view.
use super::{Exceptions, long};
use crate::script::{arg, getter, webidl};
use rust_browser_javascript::{ErrorKind, JsError, PlatformClone, RootId, Value, Vm};
use std::rc::Rc;

const DATA_SLOT: u32 = 0;
const MAX_IMAGE_DATA_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub(super) fn elements(self, format: PixelFormat) -> Result<usize, JsError> {
        let pixels = u64::from(self.width) * u64::from(self.height);
        if pixels > (MAX_IMAGE_DATA_BYTES / format.bytes_per_pixel()) as u64 {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "ImageData exceeds the 16 MiB pixel budget",
            ));
        }
        Ok(pixels as usize * 4)
    }
}

#[derive(Clone, Copy, Default)]
pub(super) enum PixelFormat {
    #[default]
    Unorm8,
    Float16,
}

impl PixelFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Unorm8 => "rgba-unorm8",
            Self::Float16 => "rgba-float16",
        }
    }

    pub(super) fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Unorm8 => 4,
            Self::Float16 => 8,
        }
    }

    fn accepts(self, vm: &Vm, value: &Value) -> bool {
        match self {
            Self::Unorm8 => vm.is_uint8_clamped_array(value),
            Self::Float16 => vm.is_float16_array(value),
        }
    }
}

#[derive(Clone, Copy, Default)]
enum ColorSpace {
    #[default]
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
}

impl ColorSpace {
    fn name(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::SrgbLinear => "srgb-linear",
            Self::DisplayP3 => "display-p3",
            Self::DisplayP3Linear => "display-p3-linear",
        }
    }

    fn linear(self) -> bool {
        matches!(self, Self::SrgbLinear | Self::DisplayP3Linear)
    }

    fn wide(self) -> bool {
        matches!(self, Self::DisplayP3 | Self::DisplayP3Linear)
    }
}

// D65 RGB matrices derived from the sRGB and Display-P3 primaries in CSS Color 4.
const P3_TO_SRGB: [[f64; 3]; 3] = [
    [1.224_940_176, -0.224_940_176, 0.],
    [-0.042_056_955, 1.042_056_955, 0.],
    [-0.019_637_555, -0.078_636_046, 1.098_273_601],
];
const SRGB_TO_P3: [[f64; 3]; 3] = [
    [0.822_461_969, 0.177_538_031, 0.],
    [0.033_194_199, 0.966_805_801, 0.],
    [0.017_082_631, 0.072_397_441, 0.910_519_928],
];

fn to_linear(value: f64) -> f64 {
    let mag = value.abs();
    let output = if mag <= 0.04045 {
        mag / 12.92
    } else {
        ((mag + 0.055) / 1.055).powf(2.4)
    };
    output.copysign(value)
}

fn from_linear(value: f64) -> f64 {
    let mag = value.abs();
    let output = if mag <= 0.003_130_8 {
        12.92 * mag
    } else {
        1.055 * mag.powf(1. / 2.4) - 0.055
    };
    output.copysign(value)
}

fn multiply(matrix: [[f64; 3]; 3], input: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| row[0] * input[0] + row[1] * input[1] + row[2] * input[2])
}

fn by_channel(rgba: [f64; 4], transform: impl Fn([f64; 3]) -> [f64; 3]) -> [f64; 4] {
    let [r, g, b, a] = rgba;
    let [r, g, b] = transform([r, g, b]);
    [r, g, b, a]
}

fn to_byte(channel: f64) -> u8 {
    (channel.clamp(0., 1.) * 255.).round_ties_even() as u8
}

#[derive(Clone, Copy, Default)]
pub(super) struct Settings {
    space: ColorSpace,
    pub format: PixelFormat,
}

impl Settings {
    pub(super) fn parse(vm: &mut Vm, input: Value) -> Result<Self, JsError> {
        if input.is_nullish() {
            return Ok(Self::default());
        }
        if input.as_object().is_none() {
            return Err(JsError::type_error(
                "ImageData settings must be a dictionary",
            ));
        }
        let space = vm.get_property(input.clone(), "colorSpace")?;
        let space = if space.is_undefined() {
            ColorSpace::default()
        } else {
            match vm.to_string(&space)?.to_string_lossy().as_str() {
                "srgb" => ColorSpace::Srgb,
                "srgb-linear" => ColorSpace::SrgbLinear,
                "display-p3" => ColorSpace::DisplayP3,
                "display-p3-linear" => ColorSpace::DisplayP3Linear,
                _ => return Err(JsError::type_error("Invalid ImageData color space")),
            }
        };
        let format = vm.get_property(input, "pixelFormat")?;
        let format = if format.is_undefined() {
            PixelFormat::default()
        } else {
            match vm.to_string(&format)?.to_string_lossy().as_str() {
                "rgba-unorm8" => PixelFormat::Unorm8,
                "rgba-float16" => PixelFormat::Float16,
                _ => return Err(JsError::type_error("Invalid ImageData pixel format")),
            }
        };
        Ok(Self { space, format })
    }

    pub(super) fn direct(self) -> bool {
        matches!(self.space, ColorSpace::Srgb) && matches!(self.format, PixelFormat::Unorm8)
    }

    pub(super) fn encode_srgb(self, pixel: [u8; 4], destination: &mut [u8]) {
        if self.direct() {
            destination.copy_from_slice(&pixel);
            return;
        }
        let mut channels = pixel.map(|value| f64::from(value) / 255.);
        if !matches!(self.space, ColorSpace::Srgb) {
            channels = by_channel(channels, |rgb| {
                let mut rgb = rgb.map(to_linear);
                if self.space.wide() {
                    rgb = multiply(SRGB_TO_P3, rgb);
                }
                if !self.space.linear() {
                    rgb = rgb.map(from_linear);
                }
                rgb
            });
        }
        match self.format {
            PixelFormat::Unorm8 => destination.copy_from_slice(&channels.map(to_byte)),
            PixelFormat::Float16 => {
                for (bytes, channel) in destination.as_chunks_mut::<2>().0.iter_mut().zip(channels)
                {
                    *bytes = Vm::encode_float16(channel);
                }
            }
        }
    }

    pub(super) fn decode_srgb(self, source: &[u8]) -> [u8; 4] {
        if self.direct() {
            return source.try_into().expect("RGBA8 pixel");
        }
        let mut rgba = [0.; 4];
        match self.format {
            PixelFormat::Unorm8 => {
                for (channel, byte) in rgba.iter_mut().zip(source) {
                    *channel = f64::from(*byte) / 255.;
                }
            }
            PixelFormat::Float16 => {
                for (channel, bytes) in rgba.iter_mut().zip(source.as_chunks::<2>().0) {
                    let number = Vm::decode_float16(*bytes);
                    *channel = if number.is_nan() {
                        0.
                    } else {
                        number.clamp(-65_504., 65_504.)
                    };
                }
            }
        }
        if !matches!(self.space, ColorSpace::Srgb) {
            rgba = by_channel(rgba, |mut rgb| {
                if !self.space.linear() {
                    rgb = rgb.map(to_linear);
                }
                if self.space.wide() {
                    rgb = multiply(P3_TO_SRGB, rgb);
                }
                rgb.map(from_linear)
            });
        }
        rgba.map(to_byte)
    }
}

pub(super) struct Record {
    pub size: Size,
    pub settings: Settings,
}

pub(super) fn receiver(vm: &Vm, object: &Value) -> Result<Rc<Record>, JsError> {
    vm.host_data(object)
        .ok_or_else(|| JsError::type_error("Expected an ImageData object"))
}

pub(super) fn view(vm: &Vm, object: &Value) -> Value {
    vm.host_slot(object, DATA_SLOT)
        .expect("ImageData data slot")
}

fn unsigned_long(vm: &mut Vm, value: &Value) -> Result<u32, JsError> {
    let number = vm.to_number(value)?;
    Ok(if number.is_finite() {
        number.trunc().rem_euclid(4_294_967_296.) as u32
    } else {
        0
    })
}

#[derive(Clone, Copy)]
pub(in crate::script) struct Factory {
    pub(super) errors: Exceptions,
    prototype: RootId,
}

impl Factory {
    pub(super) fn install(vm: &mut Vm, errors: Exceptions) -> Result<Self, JsError> {
        let prototype = vm.new_object();
        let factory = Self {
            errors,
            prototype: vm.root(prototype.clone()),
        };
        webidl::interface(
            vm,
            prototype.clone(),
            "ImageData",
            2,
            move |vm, _, args, target| factory.construct(vm, args, target),
        )?;
        for key in ["width", "height", "data", "colorSpace", "pixelFormat"] {
            getter(vm, prototype.clone(), key, move |vm, object, _| {
                let data = receiver(vm, &object)?;
                Ok(match key {
                    "width" => data.size.width.into(),
                    "height" => data.size.height.into(),
                    "data" => view(vm, &object),
                    "colorSpace" => data.settings.space.name().into(),
                    _ => data.settings.format.name().into(),
                })
            })?;
        }
        Ok(factory)
    }

    pub(super) fn size(self, vm: &mut Vm, width: u32, height: u32) -> Result<Size, JsError> {
        if width == 0 || height == 0 {
            return Err(self.index_error(vm));
        }
        Ok(Size { width, height })
    }

    pub(in crate::script) fn clone_image_data(
        self,
        vm: &mut Vm,
        source: &Value,
    ) -> Result<Option<PlatformClone>, JsError> {
        let Some(record) = vm.host_data::<Record>(source) else {
            return Ok(None);
        };
        // Filling the slot in the VM graph preserves aliases even when the
        // caller separately includes this typed array or transfers its buffer.
        let object = self.materialize(vm, record.size, record.settings, Value::Undefined, None)?;
        Ok(Some(PlatformClone {
            object,
            slots: vec![(DATA_SLOT, view(vm, source))],
        }))
    }

    fn index_error(self, vm: &mut Vm) -> JsError {
        self.errors
            .error(vm, "IndexSizeError", "Invalid ImageData dimensions")
    }

    fn materialize(
        self,
        vm: &mut Vm,
        size: Size,
        settings: Settings,
        data: Value,
        target: Option<Value>,
    ) -> Result<Value, JsError> {
        let prototype = vm
            .root_value(self.prototype)
            .expect("rooted ImageData prototype");
        let object = if let Some(target) = target {
            webidl::instance(vm, target, &prototype)?
        } else {
            vm.new_object_with_prototype(prototype)?
        };
        vm.set_host_slot(object.clone(), DATA_SLOT, data)?;
        vm.set_host_data(object.clone(), Rc::new(Record { size, settings }))?;
        Ok(object)
    }

    fn blank_data(
        self,
        vm: &mut Vm,
        size: Size,
        settings: Settings,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<Value, JsError> {
        let elements = size.elements(settings.format)?;
        vm.charge_work(elements.div_ceil(32))?;
        match settings.format {
            PixelFormat::Unorm8 => vm.new_uint8_clamped_array_with(elements, fill),
            PixelFormat::Float16 => vm.new_float16_array_with(elements, fill),
        }
    }

    pub(super) fn blank_with(
        self,
        vm: &mut Vm,
        size: Size,
        settings: Settings,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<Value, JsError> {
        let data = self.blank_data(vm, size, settings, fill)?;
        self.materialize(vm, size, settings, data, None)
    }

    pub(super) fn create(self, vm: &mut Vm, args: &[Value]) -> Result<Value, JsError> {
        webidl::required(args, 1, "createImageData")?;
        let (size, settings) = if args.len() == 1 {
            let other = receiver(vm, &args[0])?;
            (other.size, other.settings)
        } else {
            let width = long(vm, &args[0])?.unsigned_abs() as u32;
            let height = long(vm, &args[1])?.unsigned_abs() as u32;
            let settings = Settings::parse(vm, arg(args, 2))?;
            (self.size(vm, width, height)?, settings)
        };
        self.blank_with(vm, size, settings, |_| {})
    }

    fn construct(
        self,
        vm: &mut Vm,
        args: &[Value],
        target: Option<Value>,
    ) -> Result<Value, JsError> {
        let target = target.ok_or_else(|| JsError::type_error("ImageData requires new"))?;
        webidl::required(args, 2, "ImageData")?;
        let first = &args[0];
        let view = if args.len() >= 4
            || (args.len() >= 3 && vm.is_buffer_source(first))
            || vm.is_uint8_clamped_array(first)
            || vm.is_float16_array(first)
        {
            if !vm.is_uint8_clamped_array(first) && !vm.is_float16_array(first) {
                return Err(JsError::type_error("Expected an ImageDataArray"));
            }
            // Reject resizable backing during Web IDL conversion. The view
            // may still be detached by a later argument conversion.
            vm.fixed_buffer_source_bytes(first)?;
            Some(first.clone())
        } else {
            None
        };
        let (size, settings, data) = if let Some(view) = view {
            let width = unsigned_long(vm, &args[1])?;
            let height = args
                .get(2)
                .filter(|value| !value.is_undefined())
                .map(|value| unsigned_long(vm, value))
                .transpose()?;
            let settings = Settings::parse(vm, arg(args, 3))?;
            if !settings.format.accepts(vm, &view) {
                return Err(self.invalid_view(vm));
            }
            let bytes = vm.fixed_buffer_source_bytes(&view)?.len();
            let stride = settings.format.bytes_per_pixel();
            if bytes == 0 || !bytes.is_multiple_of(stride) {
                return Err(self.invalid_view(vm));
            }
            let pixels = bytes / stride;
            if width == 0 || !pixels.is_multiple_of(width as usize) {
                return Err(self.index_error(vm));
            }
            let actual_height = (pixels / width as usize) as u32;
            if height.is_some_and(|value| value != actual_height) {
                return Err(self.index_error(vm));
            }
            let size = Size {
                width,
                height: actual_height,
            };
            size.elements(settings.format)?;
            (size, settings, view)
        } else {
            let width = unsigned_long(vm, first)?;
            let height = unsigned_long(vm, &args[1])?;
            let settings = Settings::parse(vm, arg(args, 2))?;
            let size = self.size(vm, width, height)?;
            let data = self.blank_data(vm, size, settings, |_| {})?;
            (size, settings, data)
        };
        self.materialize(vm, size, settings, data, Some(target))
    }

    fn invalid_view(self, vm: &mut Vm) -> JsError {
        self.errors
            .error(vm, "InvalidStateError", "Invalid ImageData backing view")
    }
}
