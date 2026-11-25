#[allow(dead_code)]
#[path = "etc_decode_legacy.rs"]
mod legacy;

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

#[inline]
fn pack_rgba_u32(r: i32, g: i32, b: i32, a: u8) -> u32 {
    u32::from_le_bytes([clamp8(r), clamp8(g), clamp8(b), a])
}

#[inline]
fn pixel_index(lo: u32, x: usize, y: usize) -> usize {
    let i = x * 4 + y;
    let msb = (lo >> (16 + i)) & 1;
    let lsb = (lo >> i) & 1;
    ((msb << 1) | lsb) as usize
}

#[inline]
fn paint_block_u32(paint: &[[i32; 3]; 4], lo: u32, opaque: bool, out: &mut [u32; 16]) {
    let mut colours = [
        pack_rgba_u32(paint[0][0], paint[0][1], paint[0][2], 255),
        pack_rgba_u32(paint[1][0], paint[1][1], paint[1][2], 255),
        if opaque { pack_rgba_u32(paint[2][0], paint[2][1], paint[2][2], 255) } else { 0 },
        pack_rgba_u32(paint[3][0], paint[3][1], paint[3][2], 255),
    ];
    if !opaque {
        colours[2] = 0;
    }
    for y in 0..4 {
        let base = y * 4;
        out[base] = colours[pixel_index(lo, 0, y)];
        out[base + 1] = colours[pixel_index(lo, 1, y)];
        out[base + 2] = colours[pixel_index(lo, 2, y)];
        out[base + 3] = colours[pixel_index(lo, 3, y)];
    }
}

#[inline]
fn subblocks_u32(c1: [i32; 3], c2: [i32; 3], b3: u8, lo: u32, opaque: bool, out: &mut [u32; 16]) {
    let cw = [usize::from(b3 >> 5), usize::from((b3 >> 2) & 7)];
    let flip = b3 & 1 != 0;
    let colours = [c1, c2];
    let mut paint = [[0u32; 4]; 2];
    for i in 0..2 {
        let base = colours[i];
        let table = INTENSITY[cw[i]];
        let m0 = if opaque { table[0] } else { 0 };
        paint[i][0] = pack_rgba_u32(base[0] + m0, base[1] + m0, base[2] + m0, 255);
        paint[i][1] = pack_rgba_u32(base[0] + table[1], base[1] + table[1], base[2] + table[1], 255);
        paint[i][2] = if opaque {
            pack_rgba_u32(base[0] - table[0], base[1] - table[0], base[2] - table[0], 255)
        } else {
            0
        };
        paint[i][3] = pack_rgba_u32(base[0] - table[1], base[1] - table[1], base[2] - table[1], 255);
    }
    if flip {
        for y in 0..2 {
            let base = y * 4;
            let p = &paint[0];
            out[base] = p[pixel_index(lo, 0, y)];
            out[base + 1] = p[pixel_index(lo, 1, y)];
            out[base + 2] = p[pixel_index(lo, 2, y)];
            out[base + 3] = p[pixel_index(lo, 3, y)];
        }
        for y in 2..4 {
            let base = y * 4;
            let p = &paint[1];
            out[base] = p[pixel_index(lo, 0, y)];
            out[base + 1] = p[pixel_index(lo, 1, y)];
            out[base + 2] = p[pixel_index(lo, 2, y)];
            out[base + 3] = p[pixel_index(lo, 3, y)];
        }
    } else {
        let p0 = &paint[0];
        let p1 = &paint[1];
        for y in 0..4 {
            let base = y * 4;
            out[base] = p0[pixel_index(lo, 0, y)];
            out[base + 1] = p0[pixel_index(lo, 1, y)];
            out[base + 2] = p1[pixel_index(lo, 2, y)];
            out[base + 3] = p1[pixel_index(lo, 3, y)];
        }
    }
}

fn decode_etc2_rgb_u32(block: &[u8], punchthrough: bool, out: &mut [u32; 16]) {
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
        subblocks_u32(c1, c2, b3, lo, true, out);
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
        paint_block_u32(&paint, lo, opaque, out);
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
        paint_block_u32(&paint, lo, opaque, out);
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
        let (drx, dry) = (rh - ro, rv - ro);
        let (dgx, dgy) = (gh - go, gv - go);
        let (dbx, dby) = (bh - bo, bv - bo);
        let (r0, g0, b0p) = (4 * ro + 2, 4 * go + 2, 4 * bo + 2);
        for y in 0..4 {
            let yi = y as i32;
            let ry = r0 + yi * dry;
            let gy = g0 + yi * dgy;
            let by = b0p + yi * dby;
            let base = y * 4;
            for x in 0..4 {
                let xi = x as i32;
                out[base + x] = pack_rgba_u32(
                    (ry + xi * drx) >> 2,
                    (gy + xi * dgx) >> 2,
                    (by + xi * dbx) >> 2,
                    255,
                );
            }
        }
    } else {
        let c1 = [ext5(r), ext5(g), ext5(b)];
        let c2 = [ext5(r2), ext5(g2), ext5(b2c)];
        subblocks_u32(c1, c2, b3, lo, opaque, out);
    }
}

pub fn decode_etc2_rgb(block: &[u8], punchthrough: bool, out: &mut [[u8; 4]; 16]) {
    let mut px = [0u32; 16];
    decode_etc2_rgb_u32(block, punchthrough, &mut px);
    for i in 0..16 {
        out[i] = px[i].to_le_bytes();
    }
}

#[inline]
fn add(c: [i32; 3], d: i32) -> [i32; 3] {
    [c[0] + d, c[1] + d, c[2] + d]
}

#[cfg(test)]
fn paint_block(paint: &[[i32; 3]; 4], lo: u32, opaque: bool, out: &mut [[u8; 4]; 16]) {
    let mut px = [0u32; 16];
    paint_block_u32(paint, lo, opaque, &mut px);
    for i in 0..16 {
        out[i] = px[i].to_le_bytes();
    }
}

#[cfg(test)]
fn subblocks(c1: [i32; 3], c2: [i32; 3], b3: u8, lo: u32, opaque: bool, out: &mut [[u8; 4]; 16]) {
    let mut px = [0u32; 16];
    subblocks_u32(c1, c2, b3, lo, opaque, &mut px);
    for i in 0..16 {
        out[i] = px[i].to_le_bytes();
    }
}

#[inline]
fn eac_parts(block: &[u8]) -> (u8, i32, &'static [i32; 8], u64) {
    let bits = u64::from_be_bytes([
        0, 0, block[2], block[3], block[4], block[5], block[6], block[7],
    ]);
    (block[0], i32::from(block[1] >> 4), &EAC_MODIFIERS[usize::from(block[1] & 0xF)], bits)
}

#[inline]
fn eac_index(bits: u64, x: usize, y: usize) -> usize {
    let i = x * 4 + y;
    ((bits >> (45 - 3 * i)) & 7) as usize
}

#[inline]
fn decode_eac_alpha_u32(block: &[u8], out: &mut [u32; 16]) {
    let (base, mult, table, bits) = eac_parts(block);
    let paint = table.map(|m| u32::from(clamp8(i32::from(base) + m * mult)) << 24);
    for y in 0..4 {
        let base_idx = y * 4;
        out[base_idx] = (out[base_idx] & 0x00FF_FFFF) | paint[eac_index(bits, 0, y)];
        out[base_idx + 1] = (out[base_idx + 1] & 0x00FF_FFFF) | paint[eac_index(bits, 1, y)];
        out[base_idx + 2] = (out[base_idx + 2] & 0x00FF_FFFF) | paint[eac_index(bits, 2, y)];
        out[base_idx + 3] = (out[base_idx + 3] & 0x00FF_FFFF) | paint[eac_index(bits, 3, y)];
    }
}

pub fn decode_eac_alpha(block: &[u8], out: &mut [[u8; 4]; 16]) {
    let (base, mult, table, bits) = eac_parts(block);
    let paint = table.map(|m| clamp8(i32::from(base) + m * mult));
    for y in 0..4 {
        for x in 0..4 {
            out[y * 4 + x][3] = paint[eac_index(bits, x, y)];
        }
    }
}

#[inline]
fn eac_palette(block: &[u8], signed: bool) -> ([u16; 8], u64) {
    let (base, mult, table, bits) = eac_parts(block);
    let scale = if mult == 0 { 1 } else { mult * 8 };
    let b = if signed { i32::from(base as i8).max(-127) * 8 } else { i32::from(base) * 8 + 4 };
    let paint = table.map(|m| {
        let value = b + m * scale;
        if signed {
            let v = value.clamp(-1023, 1023);
            let mag = v.abs();
            let wide = (mag << 5) | (mag >> 5);
            (if v < 0 { -wide } else { wide }) as i16 as u16
        } else {
            let v = value.clamp(0, 2047);
            ((v << 5) | (v >> 6)) as u16
        }
    });
    (paint, bits)
}

