const GRADIENT_SIZE: usize = 6 * 256;

const FONT_W: usize = 5;
const FONT_H: usize = 7;

const fn glyph(ch: u8) -> [u8; FONT_H] {
    match ch {
        b'0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        b'1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        b'2' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
        ],
        b'3' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110,
        ],
        b'4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        b'5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        b'6' => [
            0b01110, 0b10001, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        b'7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        b'8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        b'9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b10001, 0b01110,
        ],
        b':' => [
            0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000,
        ],
        b'.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        b'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        b' ' => [0; FONT_H],
        _ => [
            0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111,
        ],
    }
}

pub fn render(
    width: usize,
    height: usize,
    frame_index: u64,
    fps: u32,
    y_plane: &mut [u8],
    u_plane: &mut [u8],
    v_plane: &mut [u8],
) {
    let mut rgb = vec![0u8; width * height * 3];

    fill_bars_and_circle(width, height, &mut rgb);
    fill_gradient(width, height, frame_index, fps, &mut rgb);

    let seg_size = width / 80;
    if seg_size >= 1 && height >= 13 * seg_size {
        fill_seven_seg_counter(width, height, frame_index, fps, seg_size, &mut rgb);
    }

    fill_text_overlay(width, height, frame_index, fps, &mut rgb);

    rgb_to_yuv420(width, height, &rgb, y_plane, u_plane, v_plane);
}

fn fill_bars_and_circle(width: usize, height: usize, rgb: &mut [u8]) {
    let w = width as i64;
    let h = height as i64;
    let radius = (w + h) / 4;
    let mut quad0 = w * w / 4 + h * h / 4 - radius * radius;
    let mut dquad_y: i64 = 1 - h;

    for y in 0..height {
        let mut color: usize = 0;
        let mut color_rest: usize = 0;
        let mut quad = quad0;
        let mut dquad_x: i64 = 1 - w;

        for x in 0..width {
            let mut icolor = color;
            if quad < 0 {
                icolor ^= 7;
            }
            quad += dquad_x;
            dquad_x += 2;

            let offset = (y * width + x) * 3;
            rgb[offset] = if icolor & 1 != 0 { 255 } else { 0 };
            rgb[offset + 1] = if icolor & 2 != 0 { 255 } else { 0 };
            rgb[offset + 2] = if icolor & 4 != 0 { 255 } else { 0 };

            color_rest += 8;
            if color_rest >= width {
                color_rest -= width;
                color += 1;
            }
        }
        quad0 += dquad_y;
        dquad_y += 2;
    }
}

fn gradient_r(grad: usize) -> u8 {
    if !(256..5 * 256).contains(&grad) {
        255
    } else if (2 * 256..4 * 256).contains(&grad) {
        0
    } else if grad < 2 * 256 {
        (2 * 256 - 1 - grad) as u8
    } else {
        (grad - 4 * 256) as u8
    }
}

fn gradient_g(grad: usize) -> u8 {
    if grad >= 4 * 256 {
        0
    } else if (256..3 * 256).contains(&grad) {
        255
    } else if grad < 256 {
        grad as u8
    } else {
        (4 * 256 - 1 - grad) as u8
    }
}

fn gradient_b(grad: usize) -> u8 {
    if grad < 2 * 256 {
        0
    } else if (3 * 256..5 * 256).contains(&grad) {
        255
    } else if grad < 3 * 256 {
        (grad - 2 * 256) as u8
    } else {
        (6 * 256 - 1 - grad) as u8
    }
}

fn fill_gradient(width: usize, height: usize, frame_index: u64, fps: u32, rgb: &mut [u8]) {
    let grad_y = height * 3 / 4;
    let grad_h = height / 8;
    if grad_y >= height {
        return;
    }

    let grad_start = if fps > 0 {
        ((256 * frame_index / fps as u64) % GRADIENT_SIZE as u64) as usize
    } else {
        0
    };

    for x in 0..width {
        let grad = (GRADIENT_SIZE * x / width + grad_start) % GRADIENT_SIZE;
        let offset = (grad_y * width + x) * 3;
        rgb[offset] = gradient_r(grad);
        rgb[offset + 1] = gradient_g(grad);
        rgb[offset + 2] = gradient_b(grad);
    }

    let src_start = grad_y * width * 3;
    let row_bytes = width * 3;
    for dy in 1..grad_h {
        let dst_row = grad_y + dy;
        if dst_row >= height {
            break;
        }
        let dst_start = dst_row * width * 3;
        rgb.copy_within(src_start..src_start + row_bytes, dst_start);
    }
}

