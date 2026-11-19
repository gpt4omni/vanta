// Pinned to c5a9e3727b635c29bee8e16584f7e006c533249c: changing only the
// unsigned arithmetic lost the old region path's measured throughput.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EtcFormat {
    Rgb8,
    Rgb8A1,
    Rgba8,
    R11,
    R11Signed,
    Rg11,
    Rg11Signed,
}

impl EtcFormat {
    pub fn block_bytes(self) -> usize {
        match self {
            EtcFormat::Rgb8 | EtcFormat::Rgb8A1 | EtcFormat::R11 | EtcFormat::R11Signed => 8,
            EtcFormat::Rgba8 | EtcFormat::Rg11 | EtcFormat::Rg11Signed => 16,
        }
    }

    pub fn texel_bytes(self) -> usize {
        match self {
            EtcFormat::Rgb8 | EtcFormat::Rgb8A1 | EtcFormat::Rgba8 => 4,
            EtcFormat::R11 | EtcFormat::R11Signed => 2,
            EtcFormat::Rg11 | EtcFormat::Rg11Signed => 4,
        }
    }
}

const INTENSITY: [[i32; 2]; 8] = [
    [2, 8],
    [5, 17],
    [9, 29],
    [13, 42],
    [18, 60],
    [24, 80],
    [33, 106],
    [47, 183],
];

const DISTANCE: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

const EAC_MODIFIERS: [[i32; 8]; 16] = [
    [-3, -6, -9, -15, 2, 5, 8, 14],
    [-3, -7, -10, -13, 2, 6, 9, 12],
    [-2, -5, -8, -13, 1, 4, 7, 12],
    [-2, -4, -6, -13, 1, 3, 5, 12],
    [-3, -6, -8, -12, 2, 5, 7, 11],
    [-3, -7, -9, -11, 2, 6, 8, 10],
    [-4, -7, -8, -11, 3, 6, 7, 10],
    [-3, -5, -8, -11, 2, 4, 7, 10],
    [-2, -6, -8, -10, 1, 5, 7, 9],
    [-2, -5, -8, -10, 1, 4, 7, 9],
    [-2, -4, -8, -10, 1, 3, 7, 9],
    [-2, -5, -7, -10, 1, 4, 6, 9],
    [-3, -4, -7, -10, 2, 3, 6, 9],
    [-1, -2, -3, -10, 0, 1, 2, 9],
    [-4, -6, -8, -9, 3, 5, 7, 8],
    [-3, -5, -7, -9, 2, 4, 6, 8],
];

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn ext4(v: u8) -> i32 {
    i32::from(v) * 17
}

fn ext5(v: i32) -> i32 {
    (v << 3) | (v >> 2)
}

fn ext6(v: u8) -> i32 {
    let v = i32::from(v);
    (v << 2) | (v >> 4)
}

fn ext7(v: u8) -> i32 {
    let v = i32::from(v);
    (v << 1) | (v >> 6)
}

fn signed3(v: u8) -> i32 {
    let v = i32::from(v & 7);
    if v >= 4 {
        v - 8
    } else {
        v
    }
}

fn pixel_index(lo: u32, x: usize, y: usize) -> usize {
    let i = x * 4 + y;
    let msb = (lo >> (16 + i)) & 1;
    let lsb = (lo >> i) & 1;
    ((msb << 1) | lsb) as usize
}

