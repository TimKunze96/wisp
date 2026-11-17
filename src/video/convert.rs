//! Captured frames to I420 for the encoder. Pictures are scaled by
//! averaging (so text stays readable) and letterboxed into the size of a
//! layer, so a window that changes size doesn't change the stream.

use super::{Order, Raw};

/// RGB, 3 bytes a pixel, rows without padding.
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// Pixels in memory: where they are and how they're laid out.
struct Source<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    stride: usize,
    bpp: usize,
    /// Byte offsets of red, green and blue in a pixel.
    rgb: [usize; 3],
}

impl Raw {
    fn source(&self) -> Source<'_> {
        let rgb = match self.order {
            Order::Bgra => [2, 1, 0],
            Order::Rgba => [0, 1, 2],
        };
        Source { data: &self.data, width: self.width, height: self.height, stride: self.stride, bpp: 4, rgb }
    }
}

impl Rgb {
    fn source(&self) -> Source<'_> {
        Source { data: &self.data, width: self.width, height: self.height, stride: self.width * 3, bpp: 3, rgb: [0, 1, 2] }
    }

    /// The frame fit into `width` x `height`, black around it.
    pub fn from_raw(frame: &Raw, width: usize, height: usize) -> Self {
        letterbox(&frame.source(), width, height)
    }

    /// This picture at another size (same shape: it was letterboxed already).
    pub fn scaled(&self, width: usize, height: usize) -> Self {
        Rgb { width, height, data: scale(&self.source(), width, height) }
    }

    pub fn to_i420(&self) -> Vec<u8> {
        let (width, height) = (self.width, self.height);
        let rgb = &self.data;
        let (cw, ch) = (width / 2, height / 2);
        let mut out = vec![0u8; width * height + 2 * cw * ch];
        let (y_plane, uv) = out.split_at_mut(width * height);
        let (u_plane, v_plane) = uv.split_at_mut(cw * ch);
        for (i, px) in rgb.chunks_exact(3).enumerate() {
            let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
            y_plane[i] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
        for cy in 0..ch {
            for cx in 0..cw {
                // average of the 2x2 block
                let (mut r, mut g, mut b) = (0, 0, 0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = ((cy * 2 + dy) * width + cx * 2 + dx) * 3;
                    r += rgb[i] as i32;
                    g += rgb[i + 1] as i32;
                    b += rgb[i + 2] as i32;
                }
                let (r, g, b) = (r / 4, g / 4, b / 4);
                u_plane[cy * cw + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
                v_plane[cy * cw + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
            }
        }
        out
    }
}

/// `src` scaled to fit inside `width` x `height` and centered on black.
fn letterbox(src: &Source, width: usize, height: usize) -> Rgb {
    let factor = (width as f64 / src.width as f64).min(height as f64 / src.height as f64);
    let fw = ((src.width as f64 * factor).round() as usize).clamp(1, width);
    let fh = ((src.height as f64 * factor).round() as usize).clamp(1, height);
    let inner = scale(src, fw, fh);
    if (fw, fh) == (width, height) {
        return Rgb { width, height, data: inner };
    }
    let mut data = vec![0u8; width * height * 3];
    let (x0, y0) = ((width - fw) / 2, (height - fh) / 2);
    for y in 0..fh {
        let to = ((y0 + y) * width + x0) * 3;
        data[to..to + fw * 3].copy_from_slice(&inner[y * fw * 3..(y + 1) * fw * 3]);
    }
    Rgb { width, height, data }
}

/// RGB at the target size: each target pixel is the average of the source
/// pixels it covers.
fn scale(src: &Source, width: usize, height: usize) -> Vec<u8> {
    let (sw, sh, bpp) = (src.width, src.height, src.bpp);
    let [ri, gi, bi] = src.rgb;
    let mut out = vec![0u8; width * height * 3];
    if (sw, sh) == (width, height) {
        for y in 0..height {
            let row = &src.data[y * src.stride..][..width * bpp];
            for (x, px) in row.chunks_exact(bpp).enumerate() {
                let o = (y * width + x) * 3;
                out[o] = px[ri];
                out[o + 1] = px[gi];
                out[o + 2] = px[bi];
            }
        }
        return out;
    }
    let span = |i: usize, n: usize, of: usize| (i * of / n, ((i + 1) * of / n).max(i * of / n + 1).min(of));
    let cols: Vec<(usize, usize)> = (0..width).map(|x| span(x, width, sw)).collect();
    let mut sums = vec![0u32; width * 3];
    for y in 0..height {
        let (y0, y1) = span(y, height, sh);
        sums.fill(0);
        for sy in y0..y1 {
            let row = &src.data[sy * src.stride..][..sw * bpp];
            for (x, &(x0, x1)) in cols.iter().enumerate() {
                let s = &mut sums[x * 3..x * 3 + 3];
                for px in row[x0 * bpp..x1 * bpp].chunks_exact(bpp) {
                    s[0] += px[ri] as u32;
                    s[1] += px[gi] as u32;
                    s[2] += px[bi] as u32;
                }
            }
        }
        for (x, &(x0, x1)) in cols.iter().enumerate() {
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            let o = (y * width + x) * 3;
            for c in 0..3 {
                out[o + c] = (sums[x * 3 + c] / n) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4x2 BGRA with padding at the end of each row: white, black, red, red.
    fn frame() -> Raw {
        let mut data = Vec::new();
        for _ in 0..2 {
            data.extend_from_slice(&[255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 255, 0, 9, 9]);
        }
        Raw { width: 4, height: 2, stride: 18, order: Order::Bgra, data }
    }

    #[test]
    fn halves_and_keeps_colors() {
        let rgb = Rgb::from_raw(&frame(), 2, 1);
        assert_eq!(rgb.data, [127, 127, 127, 255, 0, 0]);
        let yuv = Rgb::from_raw(&frame(), 4, 2).to_i420();
        assert_eq!(yuv.len(), 4 * 2 + 2 * 2);
        assert_eq!(&yuv[..4], &[235, 16, 82, 82]);
    }

    #[test]
    fn letterboxes() {
        // 4x2 into 4x4: two black rows above and below... one each
        let rgb = Rgb::from_raw(&frame(), 4, 4);
        assert_eq!(&rgb.data[..12], &[0; 12]);
        assert_eq!(&rgb.data[12..15], &[255, 255, 255]);
        assert_eq!(&rgb.data[36..], &[0; 12]);
    }
}
