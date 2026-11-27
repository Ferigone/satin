//! Vello renderer for Satin's document display list.
use crate::{
    Layout,
    canvas::Frame,
    css::Color,
    fonts::Run,
    layout::{Affine2D, DrawCommand, Rect, ScrollOffset},
    paint::Canvas,
};
use image::RgbaImage;
use std::{collections::HashMap, sync::Arc, time::Duration};
use vello::{
    Glyph, RenderParams, Renderer, RendererOptions, Scene,
    kurbo::{Affine, BezPath, Rect as ShapeRect, Vec2},
    peniko::{Blob, Fill, FontData, ImageAlphaType, ImageData, ImageFormat},
    util::{DeviceHandle, RenderContext},
    wgpu,
};

pub struct VelloRenderer {
    context: RenderContext,
    device_id: usize,
    renderer: Renderer,
    target: Option<Target>,
}

struct Target {
    size: wgpu::Extent3d,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    row_stride: u32,
}

impl Target {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self, String> {
        let limits = device.limits();
        if width == 0 || height == 0 || width.max(height) > limits.max_texture_dimension_2d {
            return Err("Document texture exceeds the Vello device limits".into());
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let row_stride = (width * 4).div_ceil(alignment) * alignment;
        let buffer_size = u64::from(row_stride) * u64::from(height);
        if buffer_size > limits.max_buffer_size {
            return Err("Document readback exceeds the Vello device limits".into());
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Satin document"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Satin document readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Ok(Self {
            size,
            texture,
            view,
            readback,
            row_stride,
        })
    }

    fn copy_pixels(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: &mut [u32],
    ) -> Result<(), String> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Read Satin document"),
        });
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.row_stride),
                    rows_per_image: None,
                },
            },
            self.size,
        );
        let submission = queue.submit([encoder.finish()]);
        let slice = self.readback.slice(..);
        let (send, receive) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
        let mapped = device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(5)),
            })
            .map_err(|error| error.to_string())
            .and_then(|_| receive.try_recv().map_err(|error| error.to_string()))
            .and_then(|result| result.map_err(|error| error.to_string()));
        if let Err(error) = mapped {
            self.readback.unmap();
            return Err(error);
        }
        {
            let bytes = slice.get_mapped_range();
            let width = self.size.width as usize;
            for (output, row) in pixels
                .chunks_exact_mut(width)
                .zip(bytes.chunks_exact(self.row_stride as usize))
            {
                for (pixel, rgba) in output.iter_mut().zip(row.chunks_exact(4)) {
                    *pixel = u32::from_be_bytes([0, rgba[0], rgba[1], rgba[2]]);
                }
            }
        }
        self.readback.unmap();
        Ok(())
    }
}

impl VelloRenderer {
    pub fn new() -> Result<Self, String> {
        let mut context = RenderContext::new();
        let device_id = pollster::block_on(context.device(None))
            .ok_or("No compatible wgpu adapter for Vello")?;
        let renderer = Renderer::new(
            &context.devices[device_id].device,
            RendererOptions::default(),
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            context,
            device_id,
            renderer,
            target: None,
        })
    }

    pub fn document(
        &mut self,
        canvas: &mut Canvas,
        layout: &Layout,
        viewport: Rect,
        scroll: ScrollOffset,
        scale: f32,
    ) -> Result<(), String> {
        let scene = document_scene(layout, viewport, scroll, scale)?;
        let DeviceHandle { device, queue, .. } = &self.context.devices[self.device_id];
        if self.target.as_ref().is_none_or(|target| {
            (target.size.width, target.size.height) != (canvas.width, canvas.height)
        }) {
            self.target = Some(Target::new(device, canvas.width, canvas.height)?);
        }
        let target = self.target.as_ref().expect("document target initialized");
        let Color(red, green, blue, _) = layout.background;
        self.renderer
            .render_to_texture(
                device,
                queue,
                &scene,
                &target.view,
                &RenderParams {
                    base_color: vello::peniko::Color::from_rgba8(red, green, blue, 255),
                    width: canvas.width,
                    height: canvas.height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|error| error.to_string())?;
        target.copy_pixels(device, queue, &mut canvas.pixels)
    }
}

fn shape_rect(rect: Rect) -> ShapeRect {
    ShapeRect::new(
        f64::from(rect.x),
        f64::from(rect.y),
        f64::from(rect.x + rect.width),
        f64::from(rect.y + rect.height),
    )
}

fn transform(value: Affine2D) -> Affine {
    Affine::new([value.a, value.b, value.c, value.d, value.e, value.f].map(f64::from))
}

fn ink(Color(red, green, blue, alpha): Color) -> vello::peniko::Color {
    vello::peniko::Color::from_rgba8(red, green, blue, alpha)
}

fn fill_rect(scene: &mut Scene, matrix: Affine, rect: ShapeRect, color: Color) {
    if color.3 > 0 && rect.width() > 0. && rect.height() > 0. && rect.is_finite() {
        scene.fill(Fill::NonZero, matrix, ink(color), None, &rect);
    }
}