pub fn decode_etc2_rgb(block: &[u8], punchthrough: bool, out: &mut [[u8; 4]; 16]) {
    let b0 = block[0];
    let b1 = block[1];
    let b2 = block[2];
    let b3 = block[3];
    let lo = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
    let bit33 = (b3 >> 1) & 1 != 0;
    let (differential, opaque) = if punchthrough { (true, bit33) } else { (bit33, true) };

    if !differential {
        let c1 = [ext4(b0 >> 4), ext4(b1 >> 4), ext4(b2 >> 4)];
        let c2 = [ext4(b0 & 0xF), ext4(b1 & 0xF), ext4(b2 & 0xF)];
        subblocks(c1, c2, b3, lo, true, out);
        return;
    }

    let r = i32::from(b0 >> 3);
    let g = i32::from(b1 >> 3);
    let b = i32::from(b2 >> 3);
    let r2 = r + signed3(b0);
    let g2 = g + signed3(b1);
    let b2c = b + signed3(b2);

    if !(0..=31).contains(&r2) {
        let c1 = [
            ext4((((b0 >> 3) & 3) << 2) | (b0 & 3)),
            ext4(b1 >> 4),
            ext4(b1 & 0xF),
        ];
        let c2 = [ext4(b2 >> 4), ext4(b2 & 0xF), ext4(b3 >> 4)];
        let d = DISTANCE[usize::from((((b3 >> 2) & 3) << 1) | (b3 & 1))];
        let paint = [c1, add(c2, d), c2, add(c2, -d)];
        paint_block(&paint, lo, opaque, out);
    } else if !(0..=31).contains(&g2) {
        let r1 = (b0 >> 3) & 0xF;
        let g1 = ((b0 & 7) << 1) | ((b1 >> 4) & 1);
        let bl1 = (b1 & 8) | ((b1 & 3) << 1) | (b2 >> 7);
        let r2h = (b2 >> 3) & 0xF;
        let g2h = ((b2 & 7) << 1) | (b3 >> 7);
        let bl2 = (b3 >> 3) & 0xF;
        let v1 = (u32::from(r1) << 8) | (u32::from(g1) << 4) | u32::from(bl1);
        let v2 = (u32::from(r2h) << 8) | (u32::from(g2h) << 4) | u32::from(bl2);
        let idx = (((b3 >> 2) & 1) << 2) | ((b3 & 1) << 1) | u8::from(v1 >= v2);
        let d = DISTANCE[usize::from(idx)];
        let c1 = [ext4(r1), ext4(g1), ext4(bl1)];
        let c2 = [ext4(r2h), ext4(g2h), ext4(bl2)];
        let paint = [add(c1, d), add(c1, -d), add(c2, d), add(c2, -d)];
        paint_block(&paint, lo, opaque, out);
    } else if !(0..=31).contains(&b2c) {
        let ro = ext6((b0 >> 1) & 0x3F);
        let go = ext7(((b0 & 1) << 6) | ((b1 >> 1) & 0x3F));
        let bo = ext6(((b1 & 1) << 5) | (((b2 >> 3) & 3) << 3) | ((b2 & 3) << 1) | (b3 >> 7));
        let rh = ext6((((b3 >> 2) & 0x1F) << 1) | (b3 & 1));
        let gh = ext7(((lo >> 25) & 0x7F) as u8);
        let bh = ext6(((lo >> 19) & 0x3F) as u8);
        let rv = ext6(((lo >> 13) & 0x3F) as u8);
        let gv = ext7(((lo >> 6) & 0x7F) as u8);
        let bv = ext6((lo & 0x3F) as u8);
        for y in 0..4 {
            for x in 0..4 {
                let (xi, yi) = (x as i32, y as i32);
                let f = |o: i32, h: i32, v: i32| clamp8((xi * (h - o) + yi * (v - o) + 4 * o + 2) >> 2);
                out[y * 4 + x] = [f(ro, rh, rv), f(go, gh, gv), f(bo, bh, bv), 255];
            }
        }
    } else {
        let c1 = [ext5(r), ext5(g), ext5(b)];
        let c2 = [ext5(r2), ext5(g2), ext5(b2c)];
        subblocks(c1, c2, b3, lo, opaque, out);
    }
}

fn add(c: [i32; 3], d: i32) -> [i32; 3] {
    [c[0] + d, c[1] + d, c[2] + d]
}

fn paint_block(paint: &[[i32; 3]; 4], lo: u32, opaque: bool, out: &mut [[u8; 4]; 16]) {
    let mut colours = paint.map(|p| [clamp8(p[0]), clamp8(p[1]), clamp8(p[2]), 255]);
    if !opaque {
        colours[2] = [0; 4];
    }
    for y in 0..4 {
        for x in 0..4 {
            let idx = pixel_index(lo, x, y);
            out[y * 4 + x] = colours[idx];
        }
    }
}

