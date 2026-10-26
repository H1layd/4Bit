//! VTF (Valve texture) -> RGBA8, largest mip of the first frame. DXT1/3/5 and the plain 8-bit formats.
use std::path::Path;

pub fn load(path: &Path) -> Result<image::RgbaImage, String> {
    let d = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if &d[0..4] != b"VTF\0" { return Err("not a VTF".into()); }
    let u16_ = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
    let i32_ = |o: usize| i32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let (minor, header) = (i32_(8), i32_(12) as usize);
    let (w, h) = (u16_(16), u16_(18));
    let frames = u16_(24).max(1);
    let fmt = i32_(52);
    let (lw, lh) = (d[61] as usize, d[62] as usize);
    let low_fmt = i32_(57);
    let size = |f: i32, w: usize, h: usize| -> Option<usize> {
        Some(match f {
            13 => w.div_ceil(4) * h.div_ceil(4) * 8,
            14 | 15 => w.div_ceil(4) * h.div_ceil(4) * 16,
            0 | 1 | 11 | 12 => w * h * 4,
            2 | 3 => w * h * 3,
            5 => w * h,
            _ => return None,
        })
    };
    let main = size(fmt, w, h).ok_or(format!("VTF format {fmt} not supported"))?;
    // image data: [low-res thumbnail][mips smallest..largest][frames]. 7.3+ has a resource directory instead.
    let start = if minor >= 3 {
        let n = i32_(68) as usize;
        (0..n).map(|i| 80 + i * 8).find(|&o| d[o] == 0x30 && d[o + 1] == 0 && d[o + 2] == 0)
            .map(|o| i32_(o + 4) as usize).ok_or("no image resource")?
    } else {
        header + if lw > 0 && low_fmt >= 0 { size(low_fmt, lw, lh).unwrap_or(0) } else { 0 }
    };
    let mips = d[56] as usize;
    let mut off = start;
    for m in (1..mips).rev() {
        off += size(fmt, (w >> m).max(1), (h >> m).max(1)).unwrap() * frames;
    }
    let src = d.get(off..off + main).ok_or("truncated VTF")?;
    let mut out = image::RgbaImage::new(w as u32, h as u32);
    match fmt {
        13 | 14 | 15 => {
            let bs = if fmt == 13 { 8 } else { 16 };
            for by in 0..h.div_ceil(4) {
                for bx in 0..w.div_ceil(4) {
                    let blk = &src[(by * w.div_ceil(4) + bx) * bs..][..bs];
                    let px = dxt_block(blk, fmt);
                    for (i, p) in px.iter().enumerate() {
                        let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                        if x < w && y < h { out.put_pixel(x as u32, y as u32, image::Rgba(*p)); }
                    }
                }
            }
        }
        _ => {
            for i in 0..w * h {
                let p = match fmt {
                    0 => [src[i * 4], src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3]],
                    1 => [src[i * 4 + 3], src[i * 4 + 2], src[i * 4 + 1], src[i * 4]],
                    11 => [src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3], src[i * 4]],
                    12 => [src[i * 4 + 2], src[i * 4 + 1], src[i * 4], src[i * 4 + 3]],
                    2 => [src[i * 3], src[i * 3 + 1], src[i * 3 + 2], 255],
                    3 => [src[i * 3 + 2], src[i * 3 + 1], src[i * 3], 255],
                    _ => [src[i], src[i], src[i], 255],
                };
                out.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba(p));
            }
        }
    }
    Ok(out)
}

fn rgb565(c: u16) -> [i32; 3] {
    let r = ((c >> 11) & 31) as i32;
    let g = ((c >> 5) & 63) as i32;
    let b = (c & 31) as i32;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

fn dxt_block(b: &[u8], fmt: i32) -> [[u8; 4]; 16] {
    let cb = if fmt == 13 { b } else { &b[8..] };
    let c0 = u16::from_le_bytes([cb[0], cb[1]]);
    let c1 = u16::from_le_bytes([cb[2], cb[3]]);
    let (a, bb) = (rgb565(c0), rgb565(c1));
    let mut pal = [[0i32; 4]; 4];
    pal[0] = [a[0], a[1], a[2], 255];
    pal[1] = [bb[0], bb[1], bb[2], 255];
    if c0 > c1 || fmt != 13 {
        for k in 0..3 {
            pal[2][k] = (2 * a[k] + bb[k]) / 3;
            pal[3][k] = (a[k] + 2 * bb[k]) / 3;
        }
        pal[2][3] = 255;
        pal[3][3] = 255;
    } else {
        for k in 0..3 { pal[2][k] = (a[k] + bb[k]) / 2; }
        pal[2][3] = 255;
        pal[3] = [0, 0, 0, 0];
    }
    let bits = u32::from_le_bytes([cb[4], cb[5], cb[6], cb[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, o) in out.iter_mut().enumerate() {
        let p = pal[((bits >> (i * 2)) & 3) as usize];
        *o = [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8];
    }
    if fmt == 14 {
        for (i, o) in out.iter_mut().enumerate() {
            let v = (b[i / 2] >> ((i % 2) * 4)) & 15;
            o[3] = v * 17;
        }
    } else if fmt == 15 {
        let (a0, a1) = (b[0] as i32, b[1] as i32);
        let mut ap = [a0, a1, 0, 0, 0, 0, 0, 0];
        if a0 > a1 {
            for k in 1..7i32 { ap[k as usize + 1] = ((7 - k) * a0 + k * a1) / 7; }
        } else {
            for k in 1..5i32 { ap[k as usize + 1] = ((5 - k) * a0 + k * a1) / 5; }
            ap[6] = 0;
            ap[7] = 255;
        }
        let mut abits = 0u64;
        for k in 0..6 { abits |= (b[2 + k] as u64) << (8 * k); }
        for (i, o) in out.iter_mut().enumerate() {
            o[3] = ap[((abits >> (i * 3)) & 7) as usize] as u8;
        }
    }
    out
}