fn rounded(rect: ShapeRect, radii: [[f32; 2]; 4]) -> BezPath {
    let [tl, tr, br, bl] = radii.map(|pair| pair.map(f64::from));
    let ShapeRect { x0, y0, x1, y1 } = rect;
    let k = 0.552_284_749_830_793_6;
    let mut path = BezPath::new();
    path.move_to((x0 + tl[0], y0));
    path.line_to((x1 - tr[0], y0));
    path.curve_to(
        (x1 - tr[0] * (1. - k), y0),
        (x1, y0 + tr[1] * (1. - k)),
        (x1, y0 + tr[1]),
    );
    path.line_to((x1, y1 - br[1]));
    path.curve_to(
        (x1, y1 - br[1] * (1. - k)),
        (x1 - br[0] * (1. - k), y1),
        (x1 - br[0], y1),
    );
    path.line_to((x0 + bl[0], y1));
    path.curve_to(
        (x0 + bl[0] * (1. - k), y1),
        (x0, y1 - bl[1] * (1. - k)),
        (x0, y1 - bl[1]),
    );
    path.line_to((x0, y0 + tl[1]));
    path.curve_to(
        (x0, y0 + tl[1] * (1. - k)),
        (x0 + tl[0] * (1. - k), y0),
        (x0 + tl[0], y0),
    );
    path.close_path();
    path
}

fn uniform_border(
    scene: &mut Scene,
    matrix: Affine,
    rect: ShapeRect,
    widths: [f32; 4],
    color: Color,
) {
    let [top, right, bottom, left] = widths.map(f64::from);
    let ShapeRect { x0, y0, x1, y1 } = rect;
    let inner_x = (x0 + left).min(x1);
    let inner_right = (x1 - right).max(inner_x);
    let inner_y = (y0 + top).min(y1);
    let inner_bottom = (y1 - bottom).max(inner_y);
    for strip in [
        ShapeRect::new(x0, y0, x1, inner_y),
        ShapeRect::new(x0, inner_y, inner_x, inner_bottom),
        ShapeRect::new(inner_right, inner_y, x1, inner_bottom),
        ShapeRect::new(x0, inner_bottom, x1, y1),
    ] {
        fill_rect(scene, matrix, strip, color);
    }
}

fn rounded_border(
    scene: &mut Scene,
    matrix: Affine,
    outer: ShapeRect,
    radii: [[f32; 2]; 4],
    widths: [f32; 4],
    colors: [Color; 4],
) {
    if outer.width() <= 0. || outer.height() <= 0. || !outer.is_finite() {
        return;
    }
    let [top, right, bottom, left] = widths;
    let x = (outer.x0 + f64::from(left)).min(outer.x1);
    let y = (outer.y0 + f64::from(top)).min(outer.y1);
    let inner = ShapeRect::new(
        x, y,
        (outer.x1 - f64::from(right)).max(x),
        (outer.y1 - f64::from(bottom)).max(y),
    );
    let mut path = rounded(outer, radii);
    if inner.width() > 0. && inner.height() > 0. {
        let mut inner_radii = radii;
        for (corner, [horizontal, vertical]) in inner_radii.iter_mut()
            .zip([[left, top], [right, top], [right, bottom], [left, bottom]])
        {
            corner[0] = (corner[0] - horizontal).max(0.);
            corner[1] = (corner[1] - vertical).max(0.);
        }
        path.extend(rounded(inner, inner_radii).elements().iter().copied());
    }
    if colors.iter().all(|color| *color == colors[0]) {
        if colors[0].3 > 0 {
            scene.fill(Fill::EvenOdd, matrix, ink(colors[0]), None, &path);
        }
        return;
    }
    let corners = |rect: ShapeRect| [
        (rect.x0, rect.y0), (rect.x1, rect.y0), (rect.x1, rect.y1), (rect.x0, rect.y1),
    ];
    let outer = corners(outer);
    let inner = corners(inner);
    for (side, color) in colors.into_iter().enumerate() {
        if widths[side] <= 0. || color.3 == 0 {
            continue;
        }
        let next = (side + 1) % 4;
        let mut clip = BezPath::new();
        clip.move_to(outer[side]);
        clip.line_to(outer[next]);
        clip.line_to(inner[next]);
        clip.line_to(inner[side]);
        clip.close_path();
        scene.push_clip_layer(Fill::NonZero, matrix, &clip);
        scene.fill(Fill::EvenOdd, matrix, ink(color), None, &path);
        scene.pop_layer();
    }
}

fn text(
    scene: &mut Scene,
    fonts: &mut HashMap<u64, FontData>,
    matrix: Affine,
    at: (f32, f32),
    run: &Run,
    color: Color,
) {
    if color.3 == 0 {
        return;
    }
    for glyphs in run.glyphs.chunk_by(|a, b| a.face_slot == b.face_slot) {
        let face = run.face_for(&glyphs[0]);
        let font = fonts.entry(face.face().id()).or_insert_with(|| {
            let (bytes, index) = face.face().sfnt();
            FontData::new(Blob::new(Arc::new(bytes)), index)
        });
        scene
            .draw_glyphs(font)
            .transform(matrix)
            .font_size(run.size)
            .brush(ink(color))
            .normalized_coords(&face.normalized_variations())
            .draw(
                Fill::NonZero,
                glyphs.iter().map(|glyph| Glyph {
                    id: u32::from(glyph.id),
                    x: at.0 + glyph.x,
                    y: at.1 - glyph.y,
                }),
            );
    }
}