#[cfg(test)]
pub fn decode_eac11(block: &[u8], signed: bool, out: &mut [u16; 16]) {
    let (paint, bits) = eac_palette(block, signed);
    for y in 0..4 {
        for x in 0..4 {
            out[y * 4 + x] = paint[eac_index(bits, x, y)];
        }
    }
}

#[inline]
fn write_u32x16_to_bytes(px: &[u32; 16], out: &mut [u8; 64]) {
    for i in 0..16 {
        out[i * 4..i * 4 + 4].copy_from_slice(&px[i].to_le_bytes());
    }
}

#[inline]
fn decode_block_rgb8(block: &[u8], out: &mut [u8; 64]) {
    let mut px = [0u32; 16];
    decode_etc2_rgb_u32(&block[0..8], false, &mut px);
    write_u32x16_to_bytes(&px, out);
}

#[inline]
fn decode_block_rgb8a1(block: &[u8], out: &mut [u8; 64]) {
    let mut px = [0u32; 16];
    decode_etc2_rgb_u32(&block[0..8], true, &mut px);
    write_u32x16_to_bytes(&px, out);
}

#[inline]
fn decode_block_rgba8(block: &[u8], out: &mut [u8; 64]) {
    let mut px = [0u32; 16];
    decode_etc2_rgb_u32(&block[8..16], false, &mut px);
    decode_eac_alpha_u32(&block[0..8], &mut px);
    write_u32x16_to_bytes(&px, out);
}

#[inline]
fn decode_block_r11(block: &[u8], signed: bool, out: &mut [u8; 64]) {
    let (paint, bits) = eac_palette(&block[0..8], signed);
    for y in 0..4 {
        let row = y * 8;
        for x in 0..4 {
            out[row + x * 2..row + x * 2 + 2].copy_from_slice(&paint[eac_index(bits, x, y)].to_le_bytes());
        }
    }
}

