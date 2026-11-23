use image::RgbaImage;
use rust_browser_javascript::{ErrorKind, JsError};
use std::{cell::Cell, rc::Rc, sync::Arc};

const MAX_SIDE: u32 = 8192;
const BITMAP_BYTES: usize = 32 * 1024 * 1024;
pub(super) const REALM_BYTES: usize = 64 * 1024 * 1024;

/// Straight sRGB RGBA8. Allocation is lazy, and snapshots keep their pixels
/// when drawing continues. The renderer holds at most one published frame per
/// canvas, separately bounded by the same realm allocation limit.
pub(super) struct Bitmap {
    size: [u32; 2],
    alpha: bool,
    budget: Rc<Cell<usize>>,
    image: Option<Arc<RgbaImage>>,
}

impl Bitmap {
    pub(super) fn new(size: [u32; 2], alpha: bool, budget: Rc<Cell<usize>>) -> Self {
        Self {
            size,
            alpha,
            budget,
            image: None,
        }
    }

    pub(super) fn width(&self) -> u32 {
        self.size[0]
    }

    pub(super) fn height(&self) -> u32 {
        self.size[1]
    }

    pub(super) fn alpha(&self) -> bool {
        self.alpha
    }

    pub(super) fn supported(&self) -> bool {
        Self::supports_size(self.size)
    }

    pub(super) fn supports_size([width, height]: [u32; 2]) -> bool {
        width <= MAX_SIDE
            && height <= MAX_SIDE
            && u64::from(width) * u64::from(height) * 4 <= BITMAP_BYTES as u64
    }

    pub(super) fn image(&self) -> Option<&RgbaImage> {
        self.image.as_deref()
    }

    pub(super) fn snapshot(&self) -> Option<Arc<RgbaImage>> {
        self.image.clone()
    }

    pub(super) fn reset(&mut self, size: [u32; 2]) {
        self.release();
        self.size = size;
    }

    pub(super) fn image_mut(&mut self) -> Result<Option<&mut RgbaImage>, JsError> {
        if self.size.contains(&0) {
            return Ok(None);
        }
        if !self.supported() {
            return Err(memory_limit());
        }
        let count = self.width() as usize * self.height() as usize * 4;
        let fresh = self.image.is_none();
        if fresh && self.budget.get() + count > REALM_BYTES {
            return Err(memory_limit());
        }
        if self
            .image
            .as_mut()
            .is_none_or(|image| Arc::get_mut(image).is_none())
        {
            let mut data = Vec::new();
            data.try_reserve_exact(count).map_err(|_| memory_limit())?;
            if let Some(image) = self.image() {
                data.extend_from_slice(image.as_raw());
            } else {
                data.resize(count, 0);
                if !self.alpha {
                    for pixel in data.as_chunks_mut::<4>().0 {
                        pixel[3] = 255;
                    }
                }
            }
            let image = RgbaImage::from_raw(self.width(), self.height(), data)
                .expect("canvas buffer dimensions match allocation");
            self.image = Some(Arc::new(image));
            if fresh {
                self.budget.set(self.budget.get() + count);
            }
        }
        Ok(self.image.as_mut().and_then(Arc::get_mut))
    }

    fn release(&mut self) {
        if let Some(image) = self.image.take() {
            self.budget.set(self.budget.get() - image.as_raw().len());
        }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        self.release();
    }
}

fn memory_limit() -> JsError {
    JsError::new(
        ErrorKind::HeapLimit,
        "Canvas backing bitmap budget exceeded",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn quota_is_released_on_reset_while_published_pixels_survive() {
        let budget = Rc::new(Cell::new(REALM_BYTES - 4));
        let mut first = Bitmap::new([1, 1], true, budget.clone());
        let mut second = Bitmap::new([1, 1], false, budget.clone());
        first
            .image_mut()
            .unwrap()
            .unwrap()
            .put_pixel(0, 0, Rgba([9, 8, 7, 255]));
        let old = first.snapshot().unwrap();
        assert!(second.image_mut().is_err());
        first.reset([1, 1]);
        assert!(first.image().is_none());
        assert_eq!(
            second.image_mut().unwrap().unwrap().get_pixel(0, 0).0,
            [0, 0, 0, 255]
        );
        assert_eq!(old.get_pixel(0, 0).0, [9, 8, 7, 255]);
        assert_eq!(budget.get(), REALM_BYTES);
    }

    #[test]
    fn independently_rejects_large_area_and_large_dimensions() {
        assert!(Bitmap::supports_size([4096, 2048]));
        assert!(!Bitmap::supports_size([4096, 2049]));
        assert!(!Bitmap::supports_size([u32::MAX, u32::MAX]));
        assert!(!Bitmap::supports_size([8193, 1]));
    }
}
