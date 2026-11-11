//! Image comparison: per-pixel differences, block highlighting and diff regions.

use image::RgbaImage;

pub struct ImageDiff {
    pub width: u32,
    pub height: u32,
    pub block_size: u32,
    pub blocks_x: u32,
    /// Whether each pixel differs, row by row: what the blocks are worked out from, kept to
    /// mark the pixels themselves.
    pub mask: Vec<bool>,
    pub diff_pixels: u64,
    /// Bounding rectangles (x, y, w, h) of connected differing regions, in pixels.
    pub regions: Vec<[u32; 4]>,
    /// The most any channel of a pixel differs between the first image and the last, block by
    /// block; 0 for a block where these two do not differ. They are the two the blink view
    /// shows in turn, so this tells whether the eye can see a block change there.
    pub strength: Vec<u8>,
    /// Visualisation of the absolute difference between the first image and the others.
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

/// The most a channel of two pixels differs; all of it where only one image has the pixel.
fn apart(a: Option<[u8; 4]>, b: Option<[u8; 4]>) -> u8 {
    match (a, b) {
        (Some(a), Some(b)) => (0..4).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0),
        (None, None) => 0,
        _ => 255,
    }
}

/// How red the difference image shows the least difference there can be, of 255.
const FAINTEST: u8 = 96;

pub fn compare(images: &[&RgbaImage], threshold: u8, block_size: u32) -> ImageDiff {
    let width = images.iter().map(|i| i.width()).max().unwrap_or(0);
    let height = images.iter().map(|i| i.height()).max().unwrap_or(0);
    let bs = block_size.max(1);
    let bx = width.div_ceil(bs).max(1);
    let by = height.div_ceil(bs).max(1);
    let mut blocks = vec![false; (bx * by) as usize];
    let mut strength = vec![0u8; blocks.len()];
    let mut mask = Vec::with_capacity(width as usize * height as usize);
    let mut diff_pixels = 0u64;
    let first = images[0];
    let last = images[images.len() - 1];
    let mut diff_image = RgbaImage::new(width.max(1), height.max(1));
    for y in 0..height {
        for x in 0..width {
            let p0 = pixel(first, x, y);
            // Whether any image differs from the first here, and by how much at most: of three
            // images the middle one may be the only one that does. Most pixels are the same in
            // every image, and those are passed over at once.
            let (mut d, mut v) = (false, 0u8);
            for img in &images[1..] {
                let p = pixel(img, x, y);
                if p != p0 {
                    d |= differs(p0, p, threshold);
                    v = v.max(apart(p0, p));
                }
            }
            let pl = pixel(last, x, y);
            let out = match (p0, pl) {
                (Some(a), Some(_)) => {
                    if d {
                        // From a clear red for the least difference to a full one for the
                        // most: one shade off is as much a difference as any, and in
                        // proportion alone it would be lost in the white.
                        let rest =
                            255 - FAINTEST - (v as u32 * (255 - FAINTEST as u32) / 255) as u8;
                        [255, rest, rest, 255]
                    } else {
                        let g = ((a[0] as u32 + a[1] as u32 + a[2] as u32) / 3) as u8;
                        let g = 128 + g / 2;
                        [g, g, g, 255]
                    }
                }
                _ => [255, 0, 255, 255],
            };
            diff_image.put_pixel(x, y, image::Rgba(out));
            mask.push(d);
            if d {
                diff_pixels += 1;
                let block = ((y / bs) * bx + x / bs) as usize;
                blocks[block] = true;
                if differs(p0, pl, threshold) {
                    strength[block] = strength[block].max(apart(p0, pl));
                }
            }
        }
    }
    let regions = regions(&blocks, bx, by, bs, width, height);
    ImageDiff {
        width,
        height,
        block_size: bs,
        blocks_x: bx,
        mask,
        strength,
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

    /// The difference image shows one shade of difference in a red that can be seen, and
    /// the difference of the middle of three images as well as that of the last.
    #[test]
    fn the_least_difference_shows_and_the_middle_image_counts() {
        let px = |v: u8| RgbaImage::from_pixel(8, 8, image::Rgba([v, v, v, 255]));
        let (a, shade, far) = (px(100), px(101), px(255));
        let d = compare(&[&a, &shade], 0, 8);
        assert_eq!(d.diff_image.get_pixel(0, 0).0, [255, 159, 159, 255]);
        assert_eq!(d.strength, [1]);
        // The middle image alone differs, and much: a full red, but nothing for the blink
        // view to show between the first image and the last.
        let d = compare(&[&a, &far, &a], 0, 8);
        assert_eq!(d.diff_pixels, 64);
        assert_eq!(d.diff_image.get_pixel(0, 0).0, [255, 63, 63, 255]);
        assert_eq!(d.strength, [0]);
        // The last image is a shade off under a middle one far off: the blink view shows
        // the shade.
        let d = compare(&[&a, &far, &shade], 0, 8);
        assert_eq!(d.strength, [1]);
        // One pixel that differs is the only one marked, though its whole block is the area.
        let mut one = a.clone();
        one.put_pixel(3, 2, image::Rgba([0, 0, 0, 255]));
        let d = compare(&[&a, &one], 0, 8);
        let marked: Vec<usize> = (0..d.mask.len()).filter(|&i| d.mask[i]).collect();
        assert_eq!((marked, d.regions), (vec![2 * 8 + 3], vec![[0, 0, 8, 8]]));
        // Images that are the same have no difference at all.
        let d = compare(&[&a, &a], 0, 8);
        assert_eq!((d.diff_pixels, d.strength), (0, vec![0]));
    }

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