#[inline]
fn decode_block_rg11(block: &[u8], signed: bool, out: &mut [u8; 64]) {
    let (red, rbits) = eac_palette(&block[0..8], signed);
    let (green, gbits) = eac_palette(&block[8..16], signed);
    for y in 0..4 {
        let row = y * 16;
        for x in 0..4 {
            let word = u32::from(red[eac_index(rbits, x, y)]) | (u32::from(green[eac_index(gbits, x, y)]) << 16);
            out[row + x * 4..row + x * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
}

pub fn decode_block(format: EtcFormat, block: &[u8], out: &mut [u8; 64]) {
    match format {
        EtcFormat::Rgb8 => decode_block_rgb8(block, out),
        EtcFormat::Rgb8A1 => decode_block_rgb8a1(block, out),
        EtcFormat::Rgba8 => decode_block_rgba8(block, out),
        EtcFormat::R11 => decode_block_r11(block, false, out),
        EtcFormat::R11Signed => decode_block_r11(block, true, out),
        EtcFormat::Rg11 => decode_block_rg11(block, false, out),
        EtcFormat::Rg11Signed => decode_block_rg11(block, true, out),
    }
}

pub struct Surface<'a> {
    pub data: &'a [u8],
    pub row_blocks: usize,
    pub rows_of_blocks: usize,
}

#[inline(always)]
fn decode_region_loop<const BB: usize, const TB: usize, F: FnMut(&[u8], &mut [u8; 64])>(
    src: &Surface<'_>,
    width: usize,
    height: usize,
    dst: &mut [u8],
    mut decode_one: F,
) {
    let bw = width.div_ceil(4);
    let bh = height.div_ceil(4);
    // Keep the last decoded block in L1/stack scratch so consecutive identical
    // blocks never read back from `dst`, which may be uncached write-combined
    // Vulkan staging memory where CPU reads stall by 50-100x.
    let mut texels = [0u8; 64];
    let mut previous: Option<&[u8]> = None;
    let stride = width * TB;
    let row_bytes = 4 * TB;
    let full_bw = width / 4;
    let full_bh = height / 4;
    for by in 0..bh {
        let full_y = by < full_bh;
        let max_y = if full_y { 4 } else { height - by * 4 };
        let row_base = by * src.row_blocks;
        for bx in 0..bw {
            let off = (row_base + bx) * BB;
            let block = &src.data[off..off + BB];
            let d = (by * 4 * width + bx * 4) * TB;
            if previous != Some(block) {
                decode_one(block, &mut texels);
                previous = Some(block);
            }
            if full_y && bx < full_bw {
                for y in 0..4 {
                    dst[d + y * stride..d + y * stride + row_bytes]
                        .copy_from_slice(&texels[y * row_bytes..(y + 1) * row_bytes]);
                }
            } else {
                let bytes = (width - bx * 4).min(4) * TB;
                for y in 0..max_y {
                    dst[d + y * stride..d + y * stride + bytes]
                        .copy_from_slice(&texels[y * row_bytes..y * row_bytes + bytes]);
                }
            }
        }
    }
}

pub fn decode_region(
    format: EtcFormat,
    src: &Surface<'_>,
    width: usize,
    height: usize,
    dst: &mut [u8],
) -> Result<(), String> {
    let unsigned = match format {
        EtcFormat::R11 => Some(legacy::EtcFormat::R11),
        EtcFormat::Rg11 => Some(legacy::EtcFormat::Rg11),
        _ => None,
    };
    if let Some(format) = unsigned {
        let source = legacy::Surface { data: src.data, row_blocks: src.row_blocks, rows_of_blocks: src.rows_of_blocks };
        return legacy::decode_region(format, &source, width, height, dst);
    }
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
    // Hoist format dispatch outside the (by, bx) block loop.
    match format {
        EtcFormat::Rgb8 => decode_region_loop::<8, 4, _>(src, width, height, dst, decode_block_rgb8),
        EtcFormat::Rgb8A1 => decode_region_loop::<8, 4, _>(src, width, height, dst, decode_block_rgb8a1),
        EtcFormat::Rgba8 => decode_region_loop::<16, 4, _>(src, width, height, dst, decode_block_rgba8),
        EtcFormat::R11Signed => decode_region_loop::<8, 2, _>(src, width, height, dst, |b, o| decode_block_r11(b, true, o)),
        EtcFormat::Rg11Signed => decode_region_loop::<16, 4, _>(src, width, height, dst, |b, o| decode_block_rg11(b, true, o)),
        EtcFormat::R11 | EtcFormat::Rg11 => unreachable!(),
    }
    Ok(())
}

#[inline]
fn pack_rgb565(r: i32, g: i32, b: i32) -> u16 {
    (((r.clamp(0, 255) as u16) & 0xF8) << 8)
        | (((g.clamp(0, 255) as u16) & 0xFC) << 3)
        | ((b.clamp(0, 255) as u16) >> 3)
}

#[inline]
fn unpack_rgb565(c: u16) -> [i32; 3] {
    let r = ((c >> 8) & 0xF8) as i32;
    let g = ((c >> 3) & 0xFC) as i32;
    let b = ((c << 3) & 0xF8) as i32;
    [r | (r >> 5), g | (g >> 6), b | (b >> 5)]
}

#[inline]
fn dot3(a: [i32; 3], b: [i32; 3]) -> i32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn bc1_bbox_opaque_scalar(rgba: &[u8; 64]) -> ([i32; 3], [i32; 3]) {
    let mut mn = [255i32; 3];
    let mut mx = [0i32; 3];
    for i in 0..16 {
        let p = &rgba[i * 4..i * 4 + 4];
        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
        if r < mn[0] { mn[0] = r; }
        if r > mx[0] { mx[0] = r; }
        if g < mn[1] { mn[1] = g; }
        if g > mx[1] { mx[1] = g; }
        if b < mn[2] { mn[2] = b; }
        if b > mx[2] { mx[2] = b; }
    }
    (mn, mx)
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn bc1_bbox_opaque_sse2(rgba: &[u8; 64]) -> ([i32; 3], [i32; 3]) {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is architecturally mandatory on all x86_64 targets and
    // `rgba` is a 64-byte array (4 x 16 bytes) loaded with unaligned loads.
    unsafe {
        let ptr = rgba.as_ptr() as *const __m128i;
        let r0 = _mm_loadu_si128(ptr);
        let r1 = _mm_loadu_si128(ptr.add(1));
        let r2 = _mm_loadu_si128(ptr.add(2));
        let r3 = _mm_loadu_si128(ptr.add(3));
        let mut vmin = _mm_min_epu8(_mm_min_epu8(r0, r1), _mm_min_epu8(r2, r3));
        let mut vmax = _mm_max_epu8(_mm_max_epu8(r0, r1), _mm_max_epu8(r2, r3));
        vmin = _mm_min_epu8(vmin, _mm_srli_si128::<8>(vmin));
        vmin = _mm_min_epu8(vmin, _mm_srli_si128::<4>(vmin));
        vmax = _mm_max_epu8(vmax, _mm_srli_si128::<8>(vmax));
        vmax = _mm_max_epu8(vmax, _mm_srli_si128::<4>(vmax));
        let mn_u32 = _mm_cvtsi128_si32(vmin) as u32;
        let mx_u32 = _mm_cvtsi128_si32(vmax) as u32;
        (
            [(mn_u32 & 0xFF) as i32, ((mn_u32 >> 8) & 0xFF) as i32, ((mn_u32 >> 16) & 0xFF) as i32],
            [(mx_u32 & 0xFF) as i32, ((mx_u32 >> 8) & 0xFF) as i32, ((mx_u32 >> 16) & 0xFF) as i32],
        )
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn bc1_bbox_opaque_neon(rgba: &[u8; 64]) -> ([i32; 3], [i32; 3]) {
    use std::arch::aarch64::*;
    // SAFETY: NEON is architecturally mandatory on all aarch64 targets and
    // `rgba` is a 64-byte array (4 x 16 bytes).
    unsafe {
        let ptr = rgba.as_ptr();
        let r0 = vld1q_u8(ptr);
        let r1 = vld1q_u8(ptr.add(16));
        let r2 = vld1q_u8(ptr.add(32));
        let r3 = vld1q_u8(ptr.add(48));
        let vmin16 = vminq_u8(vminq_u8(r0, r1), vminq_u8(r2, r3));
        let vmax16 = vmaxq_u8(vmaxq_u8(r0, r1), vmaxq_u8(r2, r3));
        let vmin8 = vmin_u8(vget_low_u8(vmin16), vget_high_u8(vmin16));
        let vmax8 = vmax_u8(vget_low_u8(vmax16), vget_high_u8(vmax16));
        let mut mn_arr = [0u8; 8];
        let mut mx_arr = [0u8; 8];
        vst1_u8(mn_arr.as_mut_ptr(), vmin8);
        vst1_u8(mx_arr.as_mut_ptr(), vmax8);
        (
            [
                mn_arr[0].min(mn_arr[4]) as i32,
                mn_arr[1].min(mn_arr[5]) as i32,
                mn_arr[2].min(mn_arr[6]) as i32,
            ],
            [
                mx_arr[0].max(mx_arr[4]) as i32,
                mx_arr[1].max(mx_arr[5]) as i32,
                mx_arr[2].max(mx_arr[6]) as i32,
            ],
        )
    }
}

#[inline]
fn bc1_indices_opaque_scalar(rgba: &[u8; 64], dir: [i32; 3], m02: i32, m23: i32, m31: i32) -> u32 {
    let mut bits = 0u32;
    for i in 0..16 {
        let p = &rgba[i * 4..i * 4 + 4];
        let t = (p[0] as i32 * dir[0] + p[1] as i32 * dir[1] + p[2] as i32 * dir[2]) * 2;
        let best = if t >= m02 { 0u32 } else if t >= m23 { 2u32 } else if t >= m31 { 3u32 } else { 1u32 };
        bits |= best << (i * 2);
    }
    bits
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn bc1_indices_opaque_sse2(rgba: &[u8; 64], dir: [i32; 3], m02: i32, m23: i32, m31: i32) -> u32 {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is architecturally mandatory on x86_64. Each `dir[c]` is in
    // -255..=255 so `2 * dir[c]` fits cleanly in i16 (-510..=510), and the
    // dot product fits in i32 without overflow (max |t| <= 390,150).
    unsafe {
        let zero = _mm_setzero_si128();
        let w_dir = _mm_set_epi16(
            0,
            (dir[2] * 2) as i16,
            (dir[1] * 2) as i16,
            (dir[0] * 2) as i16,
            0,
            (dir[2] * 2) as i16,
            (dir[1] * 2) as i16,
            (dir[0] * 2) as i16,
        );
        // `t >= m` is equivalent to `t > m - 1` for signed 32-bit integers.
        let v_m02 = _mm_set1_epi32(m02 - 1);
        let v_m23 = _mm_set1_epi32(m23 - 1);
        let v_m31 = _mm_set1_epi32(m31 - 1);
        let ptr = rgba.as_ptr() as *const __m128i;
        let mut bits = 0u32;
        for row in 0..4 {
            let px = _mm_loadu_si128(ptr.add(row));
            let lo16 = _mm_unpacklo_epi8(px, zero);
            let hi16 = _mm_unpackhi_epi8(px, zero);
            // Multiply-add pairs: [r*2dr + g*2dg, b*2db + 0] for each pixel.
            let mlo = _mm_madd_epi16(lo16, w_dir);
            let mhi = _mm_madd_epi16(hi16, w_dir);
            // Horizontal pairwise sum across the 4 pixels in this row:
            // rg = [p0.rg, p1.rg, p2.rg, p3.rg], bb = [p0.b, p1.b, p2.b, p3.b].
            let rg = _mm_castps_si128(_mm_shuffle_ps::<0x88>(_mm_castsi128_ps(mlo), _mm_castsi128_ps(mhi)));
            let bb = _mm_castps_si128(_mm_shuffle_ps::<0xDD>(_mm_castsi128_ps(mlo), _mm_castsi128_ps(mhi)));
            let t = _mm_add_epi32(rg, bb);

            // Classify each of the 4 pixels:
            // t >= m02 -> 0, t >= m23 -> 2, t >= m31 -> 3, else 1.
            let ge02 = _mm_movemask_ps(_mm_castsi128_ps(_mm_cmpgt_epi32(t, v_m02))) as u32;
            let ge23 = _mm_movemask_ps(_mm_castsi128_ps(_mm_cmpgt_epi32(t, v_m23))) as u32;
            let ge31 = _mm_movemask_ps(_mm_castsi128_ps(_mm_cmpgt_epi32(t, v_m31))) as u32;

            for k in 0..4 {
                let bit_k = 1u32 << k;
                let code = if (ge02 & bit_k) != 0 {
                    0u32
                } else if (ge23 & bit_k) != 0 {
                    2u32
                } else if (ge31 & bit_k) != 0 {
                    3u32
                } else {
                    1u32
                };
                bits |= code << ((row * 4 + k) * 2);
            }
        }
        bits
    }
}

fn encode_bc1_block_scalar(rgba: &[u8; 64], punchthrough: bool, out: &mut [u8]) {
    let mut has_trans = false;
    let (mut mn, mut mx) = if punchthrough {
        let mut mn = [255i32; 3];
        let mut mx = [0i32; 3];
        for i in 0..16 {
            let p = &rgba[i * 4..i * 4 + 4];
            if p[3] == 0 {
                has_trans = true;
                continue;
            }
            for c in 0..3 {
                let v = p[c] as i32;
                if v < mn[c] { mn[c] = v; }
                if v > mx[c] { mx[c] = v; }
            }
        }
        (mn, mx)
    } else {
        bc1_bbox_opaque_scalar(rgba)
    };
    encode_bc1_finish(rgba, has_trans, &mut mn, &mut mx, out, false);
}

#[inline]
fn encode_bc1_finish(
    rgba: &[u8; 64],
    has_trans: bool,
    mn: &mut [i32; 3],
    mx: &mut [i32; 3],
    out: &mut [u8],
    use_simd: bool,
) {
    if mn[0] > mx[0] {
        out[0..4].copy_from_slice(&0u32.to_le_bytes());
        out[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        return;
    }
    for c in 0..3 {
        let inset = (mx[c] - mn[c]) >> 4;
        mn[c] = (mn[c] + inset).min(255);
        mx[c] = (mx[c] - inset).max(0);
    }
    let mut c0 = pack_rgb565(mx[0], mx[1], mx[2]);
    let mut c1 = pack_rgb565(mn[0], mn[1], mn[2]);
    if has_trans {
        if c0 > c1 {
            std::mem::swap(&mut c0, &mut c1);
        }
        if c0 == c1 {
            let mut bits = 0u32;
            for i in 0..16 {
                if rgba[i * 4 + 3] == 0 {
                    bits |= 3u32 << (i * 2);
                }
            }
            out[0..2].copy_from_slice(&c0.to_le_bytes());
            out[2..4].copy_from_slice(&c1.to_le_bytes());
            out[4..8].copy_from_slice(&bits.to_le_bytes());
            return;
        }
        let p0 = unpack_rgb565(c0);
        let p1 = unpack_rgb565(c1);
        let p2 = [(p0[0] + p1[0]) >> 1, (p0[1] + p1[1]) >> 1, (p0[2] + p1[2]) >> 1];
        let dir = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let d0 = dot3(p0, dir);
        let d1 = dot3(p1, dir);
        let d2 = dot3(p2, dir);
        let m02 = d0 + d2;
        let m21 = d2 + d1;
        let mut bits = 0u32;
        for i in 0..16 {
            let p = &rgba[i * 4..i * 4 + 4];
            let best = if p[3] == 0 {
                3u32
            } else {
                let t = (p[0] as i32 * dir[0] + p[1] as i32 * dir[1] + p[2] as i32 * dir[2]) * 2;
                if t <= m02 { 0 } else if t <= m21 { 2 } else { 1 }
            };
            bits |= best << (i * 2);
        }
        out[0..2].copy_from_slice(&c0.to_le_bytes());
        out[2..4].copy_from_slice(&c1.to_le_bytes());
        out[4..8].copy_from_slice(&bits.to_le_bytes());
        return;
    }
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    if c0 == c1 {
        out[0..2].copy_from_slice(&c0.to_le_bytes());
        out[2..4].copy_from_slice(&c1.to_le_bytes());
        out[4..8].copy_from_slice(&0u32.to_le_bytes());
        return;
    }
    let p0 = unpack_rgb565(c0);
    let p1 = unpack_rgb565(c1);
    let p2 = [(2 * p0[0] + p1[0]) / 3, (2 * p0[1] + p1[1]) / 3, (2 * p0[2] + p1[2]) / 3];
    let p3 = [(p0[0] + 2 * p1[0]) / 3, (p0[1] + 2 * p1[1]) / 3, (p0[2] + 2 * p1[2]) / 3];
    let dir = [p0[0] - p1[0], p0[1] - p1[1], p0[2] - p1[2]];
    let d0 = dot3(p0, dir);
    let d1 = dot3(p1, dir);
    let d2 = dot3(p2, dir);
    let d3 = dot3(p3, dir);
    let m02 = d0 + d2;
    let m23 = d2 + d3;
    let m31 = d3 + d1;
    let bits = {
        #[cfg(target_arch = "x86_64")]
        {
            if use_simd {
                bc1_indices_opaque_sse2(rgba, dir, m02, m23, m31)
            } else {
                bc1_indices_opaque_scalar(rgba, dir, m02, m23, m31)
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = use_simd;
            bc1_indices_opaque_scalar(rgba, dir, m02, m23, m31)
        }
    };
    out[0..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    out[4..8].copy_from_slice(&bits.to_le_bytes());
}

fn encode_bc1_block(rgba: &[u8; 64], punchthrough: bool, out: &mut [u8]) {
    if punchthrough {
        encode_bc1_block_scalar(rgba, true, out);
        return;
    }
    #[cfg(target_arch = "x86_64")]
    let (mut mn, mut mx) = bc1_bbox_opaque_sse2(rgba);
    #[cfg(target_arch = "aarch64")]
    let (mut mn, mut mx) = bc1_bbox_opaque_neon(rgba);
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let (mut mn, mut mx) = bc1_bbox_opaque_scalar(rgba);

    encode_bc1_finish(rgba, false, &mut mn, &mut mx, out, true);
}

#[inline]
fn bc4_quantize_recip(span: u32) -> u32 {
    ((7u32 << 17) + span - 1) / span
}

#[inline]
fn bc4_quantize(delta: u32, inv: u32) -> u32 {
    (delta * inv + 65_536) >> 17
}

fn encode_bc4_unorm_block(vals: &[u8; 16], out: &mut [u8]) {
    let mut lo = 255u32;
    let mut hi = 0u32;
    for &v in vals {
        lo = lo.min(v as u32);
        hi = hi.max(v as u32);
    }
    if hi == lo {
        out[0..4].copy_from_slice(&(lo | (lo << 8)).to_le_bytes());
        out[4..8].copy_from_slice(&0u32.to_le_bytes());
        return;
    }
    // 1 reciprocal per block (bit-exact across all span in 1..=255).
    let inv = bc4_quantize_recip(hi - lo);
    let mut w0 = hi | (lo << 8);
    let mut w1 = 0u32;
    for (i, &v) in vals.iter().enumerate() {
        let q = bc4_quantize(hi - v as u32, inv);
        let code = if q == 0 { 0 } else if q == 7 { 1 } else { q + 1 };
        let bit = 16 + i * 3;
        if bit < 32 {
            w0 |= code << bit;
            if bit > 29 {
                w1 |= code >> (32 - bit);
            }
        } else {
            w1 |= code << (bit - 32);
        }
    }
    out[0..4].copy_from_slice(&w0.to_le_bytes());
    out[4..8].copy_from_slice(&w1.to_le_bytes());
}

fn encode_bc4_snorm_block(vals: &[i8; 16], out: &mut [u8]) {
    let mut lo = 127i32;
    let mut hi = -127i32;
    for &v in vals {
        lo = lo.min(v as i32);
        hi = hi.max(v as i32);
    }
    let uhi = (hi as u32) & 0xFF;
    let ulo = (lo as u32) & 0xFF;
    if hi == lo {
        out[0..4].copy_from_slice(&(uhi | (ulo << 8)).to_le_bytes());
        out[4..8].copy_from_slice(&0u32.to_le_bytes());
        return;
    }
    // 1 reciprocal per block (bit-exact across all span in 1..=254).
    let inv = bc4_quantize_recip((hi - lo) as u32);
    let mut w0 = uhi | (ulo << 8);
    let mut w1 = 0u32;
    for (i, &v) in vals.iter().enumerate() {
        let q = bc4_quantize((hi - v as i32) as u32, inv);
        let code = if q == 0 { 0 } else if q == 7 { 1 } else { q + 1 };
        let bit = 16 + i * 3;
        if bit < 32 {
            w0 |= code << bit;
            if bit > 29 {
                w1 |= code >> (32 - bit);
            }
        } else {
            w1 |= code << (bit - 32);
        }
    }
    out[0..4].copy_from_slice(&w0.to_le_bytes());
    out[4..8].copy_from_slice(&w1.to_le_bytes());
}

#[inline(always)]
fn transcode_region_bc_loop<const BB: usize, F: FnMut(&[u8], &mut [u8; 16])>(
    src: &Surface<'_>,
    bw: usize,
    bh: usize,
    dst: &mut [u8],
    mut transcode_one: F,
) {
    let mut bc_block = [0u8; 16];
    let mut previous: Option<&[u8]> = None;
    for by in 0..bh {
        let src_row = by * src.row_blocks;
        let dst_row = by * bw;
        for bx in 0..bw {
            let off = (src_row + bx) * BB;
            let block = &src.data[off..off + BB];
            if previous != Some(block) {
                transcode_one(block, &mut bc_block);
                previous = Some(block);
            }
            let d = (dst_row + bx) * BB;
            dst[d..d + BB].copy_from_slice(&bc_block[0..BB]);
        }
    }
}

/// CPU fallback transcoder from ETC2/EAC to native BC1/BC3/BC4/BC5 blocks.
/// Mirrors `etc_decode_gpu.comp`'s BC mode bit-for-bit so BC-backed images
/// never break if a region falls back from GPU compute to CPU.
pub fn transcode_region_bc(
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
    let dst_need = bw * bh * bb;
    if dst.len() < dst_need {
        return Err(format!("destination has {} bytes, region needs {dst_need}", dst.len()));
    }
    let mut texels = [0u8; 64];
    // Hoist format dispatch outside the (by, bx) block loop.
    match format {
        EtcFormat::Rgb8 => transcode_region_bc_loop::<8, _>(src, bw, bh, dst, |block, bc| {
            decode_block_rgb8(block, &mut texels);
            encode_bc1_block(&texels, false, &mut bc[0..8]);
        }),
        EtcFormat::Rgb8A1 => transcode_region_bc_loop::<8, _>(src, bw, bh, dst, |block, bc| {
            decode_block_rgb8a1(block, &mut texels);
            encode_bc1_block(&texels, true, &mut bc[0..8]);
        }),
        EtcFormat::Rgba8 => transcode_region_bc_loop::<16, _>(src, bw, bh, dst, |block, bc| {
            decode_block_rgba8(block, &mut texels);
            let mut a = [0u8; 16];
            for i in 0..16 {
                a[i] = texels[i * 4 + 3];
            }
            encode_bc4_unorm_block(&a, &mut bc[0..8]);
            encode_bc1_block(&texels, false, &mut bc[8..16]);
        }),
        EtcFormat::R11 => transcode_region_bc_loop::<8, _>(src, bw, bh, dst, |block, bc| {
            let (paint, bits) = eac_palette(block, false);
            let mut v = [0u8; 16];
            for y in 0..4 {
                for x in 0..4 {
                    let u = u32::from(paint[eac_index(bits, x, y)]);
                    v[y * 4 + x] = ((u + 128) >> 8).min(255) as u8;
                }
            }
            encode_bc4_unorm_block(&v, &mut bc[0..8]);
        }),
        EtcFormat::R11Signed => transcode_region_bc_loop::<8, _>(src, bw, bh, dst, |block, bc| {
            let (paint, bits) = eac_palette(block, true);
            let mut s = [0i8; 16];
            for y in 0..4 {
                for x in 0..4 {
                    let s16 = paint[eac_index(bits, x, y)] as i16 as i32;
                    let r = if s16 >= 0 { (s16 + 128) >> 8 } else { -((-s16 + 128) >> 8) };
                    s[y * 4 + x] = r.clamp(-127, 127) as i8;
                }
            }
            encode_bc4_snorm_block(&s, &mut bc[0..8]);
        }),
        EtcFormat::Rg11 => transcode_region_bc_loop::<16, _>(src, bw, bh, dst, |block, bc| {
            let (rp, rbits) = eac_palette(&block[0..8], false);
            let (gp, gbits) = eac_palette(&block[8..16], false);
            let (mut r, mut g) = ([0u8; 16], [0u8; 16]);
            for y in 0..4 {
                for x in 0..4 {
                    let ru = u32::from(rp[eac_index(rbits, x, y)]);
                    let gu = u32::from(gp[eac_index(gbits, x, y)]);
                    r[y * 4 + x] = ((ru + 128) >> 8).min(255) as u8;
                    g[y * 4 + x] = ((gu + 128) >> 8).min(255) as u8;
                }
            }
            encode_bc4_unorm_block(&r, &mut bc[0..8]);
            encode_bc4_unorm_block(&g, &mut bc[8..16]);
        }),
        EtcFormat::Rg11Signed => transcode_region_bc_loop::<16, _>(src, bw, bh, dst, |block, bc| {
            let (rp, rbits) = eac_palette(&block[0..8], true);
            let (gp, gbits) = eac_palette(&block[8..16], true);
            let (mut r, mut g) = ([0i8; 16], [0i8; 16]);
            for y in 0..4 {
                for x in 0..4 {
                    let rs = rp[eac_index(rbits, x, y)] as i16 as i32;
                    let gs = gp[eac_index(gbits, x, y)] as i16 as i32;
                    let rr = if rs >= 0 { (rs + 128) >> 8 } else { -((-rs + 128) >> 8) };
                    let gr = if gs >= 0 { (gs + 128) >> 8 } else { -((-gs + 128) >> 8) };
                    r[y * 4 + x] = rr.clamp(-127, 127) as i8;
                    g[y * 4 + x] = gr.clamp(-127, 127) as i8;
                }
            }
            encode_bc4_snorm_block(&r, &mut bc[0..8]);
            encode_bc4_snorm_block(&g, &mut bc[8..16]);
        }),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_eac11(block: &[u8], signed: bool, out: &mut [u16; 16]) {
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

    #[test]
    fn eac_palettes_match_the_old_per_pixel_decoder() {
        let mut seed = 0x1234_5678u32;
        for i in 0..65536 {
            let mut block = [0; 16];
            for byte in &mut block {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = (seed >> 24) as u8;
            }
            // Cover every base/table/multiplier pair as well as random selectors.
            block[0] = (i >> 8) as u8;
            block[1] = i as u8;
            for signed in [false, true] {
                let mut expected = [[0; 16]; 2];
                reference_eac11(&block[..8], signed, &mut expected[0]);
                reference_eac11(&block[8..], signed, &mut expected[1]);
                for (format, channels) in if signed {
                    [(EtcFormat::R11Signed, 1), (EtcFormat::Rg11Signed, 2)]
                } else {
                    [(EtcFormat::R11, 1), (EtcFormat::Rg11, 2)]
                } {
                    let mut out = [0; 64];
                    decode_block(format, &block, &mut out);
                    for px in 0..16 {
                        for c in 0..channels {
                            let at = (px * channels + c) * 2;
                            assert_eq!(&out[at..at + 2], &expected[c][px].to_le_bytes());
                        }
                    }
                }
            }
            let (base, mult, table, bits) = eac_parts(&block);
            let mut alpha = [[0; 4]; 16];
            decode_eac_alpha(&block, &mut alpha);
            for y in 0..4 {
                for x in 0..4 {
                    assert_eq!(alpha[y * 4 + x][3], clamp8(i32::from(base) + table[eac_index(bits, x, y)] * mult));
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn microbench_eac_interleaved() {
        use std::hint::black_box;
        use std::time::Instant;

        let (width, height) = (1024, 1024);
        for (format, old_format) in [
            (EtcFormat::R11, legacy::EtcFormat::R11),
            (EtcFormat::R11Signed, legacy::EtcFormat::R11Signed),
            (EtcFormat::Rg11, legacy::EtcFormat::Rg11),
            (EtcFormat::Rg11Signed, legacy::EtcFormat::Rg11Signed),
        ] {
            let mut seed = 0x1234_5678u32;
            let mut data = vec![0; width * height / 16 * format.block_bytes()];
            for out in data.chunks_mut(format.block_bytes()) {
                let mut block = [0; 16];
                for byte in &mut block {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    *byte = (seed >> 24) as u8;
                }
                out.copy_from_slice(&block[..format.block_bytes()]);
            }
            let surface = Surface { data: &data, row_blocks: width / 4, rows_of_blocks: height / 4 };
            let old_surface = legacy::Surface { data: &data, row_blocks: width / 4, rows_of_blocks: height / 4 };
            let mut old_dst = vec![0; width * height * format.texel_bytes()];
            let mut new_dst = old_dst.clone();
            legacy::decode_region(old_format, &old_surface, width, height, &mut old_dst).unwrap();
            decode_region(format, &surface, width, height, &mut new_dst).unwrap();
            assert_eq!(old_dst, new_dst);
            for round in 0..3 {
                let (mut old_samples, mut new_samples) = (Vec::new(), Vec::new());
                for batch in 0..7 {
                    let (mut old_time, mut new_time) = (0.0, 0.0);
                    for image in 0..10 {
                        for old in if (round + batch + image) % 2 == 0 { [true, false] } else { [false, true] } {
                            let start = Instant::now();
                            if old {
                                legacy::decode_region(old_format, black_box(&old_surface), width, height, black_box(&mut new_dst)).unwrap();
                                old_time += start.elapsed().as_secs_f64();
                                black_box(&new_dst);
                            } else {
                                decode_region(format, black_box(&surface), width, height, black_box(&mut new_dst)).unwrap();
                                new_time += start.elapsed().as_secs_f64();
                                black_box(&new_dst);
                            }
                        }
                    }
                    old_samples.push(old_time / 10.0);
                    new_samples.push(new_time / 10.0);
                }
                old_samples.sort_by(f64::total_cmp);
                new_samples.sort_by(f64::total_cmp);
                println!("interleaved {format:?} round {}: old {:.3} ms; new {:.3} ms", round + 1, old_samples[3] * 1000.0, new_samples[3] * 1000.0);
                assert_eq!(old_dst, new_dst);
            }
        }
    }

    #[test]
    fn unsigned_regions_preserve_the_pinned_path_including_errors_and_edges() {
        for (format, old_format) in [(EtcFormat::R11, legacy::EtcFormat::R11),
                                     (EtcFormat::Rg11, legacy::EtcFormat::Rg11)] {
            for (width, height) in [(0usize, 0usize), (0, 9), (1, 1), (5, 9), (16, 8)] {
                let row_blocks = width.div_ceil(4) + 1;
                let rows_of_blocks = height.div_ceil(4) + 1;
                let data: Vec<_> = (0..row_blocks * rows_of_blocks * format.block_bytes()).map(|i| i as u8).collect();
                for (short_source, short_dest) in [(false, false), (true, false), (false, true)] {
                    let bytes = if short_source { &data[..0] } else { &data[..] };
                    let surface = Surface { data: bytes, row_blocks, rows_of_blocks };
                    let old_surface = legacy::Surface { data: bytes, row_blocks, rows_of_blocks };
                    let size = width * height * format.texel_bytes();
                    let mut out = vec![0xa5; if short_dest { size.saturating_sub(1) } else { size + 7 }];
                    let mut expected = out.clone();
                    assert_eq!(decode_region(format, &surface, width, height, &mut out),
                               legacy::decode_region(old_format, &old_surface, width, height, &mut expected));
                    assert_eq!(out, expected);
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn microbench_decode() {
        use std::hint::black_box;
        use std::time::Instant;

        let (width, height) = (1024usize, 1024usize);
        for format in [EtcFormat::Rgb8, EtcFormat::Rgba8, EtcFormat::R11,
                       EtcFormat::R11Signed, EtcFormat::Rg11, EtcFormat::Rg11Signed] {
            for pattern in ["random", "flat", "runs"] {
                let bb = format.block_bytes();
                let mut data = vec![0u8; width * height / 16 * bb];
                let mut seed = 0x1234_5678u32;
                let mut block = [0u8; 16];
                for (i, out) in data.chunks_mut(bb).enumerate() {
                    if i == 0 || pattern == "random" || (pattern == "runs" && i % 8 == 0) {
                        for b in &mut block {
                            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                            *b = (seed >> 24) as u8;
                        }
                    }
                    out.copy_from_slice(&block[..bb]);
                }
                let surface = Surface { data: &data, row_blocks: width / 4, rows_of_blocks: height / 4 };
                let mut dst = vec![0; width * height * format.texel_bytes()];
                decode_region(format, &surface, width, height, &mut dst).unwrap();
                let mut samples = Vec::new();
                for _ in 0..5 {
                    let start = Instant::now();
                    for _ in 0..10 {
                        decode_region(format, black_box(&surface), width, height, black_box(&mut dst)).unwrap();
                        black_box(&dst);
                    }
                    samples.push(start.elapsed().as_secs_f64() / 10.0);
                }
                samples.sort_by(f64::total_cmp);
                println!("decode {format:?} {pattern}: {:.3} ms/image", samples[2] * 1000.0);
            }
        }
    }

    type Vector = (&'static str, &'static str, &'static [u8], &'static [u8]);

    const VECTORS: &[Vector] = &[
        ("rgb8", "H", &[0xf5, 0x4, 0xd8, 0xaf, 0x0, 0x36, 0x35, 0xed], &[174, 106, 0, 255, 251, 81, 149, 255, 174, 106, 0, 255, 174, 106, 0, 255, 251, 81, 149, 255, 123, 0, 21, 255, 255, 234, 81, 255, 174, 106, 0, 255, 123, 0, 21, 255, 174, 106, 0, 255, 174, 106, 0, 255, 255, 234, 81, 255, 174, 106, 0, 255, 174, 106, 0, 255, 255, 234, 81, 255, 255, 234, 81, 255]),
        ("rgb8", "T", &[0xfb, 0x65, 0xf6, 0x73, 0xa7, 0xbd, 0x9d, 0xa6], &[255, 102, 119, 255, 255, 102, 119, 255, 249, 96, 113, 255, 255, 108, 125, 255, 255, 108, 125, 255, 249, 96, 113, 255, 255, 102, 119, 255, 255, 102, 119, 255, 249, 96, 113, 255, 255, 102, 85, 255, 249, 96, 113, 255, 255, 102, 85, 255, 255, 102, 119, 255, 249, 96, 113, 255, 255, 108, 125, 255, 249, 96, 113, 255]),
        ("rgb8", "differential", &[0x73, 0xdd, 0x8f, 0xdb, 0xec, 0xc7, 0x77, 0x73], &[9, 116, 34, 255, 221, 255, 246, 255, 221, 255, 246, 255, 221, 255, 246, 255, 9, 116, 34, 255, 221, 255, 246, 255, 221, 255, 246, 255, 9, 116, 34, 255, 107, 165, 99, 255, 34, 92, 26, 255, 34, 92, 26, 255, 34, 92, 26, 255, 173, 231, 165, 255, 107, 165, 99, 255, 107, 165, 99, 255, 107, 165, 99, 255]),
        ("rgb8", "individual", &[0x82, 0xda, 0x96, 0x30, 0x2f, 0xcd, 0x83, 0x79], &[119, 204, 136, 255, 153, 238, 170, 255, 0, 110, 42, 255, 52, 188, 120, 255, 141, 226, 158, 255, 153, 238, 170, 255, 0, 110, 42, 255, 16, 152, 84, 255, 131, 216, 148, 255, 119, 204, 136, 255, 16, 152, 84, 255, 52, 188, 120, 255, 119, 204, 136, 255, 131, 216, 148, 255, 16, 152, 84, 255, 94, 230, 162, 255]),
        ("rgb8", "planar", &[0xb6, 0x10, 0xf3, 0xfa, 0x46, 0xd2, 0x2b, 0x1c], &[109, 16, 93, 255, 143, 30, 96, 255, 176, 43, 99, 255, 210, 57, 102, 255, 99, 34, 98, 255, 133, 48, 101, 255, 166, 61, 104, 255, 200, 75, 107, 255, 89, 52, 103, 255, 123, 66, 106, 255, 156, 79, 109, 255, 190, 93, 112, 255, 79, 70, 108, 255, 113, 84, 111, 255, 146, 97, 114, 255, 180, 111, 117, 255]),
        ("rgb8a1", "differential opaque=0", &[0x73, 0x78, 0x46, 0xec, 0x36, 0xd4, 0x69, 0x61], &[255, 255, 249, 255, 0, 0, 0, 0, 182, 165, 91, 255, 0, 0, 0, 0, 115, 123, 66, 255, 255, 255, 249, 255, 0, 0, 0, 0, 98, 81, 7, 255, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 0, 182, 165, 91, 255, 115, 123, 66, 255, 0, 0, 0, 0, 182, 165, 91, 255, 140, 123, 49, 255]),
        ("rgb8a1", "T opaque=0", &[0xfb, 0x9f, 0x73, 0x50, 0x13, 0xd4, 0x8, 0x47], &[122, 54, 88, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 122, 54, 88, 255, 255, 153, 255, 255, 0, 0, 0, 0, 255, 153, 255, 255, 116, 48, 82, 255, 116, 48, 82, 255, 255, 153, 255, 255, 255, 153, 255, 255, 255, 153, 255, 255, 0, 0, 0, 0, 122, 54, 88, 255, 255, 153, 255, 255]),
        ("rgb8a1", "H opaque=0", &[0x89, 0xfa, 0xca, 0xec, 0x52, 0x43, 0xe8, 0xb7], &[130, 62, 198, 255, 0, 28, 198, 255, 40, 74, 244, 255, 0, 0, 0, 0, 130, 62, 198, 255, 0, 28, 198, 255, 0, 0, 0, 0, 0, 28, 198, 255, 0, 28, 198, 255, 0, 0, 0, 0, 40, 74, 244, 255, 130, 62, 198, 255, 40, 74, 244, 255, 0, 28, 198, 255, 0, 28, 198, 255, 0, 28, 198, 255]),
        ("rgb8a1", "differential opaque=1", &[0x8c, 0x6b, 0xc4, 0x46, 0x7c, 0xeb, 0x7, 0x37], &[111, 78, 169, 255, 169, 136, 227, 255, 124, 149, 182, 255, 102, 127, 160, 255, 111, 78, 169, 255, 111, 78, 169, 255, 124, 149, 182, 255, 102, 127, 160, 255, 169, 136, 227, 255, 131, 98, 189, 255, 90, 115, 148, 255, 102, 127, 160, 255, 131, 98, 189, 255, 131, 98, 189, 255, 102, 127, 160, 255, 112, 137, 170, 255]),
        ("rgb8a1", "T opaque=1", &[0xf2, 0xe1, 0x7e, 0x96, 0xf2, 0xb0, 0x16, 0xc2], &[170, 238, 17, 255, 119, 238, 153, 255, 170, 238, 17, 255, 108, 227, 142, 255, 130, 249, 164, 255, 119, 238, 153, 255, 108, 227, 142, 255, 119, 238, 153, 255, 170, 238, 17, 255, 130, 249, 164, 255, 130, 249, 164, 255, 119, 238, 153, 255, 170, 238, 17, 255, 108, 227, 142, 255, 170, 238, 17, 255, 119, 238, 153, 255]),
        ("rgb8a1", "H opaque=1", &[0x4b, 0x6, 0x3, 0xc7, 0x3e, 0x95, 0xa, 0xa2], &[64, 183, 200, 255, 64, 183, 200, 255, 217, 166, 132, 255, 64, 183, 200, 255, 89, 38, 4, 255, 89, 38, 4, 255, 0, 55, 72, 255, 64, 183, 200, 255, 64, 183, 200, 255, 217, 166, 132, 255, 64, 183, 200, 255, 217, 166, 132, 255, 217, 166, 132, 255, 0, 55, 72, 255, 0, 55, 72, 255, 217, 166, 132, 255]),
        ("rgba8", "eac alpha", &[0x28, 0x68, 0xc1, 0xaf, 0x63, 0xd, 0xe5, 0x50, 0xbf, 0x65, 0xc, 0x94, 0xfd, 0xb9, 0xf1, 0x51], &[127, 42, 0, 82, 127, 42, 0, 94, 175, 5, 124, 28, 175, 5, 124, 0, 205, 120, 18, 28, 169, 84, 0, 70, 255, 109, 228, 0, 175, 5, 124, 70, 205, 120, 18, 0, 247, 162, 60, 46, 231, 61, 180, 0, 175, 5, 124, 0, 169, 84, 0, 0, 169, 84, 0, 0, 231, 61, 180, 82, 175, 5, 124, 28]),
        ("r11", "mult0=False", &[0x12, 0xcc, 0xe7, 0x39, 0x6d, 0xb8, 0x7a, 0x40], &[126, 0, 42, 0, 54, 0, 54, 0, 0, 0, 54, 0, 90, 0, 0, 0, 90, 0, 54, 0, 0, 0, 0, 0, 0, 0, 54, 0, 126, 0, 0, 0]),
        ("r11", "mult0=True", &[0xf9, 0x1, 0x3e, 0xb, 0x85, 0x19, 0xef, 0xcf], &[248, 0, 250, 0, 249, 0, 251, 0, 251, 0, 250, 0, 250, 0, 251, 0, 249, 0, 249, 0, 247, 0, 248, 0, 249, 0, 250, 0, 250, 0, 251, 0]),
        ("r11s", "mult0=False", &[0xb9, 0xe8, 0x77, 0x24, 0xfa, 0xf5, 0xd1, 0x3e], &[0, 0, 0, 0, 182, 0, 28, 0, 126, 0, 0, 0, 126, 0, 70, 0, 154, 0, 182, 0, 0, 0, 182, 0, 0, 0, 0, 0, 126, 0, 154, 0]),
        ("r11s", "mult0=True", &[0x9a, 0x4, 0x1d, 0xc, 0x9f, 0x6a, 0x74, 0x1e], &[25, 0, 26, 0, 24, 0, 24, 0, 27, 0, 24, 0, 24, 0, 25, 0, 24, 0, 24, 0, 26, 0, 24, 0, 25, 0, 27, 0, 27, 0, 26, 0]),
        ("rg11", "mult0=False", &[0x34, 0xdc, 0xd, 0x5b, 0x87, 0x27, 0x1f, 0x5d, 0x71, 0x23, 0xaa, 0x6a, 0x74, 0x99, 0x43, 0xa3], &[13, 119, 91, 119, 0, 115, 169, 105, 0, 101, 130, 105, 0, 123, 91, 123, 0, 115, 13, 123, 130, 101, 0, 115, 91, 123, 169, 115, 0, 115, 91, 87]),
        ("rg11", "mult0=True", &[0x95, 0xe, 0x6b, 0x5d, 0xc7, 0xbe, 0x87, 0xd0, 0xff, 0x3, 0xf3, 0x4a, 0xbb, 0x20, 0x3d, 0x7b], &[148, 255, 150, 255, 150, 255, 148, 255, 148, 255, 150, 254, 150, 255, 150, 255, 150, 255, 149, 255, 150, 255, 148, 255, 150, 255, 150, 253, 149, 253, 149, 253]),
        ("rg11s", "mult0=False", &[0x1d, 0x80, 0xe4, 0x4f, 0xc8, 0x81, 0x9f, 0xd8, 0x5b, 0x46, 0x45, 0x9c, 0xaf, 0xb9, 0x90, 0xb2], &[255, 186, 255, 246, 172, 242, 255, 202, 108, 190, 255, 186, 132, 246, 255, 186, 132, 174, 108, 242, 36, 174, 36, 246, 172, 190, 132, 255, 108, 190, 132, 186]),
        ("rg11s", "mult0=True", &[0x97, 0x0, 0xa2, 0x47, 0xc0, 0x3d, 0x30, 0xf3, 0x3f, 0x0, 0xfa, 0xa7, 0xea, 0x32, 0xe0, 0xae], &[23, 192, 21, 189, 22, 190, 22, 190, 22, 191, 24, 192, 24, 191, 21, 189, 23, 191, 22, 191, 21, 191, 23, 191, 23, 189, 22, 189, 21, 191, 21, 191]),
    ];

    fn format_of(kind: &str) -> EtcFormat {
        match kind {
            "rgb8" => EtcFormat::Rgb8,
            "rgb8a1" => EtcFormat::Rgb8A1,
            "rgba8" => EtcFormat::Rgba8,
            "r11" => EtcFormat::R11,
            "r11s" => EtcFormat::R11Signed,
            "rg11" => EtcFormat::Rg11,
            _ => EtcFormat::Rg11Signed,
        }
    }

    fn eac_to_8bit(v: u16, signed: bool) -> i32 {
        if signed {
            let s = i32::from(v as i16);
            let v11 = if s < 0 { -((-s) >> 5) } else { s >> 5 };
            (v11 + 1023) * 255 / 2046
        } else {
            i32::from(v >> 5) >> 3
        }
    }

    #[test]
    fn every_mode_matches_an_independent_decoder() {
        for &(kind, mode, block, want) in VECTORS {
            let format = format_of(kind);
            let mut out = [0u8; 64];
            decode_block(format, block, &mut out);
            match format {
                EtcFormat::Rgb8 | EtcFormat::Rgb8A1 | EtcFormat::Rgba8 => {
                    assert_eq!(&out[..], want, "{kind} {mode}");
                }
                _ => {
                    let signed = matches!(format, EtcFormat::R11Signed | EtcFormat::Rg11Signed);
                    let channels = format.texel_bytes() / 2;
                    for px in 0..16 {
                        for c in 0..channels {
                            let o = px * format.texel_bytes() + c * 2;
                            let got = eac_to_8bit(u16::from_le_bytes([out[o], out[o + 1]]), signed);
                            let exp = i32::from(want[px * 2 + c]);
                            assert!((got - exp).abs() <= 1, "{kind} {mode} pixel {px} channel {c}: {got} vs {exp}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn eac_extends_to_sixteen_bits_at_both_ends() {
        let mut out = [0u16; 16];
        decode_eac11(&[0xff, 0x1f, 0xdb, 0x6d, 0xb6, 0xdb, 0x6d, 0xb6], false, &mut out);
        assert!(out.iter().all(|&v| v == 0xffff));
        decode_eac11(&[0x00, 0x10, 0, 0, 0, 0, 0, 0], false, &mut out);
        assert!(out.iter().all(|&v| v == 0));
        decode_eac11(&[0x81, 0x13, 0x49, 0x24, 0x92, 0x49, 0x24, 0x92], true, &mut out);
        assert!(out.iter().all(|&v| v as i16 == -32767), "{out:?}");
    }

    #[test]
    fn partial_blocks_at_small_mips_are_cropped_not_overrun() {
        let block = [0x82, 0xda, 0x96, 0x30, 0x2f, 0xcd, 0x83, 0x79];
        let mut full = [0u8; 64];
        decode_block(EtcFormat::Rgb8, &block, &mut full);
        let src = Surface { data: &block, row_blocks: 1, rows_of_blocks: 1 };
        let mut two = [0u8; 2 * 2 * 4];
        decode_region(EtcFormat::Rgb8, &src, 2, 2, &mut two).unwrap();
        assert_eq!(&two[0..8], &full[0..8]);
        assert_eq!(&two[8..16], &full[16..24]);
        let mut one = [0u8; 4];
        decode_region(EtcFormat::Rgb8, &src, 1, 1, &mut one).unwrap();
        assert_eq!(&one, &full[0..4]);
    }

    #[test]
    fn row_length_wider_than_the_region_is_respected() {
        let a = [0x82, 0xda, 0x96, 0x30, 0x2f, 0xcd, 0x83, 0x79];
        let b = [0x73, 0xdd, 0x8f, 0xdb, 0xec, 0xc7, 0x77, 0x73];
        let data: Vec<u8> = [a, b, b, a].concat();
        let src = Surface { data: &data, row_blocks: 2, rows_of_blocks: 2 };
        let mut out = vec![0u8; 4 * 8 * 4];
        decode_region(EtcFormat::Rgb8, &src, 4, 8, &mut out).unwrap();
        let mut fa = [0u8; 64];
        let mut fb = [0u8; 64];
        decode_block(EtcFormat::Rgb8, &a, &mut fa);
        decode_block(EtcFormat::Rgb8, &b, &mut fb);
        assert_eq!(&out[0..64], &fa[..]);
        assert_eq!(&out[64..128], &fb[..]);
    }

    #[test]
    fn a_short_source_is_an_error_not_a_panic() {
        let src = Surface { data: &[0u8; 8], row_blocks: 2, rows_of_blocks: 2 };
        let mut out = vec![0u8; 8 * 8 * 4];
        assert!(decode_region(EtcFormat::Rgb8, &src, 8, 8, &mut out).is_err());
    }

    #[test]
    fn region_rows_and_repeated_blocks_match_per_texel_copies() {
        for format in [EtcFormat::Rgb8, EtcFormat::Rgb8A1, EtcFormat::Rgba8,
                       EtcFormat::R11, EtcFormat::R11Signed, EtcFormat::Rg11, EtcFormat::Rg11Signed] {
            for width in 1usize..=9 {
                for height in 1usize..=9 {
                    let bb = format.block_bytes();
                    let tb = format.texel_bytes();
                    let row_blocks = width.div_ceil(4) + 1;
                    let rows_of_blocks = height.div_ceil(4);
                    let mut data = vec![0; row_blocks * rows_of_blocks * bb];
                    let mut seed = 0x1234_5678u32;
                    let mut block = [0; 16];
                    for (i, out) in data.chunks_mut(bb).enumerate() {
                        if i % 3 == 0 {
                            for b in &mut block {
                                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                                *b = (seed >> 24) as u8;
                            }
                        }
                        out.copy_from_slice(&block[..bb]);
                    }
                    let surface = Surface { data: &data, row_blocks, rows_of_blocks };
                    let len = width * height * tb;
                    let mut dst = vec![0xa5; len + 16];
                    decode_region(format, &surface, width, height, &mut dst).unwrap();
                    let mut expected = vec![0; len];
                    let mut pixels = [0; 64];
                    for by in 0..height.div_ceil(4) {
                        for bx in 0..width.div_ceil(4) {
                            let at = (by * row_blocks + bx) * bb;
                            decode_block(format, &data[at..at + bb], &mut pixels);
                            for y in 0..(height - by * 4).min(4) {
                                for x in 0..(width - bx * 4).min(4) {
                                    let s = (y * 4 + x) * tb;
                                    let d = ((by * 4 + y) * width + bx * 4 + x) * tb;
                                    expected[d..d + tb].copy_from_slice(&pixels[s..s + tb]);
                                }
                            }
                        }
                    }
                    assert_eq!(dst[..len], expected, "{format:?} {width}x{height}");
                    assert_eq!(dst[len..], [0xa5; 16]);
                }
            }
        }
    }

    #[test]
    fn empty_regions_and_invalid_extents() {
        let empty = Surface { data: &[], row_blocks: 0, rows_of_blocks: 0 };
        assert!(decode_region(EtcFormat::Rgb8, &empty, 0, 4, &mut []).is_ok());
        assert!(decode_region(EtcFormat::Rgb8, &empty, 4, 0, &mut []).is_ok());
        assert!(decode_region(EtcFormat::Rgb8, &empty, 4, 4, &mut [0; 64]).is_err());
        let full = Surface { data: &[0; 8], row_blocks: 1, rows_of_blocks: 1 };
        assert!(decode_region(EtcFormat::Rgb8, &full, 4, 4, &mut [0; 63]).is_err());
    }

    #[test]
    fn repeated_blocks_across_cropped_edges_keep_all_texels() {
        for format in [EtcFormat::Rgb8, EtcFormat::Rgb8A1, EtcFormat::Rgba8,
                       EtcFormat::R11, EtcFormat::R11Signed, EtcFormat::Rg11, EtcFormat::Rg11Signed] {
            let block = [0x82, 0xda, 0x96, 0x30, 0x2f, 0xcd, 0x83, 0x79,
                         0x73, 0xdd, 0x8f, 0xdb, 0xec, 0xc7, 0x77, 0x73];
            let data = block[..format.block_bytes()].repeat(6);
            let surface = Surface { data: &data, row_blocks: 2, rows_of_blocks: 3 };
            let tb = format.texel_bytes();
            let mut pixels = [0; 64];
            decode_block(format, &block, &mut pixels);
            let mut out = vec![0; 5 * 9 * tb];
            decode_region(format, &surface, 5, 9, &mut out).unwrap();
            for y in 0..9 {
                for x in 0..5 {
                    let from = (y % 4 * 4 + x % 4) * tb;
                    let at = (y * 5 + x) * tb;
                    assert_eq!(out[at..at + tb], pixels[from..from + tb], "{format:?} {x},{y}");
                }
            }
        }
    }

    #[test]
    fn palettes_match_per_pixel_clamping() {
        let mut seed = 0x1234_5678u32;
        for i in 0..2048 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let lo = seed;
            let b3 = i as u8;
            let opaque = i & 256 == 0;
            let bases = [[(seed & 255) as i32, ((seed >> 8) & 255) as i32, ((seed >> 16) & 255) as i32],
                         [((seed >> 24) & 255) as i32, 255, 0]];
            let mut out = [[0; 4]; 16];
            subblocks(bases[0], bases[1], b3, lo, opaque, &mut out);
            for y in 0..4 {
                for x in 0..4 {
                    let second = if b3 & 1 != 0 { y >= 2 } else { x >= 2 };
                    let base = bases[usize::from(second)];
                    let table = INTENSITY[if second { usize::from((b3 >> 2) & 7) } else { usize::from(b3 >> 5) }];
                    let idx = pixel_index(lo, x, y);
                    let m = match idx {
                        0 if !opaque => 0,
                        0 => table[0],
                        1 => table[1],
                        2 => -table[0],
                        _ => -table[1],
                    };
                    let expected = if !opaque && idx == 2 { [0; 4] } else {
                        [clamp8(base[0] + m), clamp8(base[1] + m), clamp8(base[2] + m), 255]
                    };
                    assert_eq!(out[y * 4 + x], expected);
                }
            }
            let paint = [add(bases[0], -183), add(bases[0], 183), bases[1], add(bases[1], 183)];
            paint_block(&paint, lo, opaque, &mut out);
            for y in 0..4 {
                for x in 0..4 {
                    let idx = pixel_index(lo, x, y);
                    let p = paint[idx];
                    let expected = if !opaque && idx == 2 { [0; 4] } else {
                        [clamp8(p[0]), clamp8(p[1]), clamp8(p[2]), 255]
                    };
                    assert_eq!(out[y * 4 + x], expected);
                }
            }
        }
    }

    #[test]
    fn bc4_reciprocal_quantize_is_bit_exact_for_all_spans() {
        for span in 1u32..=255 {
            let inv = bc4_quantize_recip(span);
            for delta in 0..=span {
                let exact = (delta * 7 + (span >> 1)) / span;
                let fast = bc4_quantize(delta, inv);
                assert_eq!(fast, exact, "span={span} delta={delta} inv={inv}");
            }
        }
    }

    #[test]
    fn bc1_simd_matches_scalar_bit_for_bit() {
        let mut seed = 0x9e37_79b9u32;
        for i in 0..4096 {
            let mut rgba = [0u8; 64];
            for b in &mut rgba {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (seed >> 24) as u8;
            }
            if i % 8 == 0 {
                // Degenerate single-color block.
                let px = [rgba[0], rgba[1], rgba[2], 255];
                for chunk in rgba.chunks_exact_mut(4) {
                    chunk.copy_from_slice(&px);
                }
            }
            for punchthrough in [false, true] {
                if punchthrough && i % 3 == 0 {
                    rgba[3] = 0;
                    rgba[19] = 0;
                }
                let mut simd_out = [0u8; 8];
                let mut scalar_out = [0u8; 8];
                encode_bc1_block(&rgba, punchthrough, &mut simd_out);
                encode_bc1_block_scalar(&rgba, punchthrough, &mut scalar_out);
                assert_eq!(simd_out, scalar_out, "block {i} punchthrough={punchthrough}");
            }
        }
    }
}