fn subblocks(c1: [i32; 3], c2: [i32; 3], b3: u8, lo: u32, opaque: bool, out: &mut [[u8; 4]; 16]) {
    let cw = [usize::from(b3 >> 5), usize::from((b3 >> 2) & 7)];
    let flip = b3 & 1 != 0;
    let colours = [c1, c2];
    let paint: [[[u8; 4]; 4]; 2] = std::array::from_fn(|i| {
        let base = colours[i];
        let table = INTENSITY[cw[i]];
        let modifiers = [if opaque { table[0] } else { 0 }, table[1], -table[0], -table[1]];
        std::array::from_fn(|idx| {
            let m = modifiers[idx];
            if !opaque && idx == 2 {
                [0; 4]
            } else {
                [clamp8(base[0] + m), clamp8(base[1] + m), clamp8(base[2] + m), 255]
            }
        })
    });
    for y in 0..4 {
        for x in 0..4 {
            let second = if flip { y >= 2 } else { x >= 2 };
            let idx = pixel_index(lo, x, y);
            out[y * 4 + x] = paint[usize::from(second)][idx];
        }
    }
}

fn eac_parts(block: &[u8]) -> (u8, i32, &'static [i32; 8], u64) {
    let bits = u64::from_be_bytes([
        0, 0, block[2], block[3], block[4], block[5], block[6], block[7],
    ]);
    (block[0], i32::from(block[1] >> 4), &EAC_MODIFIERS[usize::from(block[1] & 0xF)], bits)
}

fn eac_index(bits: u64, x: usize, y: usize) -> usize {
    let i = x * 4 + y;
    ((bits >> (45 - 3 * i)) & 7) as usize
}

pub fn decode_eac_alpha(block: &[u8], out: &mut [[u8; 4]; 16]) {
    let (base, mult, table, bits) = eac_parts(block);
    for y in 0..4 {
        for x in 0..4 {
            out[y * 4 + x][3] = clamp8(i32::from(base) + table[eac_index(bits, x, y)] * mult);
        }
    }
}

pub fn decode_eac11(block: &[u8], signed: bool, out: &mut [u16; 16]) {
    let (base, mult, table, bits) = eac_parts(block);
    for y in 0..4 {
        for x in 0..4 {
            let m = table[eac_index(bits, x, y)];
            let step = if mult == 0 { m } else { m * mult * 8 };
            out[y * 4 + x] = if signed {
                let b = i32::from(base as i8).max(-127);
                let v = (b * 8 + step).clamp(-1023, 1023);
                let mag = v.abs();
                let wide = (mag << 5) | (mag >> 5);
                (if v < 0 { -wide } else { wide }) as i16 as u16
            } else {
                let v = (i32::from(base) * 8 + 4 + step).clamp(0, 2047);
                ((v << 5) | (v >> 6)) as u16
            };
        }
    }
}

pub fn decode_block(format: EtcFormat, block: &[u8], out: &mut [u8; 64]) {
    decode_block_rows(format, block, out, 4 * format.texel_bytes());
}

