//! Image comparison: per-pixel differences, block highlighting and diff regions.

use image::RgbaImage;

pub struct ImageDiff {
    pub width: u32,
    pub height: u32,
    pub block_size: u32,
    pub blocks_x: u32,
    pub blocks_y: u32,
    /// Block grid; true when a block contains differing pixels.
    pub blocks: Vec<bool>,
    pub diff_pixels: u64,
    /// Bounding rectangles (x, y, w, h) of connected differing regions, in pixels.
    pub regions: Vec<[u32; 4]>,
    /// Visualisation of the absolute difference between the first and last image.
    pub diff_image: RgbaImage,
}

fn pixel(img: &RgbaImage, x: u32, y: u32) -> Option<[u8; 4]> {
    if x < img.width() && y < img.height() {
        Some(img.get_pixel(x, y).0)
    } else {
        None
    }
}

fn differs(a: Option<[u8; 4]>, b: Option<[u8; 4]>, threshold: u8) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a
            .iter()
            .zip(b.iter())
            .any(|(x, y)| x.abs_diff(*y) > threshold),
        (None, None) => false,
        _ => true,
    }
}

pub fn compare(images: &[&RgbaImage], threshold: u8, block_size: u32) -> ImageDiff {
    let width = images.iter().map(|i| i.width()).max().unwrap_or(0);
    let height = images.iter().map(|i| i.height()).max().unwrap_or(0);
    let bs = block_size.max(1);
    let bx = width.div_ceil(bs).max(1);
    let by = height.div_ceil(bs).max(1);
    let mut blocks = vec![false; (bx * by) as usize];
    let mut diff_pixels = 0u64;
    let first = images[0];
    let last = images[images.len() - 1];
    let mut diff_image = RgbaImage::new(width.max(1), height.max(1));
    for y in 0..height {
        for x in 0..width {
            let p0 = pixel(first, x, y);
            let d = images[1..]
                .iter()
                .any(|img| differs(p0, pixel(img, x, y), threshold));
            let pl = pixel(last, x, y);
            let out = match (p0, pl) {
                (Some(a), Some(b)) => {
                    let m = |i: usize| a[i].abs_diff(b[i]);
                    let v = m(0).max(m(1)).max(m(2)).max(m(3));
                    if d {
                        [255, 255u8.saturating_sub(v), 255u8.saturating_sub(v), 255]
                    } else {
                        let g = ((a[0] as u32 + a[1] as u32 + a[2] as u32) / 3) as u8;
                        let g = 128 + g / 2;
                        [g, g, g, 255]
                    }
                }
                _ => [255, 0, 255, 255],
            };
            diff_image.put_pixel(x, y, image::Rgba(out));
            if d {
                diff_pixels += 1;
                blocks[((y / bs) * bx + x / bs) as usize] = true;
            }
        }
    }
    let regions = regions(&blocks, bx, by, bs, width, height);
    ImageDiff {
        width,
        height,
        block_size: bs,
        blocks_x: bx,
        blocks_y: by,
        blocks,
        diff_pixels,
        regions,
        diff_image,
    }
}

fn regions(blocks: &[bool], bx: u32, by: u32, bs: u32, w: u32, h: u32) -> Vec<[u32; 4]> {
    let mut seen = vec![false; blocks.len()];
    let mut out = Vec::new();
    for start in 0..blocks.len() {
        if !blocks[start] || seen[start] {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i as u32 % bx, i as u32 / bx);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            let mut visit = |nx: i64, ny: i64| {
                if nx >= 0 && ny >= 0 && (nx as u32) < bx && (ny as u32) < by {
                    let j = (ny as u32 * bx + nx as u32) as usize;
                    if blocks[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            };
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                visit(x as i64 + dx, y as i64 + dy);
            }
        }
        let px = x0 * bs;
        let py = y0 * bs;
        let pw = ((x1 + 1) * bs).min(w) - px;
        let ph = ((y1 + 1) * bs).min(h) - py;
        out.push([px, py, pw, ph]);
    }
    out.sort_by_key(|r| (r[1], r[0]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_diff() {
        let a = RgbaImage::from_pixel(32, 32, image::Rgba([0, 0, 0, 255]));
        let mut b = a.clone();
        b.put_pixel(5, 5, image::Rgba([255, 0, 0, 255]));
        b.put_pixel(30, 30, image::Rgba([255, 0, 0, 255]));
        let d = compare(&[&a, &b], 0, 8);
        assert_eq!(d.diff_pixels, 2);
        assert_eq!(d.regions.len(), 2);
    }
}