struct ImageBytes(Arc<RgbaImage>);

impl AsRef<[u8]> for ImageBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_raw()
    }
}

fn draw_image(scene: &mut Scene, matrix: Affine, rect: Rect, image: Arc<RgbaImage>) {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return;
    }
    scene.draw_image(
        &ImageData {
            data: Blob::new(Arc::new(ImageBytes(image))),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        },
        matrix
            * Affine::translate(Vec2::new(f64::from(rect.x), f64::from(rect.y)))
            * Affine::scale_non_uniform(
                f64::from(rect.width) / f64::from(width),
                f64::from(rect.height) / f64::from(height),
            ),
    );
}

fn document_scene(
    layout: &Layout,
    viewport: Rect,
    scroll: ScrollOffset,
    scale: f32,
) -> Result<Scene, String> {
    let mut scene = Scene::new();
    scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &shape_rect(viewport));
    let mut fonts = HashMap::new();
    let mut matrix = Affine2D {
        a: scale,
        d: scale,
        e: viewport.x - scroll.x * scale,
        f: viewport.y - scroll.y * scale,
        ..Affine2D::IDENTITY
    };
    let mut active = true;
    let mut stack = Vec::new();
    for command in &layout.commands {
        match command {
            DrawCommand::PushState {
                transform: local,
                clip,
            } => {
                let prior = (matrix, active);
                matrix = matrix.multiply(*local);
                active &= matrix.inverse().is_some();
                let clipped = active && clip.is_some();
                if let Some(clip) = clip.filter(|_| clipped) {
                    scene.push_clip_layer(Fill::NonZero, transform(matrix), &shape_rect(clip));
                }
                stack.push((prior, clipped));
            }
            DrawCommand::PopState => {
                if let Some((prior, clipped)) = stack.pop() {
                    (matrix, active) = prior;
                    if clipped {
                        scene.pop_layer();
                    }
                }
            }
            _ if !active => {}
            DrawCommand::Rect { rect, color } => {
                fill_rect(&mut scene, transform(matrix), shape_rect(*rect), *color)
            }
            DrawCommand::RoundedFill { rect, radii, color } => {
                scene.fill(
                    Fill::NonZero,
                    transform(matrix),
                    ink(*color),
                    None,
                    &rounded(shape_rect(*rect), *radii),
                );
            }
            DrawCommand::UniformBorder {
                rect,
                widths,
                color,
            } => {
                uniform_border(
                    &mut scene,
                    transform(matrix),
                    shape_rect(*rect),
                    *widths,
                    *color,
                );
            }
            DrawCommand::Text {
                x,
                baseline,
                run,
                color,
                underline,
            } => {
                text(
                    &mut scene,
                    &mut fonts,
                    transform(matrix),
                    (*x, *baseline),
                    run,
                    *color,
                );
                if *underline {
                    fill_rect(
                        &mut scene,
                        transform(matrix),
                        ShapeRect::new(
                            f64::from(*x),
                            f64::from(*baseline + 2.),
                            f64::from(*x + run.advance),
                            f64::from(*baseline + 3.),
                        ),
                        *color,
                    );
                }
            }
            DrawCommand::Image { rect, image } => {
                draw_image(&mut scene, transform(matrix), *rect, image.clone())
            }
            DrawCommand::Canvas { node, rect } => match layout.canvas_frames.get(*node) {
                Some(Frame::Bitmap(image)) => {
                    draw_image(&mut scene, transform(matrix), *rect, image)
                }
                Some(Frame::OpaqueBlack) => fill_rect(
                    &mut scene,
                    transform(matrix),
                    shape_rect(*rect),
                    Color::BLACK,
                ),
                None => {}
            },
            DrawCommand::RoundedBorder { rect, radii, widths, colors } => {
                rounded_border(&mut scene, transform(matrix), shape_rect(*rect), *radii, *widths, *colors);
            }
            DrawCommand::Control { .. } => {
                return Err("Vello form controls are not integrated".into());
            }
            DrawCommand::Svg { rect, scene: svg, .. } => {
                if svg.width > 0. && svg.height > 0. {
                    let local = Affine::translate((f64::from(rect.x), f64::from(rect.y)))
                        * Affine::scale_non_uniform(
                            f64::from(rect.width / svg.width), f64::from(rect.height / svg.height),
                        );
                    svg.record_vello(&mut scene, transform(matrix) * local)?;
                }
            }
        }
    }
    for (_, clipped) in stack {
        if clipped {
            scene.pop_layer();
        }
    }
    scene.pop_layer();
    Ok(scene)
}