fn decode_block_rows(format: EtcFormat, block: &[u8], out: &mut [u8], stride: usize) {
    match format {
        EtcFormat::Rgb8 | EtcFormat::Rgb8A1 | EtcFormat::Rgba8 => {
            let mut px = [[0u8; 4]; 16];
            if format == EtcFormat::Rgba8 {
                decode_etc2_rgb(&block[8..16], false, &mut px);
                decode_eac_alpha(&block[0..8], &mut px);
            } else {
                decode_etc2_rgb(&block[0..8], format == EtcFormat::Rgb8A1, &mut px);
            }
            for y in 0..4 {
                let row: [u8; 16] = std::array::from_fn(|i| px[y * 4 + i / 4][i % 4]);
                out[y * stride..y * stride + 16].copy_from_slice(&row);
            }
        }
        EtcFormat::R11 | EtcFormat::R11Signed => {
            let mut r = [0u16; 16];
            decode_eac11(&block[0..8], format == EtcFormat::R11Signed, &mut r);
            for y in 0..4 {
                let row: [u8; 8] = std::array::from_fn(|i| r[y * 4 + i / 2].to_le_bytes()[i % 2]);
                out[y * stride..y * stride + 8].copy_from_slice(&row);
            }
        }
        EtcFormat::Rg11 | EtcFormat::Rg11Signed => {
            let signed = format == EtcFormat::Rg11Signed;
            let mut r = [0u16; 16];
            let mut g = [0u16; 16];
            decode_eac11(&block[0..8], signed, &mut r);
            decode_eac11(&block[8..16], signed, &mut g);
            for y in 0..4 {
                let row: [u8; 16] = std::array::from_fn(|i| {
                    let v = if i % 4 < 2 { r[y * 4 + i / 4] } else { g[y * 4 + i / 4] };
                    v.to_le_bytes()[i % 2]
                });
                out[y * stride..y * stride + 16].copy_from_slice(&row);
            }
        }
    }
}

pub struct Surface<'a> {
    pub data: &'a [u8],
    pub row_blocks: usize,
    pub rows_of_blocks: usize,
}

pub fn decode_region(
    format: EtcFormat,
    src: &Surface<'_>,
    width: usize,
    height: usize,
    dst: &mut [u8],
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Ok(());
    }
    let bb = format.block_bytes();
    let tb = format.texel_bytes();
    let bw = width.div_ceil(4);
    let bh = height.div_ceil(4);
    if bw > src.row_blocks || bh > src.rows_of_blocks {
        return Err(format!(
            "region {width}x{height} exceeds source {}x{} blocks",
            src.row_blocks, src.rows_of_blocks
        ));
    }
    let need = ((bh - 1) * src.row_blocks + bw) * bb;
    if src.data.len() < need {
        return Err(format!("source has {} bytes, region needs {need}", src.data.len()));
    }
    if dst.len() < width * height * tb {
        return Err(format!("destination has {} bytes, region needs {}", dst.len(), width * height * tb));
    }
    // Keep interior results in place; only cropped blocks need uncropped texels
    // in scratch for a following identical block.
    let mut texels = [0u8; 64];
    let mut previous: Option<(&[u8], usize, bool)> = None;
    let stride = width * tb;
    for by in 0..bh {
        for bx in 0..bw {
            let off = (by * src.row_blocks + bx) * bb;
            let block = &src.data[off..off + bb];
            let d = (by * 4 * width + bx * 4) * tb;
            let full = bx * 4 + 4 <= width && by * 4 + 4 <= height;
            let same = previous.filter(|&(raw, _, _)| raw == block);
            if full {
                if let Some((_, from, was_full)) = same {
                    for y in 0..4 {
                        if was_full {
                            dst.copy_within(from + y * stride..from + y * stride + 4 * tb, d + y * stride);
                        } else {
                            dst[d + y * stride..d + y * stride + 4 * tb]
                                .copy_from_slice(&texels[y * 4 * tb..(y + 1) * 4 * tb]);
                        }
                    }
                } else {
                    decode_block_rows(format, block, &mut dst[d..], stride);
                }
            } else {
                if let Some((_, from, true)) = same {
                    for y in 0..4 {
                        texels[y * 4 * tb..(y + 1) * 4 * tb]
                            .copy_from_slice(&dst[from + y * stride..from + y * stride + 4 * tb]);
                    }
                } else if same.is_none() {
                    decode_block(format, block, &mut texels);
                }
                let bytes = (width - bx * 4).min(4) * tb;
                for y in 0..(height - by * 4).min(4) {
                    dst[d + y * stride..d + y * stride + bytes]
                        .copy_from_slice(&texels[y * 4 * tb..y * 4 * tb + bytes]);
                }
            }
            previous = Some((block, d, full));
        }
    }
    Ok(())
}