struct Segment {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

const SEGMENTS: [Segment; 7] = [
    Segment {
        x: 1,
        y: 0,
        w: 5,
        h: 1,
    },
    Segment {
        x: 1,
        y: 6,
        w: 5,
        h: 1,
    },
    Segment {
        x: 1,
        y: 12,
        w: 5,
        h: 1,
    },
    Segment {
        x: 0,
        y: 1,
        w: 1,
        h: 5,
    },
    Segment {
        x: 0,
        y: 7,
        w: 1,
        h: 5,
    },
    Segment {
        x: 6,
        y: 1,
        w: 1,
        h: 5,
    },
    Segment {
        x: 6,
        y: 7,
        w: 1,
        h: 5,
    },
];

const DIGIT_MASKS: [u8; 10] = [
    0b1111101, // 0
    0b1100000, // 1
    0b0110111, // 2
    0b1100111, // 3
    0b1101010, // 4
    0b1001111, // 5
    0b1011111, // 6
    0b1100001, // 7
    0b1111111, // 8
    0b1101111, // 9
];

#[allow(clippy::too_many_arguments)]
fn fill_rect_rgb(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    val: u8,
    px: usize,
    py: usize,
    pw: usize,
    ph: usize,
) {
    for row in py..(py + ph).min(height) {
        for col in px..(px + pw).min(width) {
            let offset = (row * width + col) * 3;
            rgb[offset] = val;
            rgb[offset + 1] = val;
            rgb[offset + 2] = val;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_seven_seg_digit(
    digit: usize,
    rgb: &mut [u8],
    width: usize,
    height: usize,
    bx: usize,
    by: usize,
    seg_size: usize,
) {
    fill_rect_rgb(rgb, width, height, 0, bx, by, 8 * seg_size, 13 * seg_size);

    let mask = DIGIT_MASKS[digit];
    for (i, seg) in SEGMENTS.iter().enumerate() {
        if mask & (1 << i) != 0 {
            fill_rect_rgb(
                rgb,
                width,
                height,
                255,
                bx + seg.x * seg_size,
                by + seg.y * seg_size,
                seg.w * seg_size,
                seg.h * seg_size,
            );
        }
    }
}

fn fill_seven_seg_counter(
    width: usize,
    height: usize,
    frame_index: u64,
    fps: u32,
    seg_size: usize,
    rgb: &mut [u8],
) {
    let mut second = if fps > 0 {
        frame_index / fps as u64
    } else {
        frame_index
    };

    let digit_w = 8 * seg_size;
    let digit_h = 13 * seg_size;
    let x_right = width - (width - seg_size * 64) / 2;
    let y = (height.saturating_sub(digit_h)) / 2;

    let mut x = x_right;
    for _ in 0..8 {
        if x < digit_w {
            break;
        }
        x -= digit_w;
        draw_seven_seg_digit((second % 10) as usize, rgb, width, height, x, y, seg_size);
        second /= 10;
        if second == 0 {
            break;
        }
    }
}

fn fill_text_overlay(width: usize, height: usize, frame_index: u64, fps: u32, rgb: &mut [u8]) {
    let scale = (width.min(height) / 180).clamp(1, 8);
    let char_w = (FONT_W + 1) * scale;
    let char_h = FONT_H * scale;
    let gap = scale;
    let pad = scale * 2;

    let total_secs = frame_index / fps.max(1) as u64;
    let hh = (total_secs / 3600) % 100;
    let mm = (total_secs % 3600) / 60;
    let ss = total_secs % 60;
    let frac = if fps > 0 {
        (frame_index % fps as u64) * 1000 / fps as u64
    } else {
        0
    };

    let ts: [u8; 12] = [
        b'0' + (hh / 10) as u8,
        b'0' + (hh % 10) as u8,
        b':',
        b'0' + (mm / 10) as u8,
        b'0' + (mm % 10) as u8,
        b':',
        b'0' + (ss / 10) as u8,
        b'0' + (ss % 10) as u8,
        b'.',
        b'0' + (frac / 100) as u8,
        b'0' + ((frac / 10) % 10) as u8,
        b'0' + (frac % 10) as u8,
    ];

    let fc = format_frame_label(frame_index);

    let text_cols = 12.max(fc.len());
    let rect_w = text_cols * char_w + pad * 2;
    let rect_h = 2 * char_h + gap + pad * 2;
    let rx = pad;
    let ry = pad;

    fill_rect_rgb_color(rgb, width, height, 0, 0, 0, rx, ry, rect_w, rect_h);

    let border_val: u8 = 128;
    for col in rx..(rx + rect_w).min(width) {
        set_rgb(
            rgb, width, height, ry, col, border_val, border_val, border_val,
        );
        let bot = ry + rect_h - 1;
        if bot < height {
            set_rgb(
                rgb, width, height, bot, col, border_val, border_val, border_val,
            );
        }
    }
    for row in ry..(ry + rect_h).min(height) {
        set_rgb(
            rgb, width, height, row, rx, border_val, border_val, border_val,
        );
        let right = rx + rect_w - 1;
        if right < width {
            set_rgb(
                rgb, width, height, row, right, border_val, border_val, border_val,
            );
        }
    }

    let tx = rx + pad;
    let ty = ry + pad;
    render_text_rgb(&ts, tx, ty, scale, width, height, rgb, 255, 255, 255);

    let fy = ty + char_h + gap;
    render_text_rgb(&fc, tx, fy, scale, width, height, rgb, 200, 200, 200);
}

fn format_frame_label(frame_index: u64) -> [u8; 9] {
    let mut buf = [b'0'; 9];
    buf[0] = b'F';
    let mut val = frame_index;
    for i in (1..9).rev() {
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    buf
}

#[allow(clippy::too_many_arguments)]
const fn set_rgb(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    row: usize,
    col: usize,
    r: u8,
    g: u8,
    b: u8,
) {
    if row < height && col < width {
        let offset = (row * width + col) * 3;
        rgb[offset] = r;
        rgb[offset + 1] = g;
        rgb[offset + 2] = b;
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect_rgb_color(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    r: u8,
    g: u8,
    b: u8,
    px: usize,
    py: usize,
    pw: usize,
    ph: usize,
) {
    for row in py..(py + ph).min(height) {
        for col in px..(px + pw).min(width) {
            let offset = (row * width + col) * 3;
            rgb[offset] = r;
            rgb[offset + 1] = g;
            rgb[offset + 2] = b;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_text_rgb(
    text: &[u8],
    x: usize,
    y: usize,
    scale: usize,
    width: usize,
    height: usize,
    rgb: &mut [u8],
    r: u8,
    g: u8,
    b: u8,
) {
    let char_w = (FONT_W + 1) * scale;
    for (i, &ch) in text.iter().enumerate() {
        let gl = glyph(ch);
        let bx = x + i * char_w;
        for (gy, gl_row) in gl.iter().enumerate().take(FONT_H) {
            for gx in 0..FONT_W {
                if gl_row & (1 << (FONT_W - 1 - gx)) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = bx + gx * scale + sx;
                            let py = y + gy * scale + sy;
                            set_rgb(rgb, width, height, py, px, r, g, b);
                        }
                    }
                }
            }
        }
    }
}

fn rgb_to_yuv420(
    width: usize,
    height: usize,
    rgb: &[u8],
    y_plane: &mut [u8],
    u_plane: &mut [u8],
    v_plane: &mut [u8],
) {
    let uv_w = width / 2;

    for row in 0..height {
        for col in 0..width {
            let offset = (row * width + col) * 3;
            let r = rgb[offset] as f32;
            let g = rgb[offset + 1] as f32;
            let b = rgb[offset + 2] as f32;

            y_plane[row * width + col] = 0.114f32
                .mul_add(b, 0.299f32.mul_add(r, 0.587 * g))
                .clamp(0.0, 255.0) as u8;

            if row % 2 == 0 && col % 2 == 0 {
                let i = (row / 2) * uv_w + (col / 2);
                u_plane[i] = (0.5f32.mul_add(b, (-0.168736f32).mul_add(r, -(0.331264 * g))) + 128.0)
                    .clamp(0.0, 255.0) as u8;
                v_plane[i] = (0.081312f32.mul_add(-b, 0.5f32.mul_add(r, -(0.418688 * g))) + 128.0)
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yuv420_to_rgb(
        width: usize,
        height: usize,
        y_plane: &[u8],
        u_plane: &[u8],
        v_plane: &[u8],
    ) -> Vec<u8> {
        let uv_w = width / 2;
        let mut rgb = vec![0u8; width * height * 3];
        for row in 0..height {
            for col in 0..width {
                let y = y_plane[row * width + col] as f32;
                let cb = u_plane[(row / 2) * uv_w + col / 2] as f32 - 128.0;
                let cr = v_plane[(row / 2) * uv_w + col / 2] as f32 - 128.0;
                let r = 1.402f32.mul_add(cr, y).clamp(0.0, 255.0) as u8;
                let g = 0.714136f32
                    .mul_add(-cr, 0.344136f32.mul_add(-cb, y))
                    .clamp(0.0, 255.0) as u8;
                let b = 1.772f32.mul_add(cb, y).clamp(0.0, 255.0) as u8;
                let i = (row * width + col) * 3;
                rgb[i] = r;
                rgb[i + 1] = g;
                rgb[i + 2] = b;
            }
        }
        rgb
    }

    #[test]
    fn render_testsrc_to_jpeg() {
        let width = 1280;
        let height = 720;
        let y_size = width * height;
        let uv_size = (width / 2) * (height / 2);

        let mut y_plane = vec![0u8; y_size];
        let mut u_plane = vec![0u8; uv_size];
        let mut v_plane = vec![0u8; uv_size];

        render(
            width,
            height,
            42,
            25,
            &mut y_plane,
            &mut u_plane,
            &mut v_plane,
        );

        let rgb = yuv420_to_rgb(width, height, &y_plane, &u_plane, &v_plane);

        let img = image::RgbImage::from_raw(width as u32, height as u32, rgb)
            .expect("failed to create image");

        let out_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target");
        std::fs::create_dir_all(&out_dir).expect("failed to create target dir");
        let out = out_dir.join("test_testsrc.jpg");
        img.save(&out).expect("failed to save JPEG");
        println!("saved to {}", out.display());

        assert!(out.exists());
        assert!(out.metadata().unwrap().len() > 1000);
    }
}
