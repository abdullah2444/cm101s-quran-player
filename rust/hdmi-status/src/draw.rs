use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};
use std::fs::File;
use unicode_bidi::BidiInfo;

pub const FONT_REG: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
pub const FONT_BOLD: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf";

#[derive(Clone, Copy)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b, 255)
    }
    pub const fn with_a(self, a: u8) -> Self {
        Self(self.0, self.1, self.2, a)
    }
}

pub struct Fonts {
    reg: Vec<u8>,
    bold: Vec<u8>,
}

impl Fonts {
    pub fn load() -> Self {
        let reg = std::fs::read(FONT_REG).expect("load DejaVuSans.ttf");
        let bold = std::fs::read(FONT_BOLD).unwrap_or_else(|_| reg.clone());
        Self { reg, bold }
    }

    pub fn get(&self, bold: bool) -> FontRef<'_> {
        FontRef::try_from_slice(if bold { &self.bold } else { &self.reg }).unwrap()
    }

    pub fn face(&self, bold: bool) -> Option<rustybuzz::Face<'_>> {
        rustybuzz::Face::from_slice(if bold { &self.bold } else { &self.reg }, 0)
    }
}

fn shape_line(fonts: &Fonts, text: &str, size: f32, bold: bool) -> (f32, f32, f32, Vec<(GlyphId, f32, f32)>) {
    let raw_font = fonts.get(bold);
    let font = raw_font.as_scaled(PxScale::from(size));
    let ascent = font.ascent();
    let height = ascent - font.descent();
    if text.is_empty() {
        return (0.0, ascent, height, Vec::new());
    }
    let Some(face) = fonts.face(bold) else {
        return (0.0, ascent, height, Vec::new());
    };
    let sf = size / raw_font.height_unscaled();
    let bidi = BidiInfo::new(text, None);
    let mut caret_x = 0.0f32;
    let mut glyphs = Vec::new();
    for para in &bidi.paragraphs {
        let (levels, runs) = bidi.visual_runs(para, para.range.clone());
        for run in runs {
            let slice = &text[run.clone()];
            if slice.is_empty() {
                continue;
            }
            let rtl = levels[run.start].is_rtl();
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(slice);
            buf.set_direction(if rtl {
                rustybuzz::Direction::RightToLeft
            } else {
                rustybuzz::Direction::LeftToRight
            });
            let shaped = rustybuzz::shape(&face, &[], buf);
            for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions().iter()) {
                let gx = caret_x + (pos.x_offset as f32) * sf;
                let gy = -(pos.y_offset as f32) * sf;
                glyphs.push((GlyphId(info.glyph_id as u16), gx, gy));
                caret_x += (pos.x_advance as f32) * sf;
            }
        }
    }
    (caret_x, ascent, height, glyphs)
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>, // BGRA little-endian (= ARGB8888)
}

impl Canvas {
    pub fn new(w: usize, h: usize, bg: Rgba) -> Self {
        let mut data = vec![0u8; w * h * 4];
        for px in data.chunks_exact_mut(4) {
            px[0] = bg.2;
            px[1] = bg.1;
            px[2] = bg.0;
            px[3] = bg.3;
        }
        Self { w, h, data }
    }

    #[inline]
    pub fn set_px(&mut self, x: i32, y: i32, c: Rgba) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let off = (y as usize * self.w + x as usize) * 4;
        self.data[off] = c.2;
        self.data[off + 1] = c.1;
        self.data[off + 2] = c.0;
        self.data[off + 3] = c.3;
    }

    #[inline]
    pub fn blend_px(&mut self, x: i32, y: i32, c: Rgba, cov: u8) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || cov == 0 {
            return;
        }
        let a = ((c.3 as u32) * (cov as u32) + 127) / 255;
        if a == 0 {
            return;
        }
        let off = (y as usize * self.w + x as usize) * 4;
        if a >= 255 {
            self.data[off] = c.2;
            self.data[off + 1] = c.1;
            self.data[off + 2] = c.0;
            self.data[off + 3] = 255;
            return;
        }
        let inv = 255 - a;
        let dst_b = self.data[off] as u32;
        let dst_g = self.data[off + 1] as u32;
        let dst_r = self.data[off + 2] as u32;
        let dst_a = self.data[off + 3] as u32;
        self.data[off] = ((c.2 as u32 * a + dst_b * inv + 127) / 255) as u8;
        self.data[off + 1] = ((c.1 as u32 * a + dst_g * inv + 127) / 255) as u8;
        self.data[off + 2] = ((c.0 as u32 * a + dst_r * inv + 127) / 255) as u8;
        self.data[off + 3] = (a + (dst_a * inv + 127) / 255).min(255) as u8;
    }

    pub fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgba) {
        let sx = x0.max(0).min(self.w as i32);
        let sy = y0.max(0).min(self.h as i32);
        let ex = x1.max(0).min(self.w as i32);
        let ey = y1.max(0).min(self.h as i32);
        if ex <= sx || ey <= sy {
            return;
        }
        if c.3 == 255 || c.3 == 0 {
            for y in sy..ey {
                let row = y as usize * self.w;
                for x in sx..ex {
                    let off = (row + x as usize) * 4;
                    self.data[off] = c.2;
                    self.data[off + 1] = c.1;
                    self.data[off + 2] = c.0;
                    self.data[off + 3] = c.3;
                }
            }
        } else {
            for y in sy..ey {
                for x in sx..ex {
                    self.blend_px(x, y, c, 255);
                }
            }
        }
    }

    pub fn stroke_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, c: Rgba) {
        self.fill_rect(x0, y0, x1, y0 + width, c);
        self.fill_rect(x0, y1 - width, x1, y1, c);
        self.fill_rect(x0, y0, x0 + width, y1, c);
        self.fill_rect(x1 - width, y0, x1, y1, c);
    }

    pub fn rounded_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, r: i32, c: Rgba) {
        let r = r.min((x1 - x0) / 2).min((y1 - y0) / 2).max(0);
        if r == 0 {
            self.fill_rect(x0, y0, x1, y1, c);
            return;
        }
        self.fill_rect(x0 + r, y0, x1 - r, y1, c);
        self.fill_rect(x0, y0 + r, x0 + r, y1 - r, c);
        self.fill_rect(x1 - r, y0 + r, x1, y1 - r, c);
        let r2 = r * r;
        for dy in 0..r {
            for dx in 0..r {
                let d2 = (r - dx - 1) * (r - dx - 1) + (r - dy - 1) * (r - dy - 1);
                if d2 <= r2 {
                    self.set_px(x0 + dx, y0 + dy, c);
                    self.set_px(x1 - 1 - dx, y0 + dy, c);
                    self.set_px(x0 + dx, y1 - 1 - dy, c);
                    self.set_px(x1 - 1 - dx, y1 - 1 - dy, c);
                }
            }
        }
    }

    pub fn fill_circle(&mut self, cx: i32, cy: i32, r: i32, c: Rgba) {
        let r2 = r * r;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r2 {
                    self.set_px(cx + dx, cy + dy, c);
                }
            }
        }
    }

    pub fn stroke_ellipse(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, c: Rgba) {
        let rx = (x1 - x0) as f32 / 2.0;
        let ry = (y1 - y0) as f32 / 2.0;
        let cx = (x0 + x1) as f32 / 2.0;
        let cy = (y0 + y1) as f32 / 2.0;
        let steps = ((rx + ry) * 6.0) as i32;
        for i in 0..=steps {
            let a = (i as f32 / steps as f32) * std::f32::consts::TAU;
            let px = cx + rx * a.cos();
            let py = cy + ry * a.sin();
            self.fill_circle(px.round() as i32, py.round() as i32, (width / 2).max(0), c);
        }
    }

    pub fn draw_arc(
        &mut self,
        cx: f32,
        cy: f32,
        r: f32,
        deg0: f32,
        deg1: f32,
        width: i32,
        c: Rgba,
    ) {
        let steps = (r * 4.0) as i32;
        for i in 0..=steps {
            let deg = deg0 + (deg1 - deg0) * (i as f32 / steps.max(1) as f32);
            let rad = deg.to_radians();
            let px = (cx + r * rad.cos()).round() as i32;
            let py = (cy + r * rad.sin()).round() as i32;
            self.fill_circle(px, py, (width / 2).max(1), c);
        }
    }

    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, c: Rgba) {
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let px = (x0 as f32 + (x1 - x0) as f32 * t).round() as i32;
            let py = (y0 as f32 + (y1 - y0) as f32 * t).round() as i32;
            self.fill_circle(px, py, (width / 2).max(0), c);
        }
    }

    pub fn fill_triangle(&mut self, pts: [(i32, i32); 3], c: Rgba) {
        let min_x = pts.iter().map(|p| p.0).min().unwrap().max(0);
        let max_x = pts.iter().map(|p| p.0).max().unwrap().min(self.w as i32 - 1);
        let min_y = pts.iter().map(|p| p.1).min().unwrap().max(0);
        let max_y = pts.iter().map(|p| p.1).max().unwrap().min(self.h as i32 - 1);
        let edge = |a: (i32, i32), b: (i32, i32), p: (i32, i32)| -> i64 {
            (p.0 - a.0) as i64 * (b.1 - a.1) as i64 - (p.1 - a.1) as i64 * (b.0 - a.0) as i64
        };
        let area = edge(pts[0], pts[1], pts[2]);
        if area == 0 {
            return;
        }
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let w0 = edge(pts[1], pts[2], (x, y));
                let w1 = edge(pts[2], pts[0], (x, y));
                let w2 = edge(pts[0], pts[1], (x, y));
                let inside = if area > 0 {
                    w0 >= 0 && w1 >= 0 && w2 >= 0
                } else {
                    w0 <= 0 && w1 <= 0 && w2 <= 0
                };
                if inside {
                    self.set_px(x, y, c);
                }
            }
        }
    }

    pub fn measure_text(&self, fonts: &Fonts, text: &str, size: f32, bold: bool) -> (i32, i32) {
        let (w, _, h, _) = shape_line(fonts, text, size, bold);
        (w.ceil() as i32, h.ceil() as i32)
    }

    pub fn draw_text(&mut self, fonts: &Fonts, x: i32, y: i32, text: &str, size: f32, bold: bool, c: Rgba) {
        let raw_font = fonts.get(bold);
        let (_, ascent, _, glyphs) = shape_line(fonts, text, size, bold);
        let base_x = x as f32;
        let baseline = y as f32 + ascent;
        let scale = PxScale::from(size);
        for (gid, gx, gy) in glyphs {
            let glyph = gid.with_scale_and_position(scale, ab_glyph::point(base_x + gx, baseline + gy));
            if let Some(outlined) = raw_font.outline_glyph(glyph) {
                let bb = outlined.px_bounds();
                outlined.draw(|px, py, cov| {
                    let a = (cov.clamp(0.0, 1.0) * 255.0).round() as u8;
                    self.blend_px(bb.min.x as i32 + px as i32, bb.min.y as i32 + py as i32, c, a);
                });
            }
        }
    }

    pub fn fit_text(&self, fonts: &Fonts, text: &str, size: f32, bold: bool, max_w: i32) -> String {
        if self.measure_text(fonts, text, size, bold).0 <= max_w {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            let mut s: String = chars.iter().collect();
            s.push('…');
            if self.measure_text(fonts, &s, size, bold).0 <= max_w {
                return s;
            }
            chars.pop();
        }
        "…".to_string()
    }

    pub fn paste_canvas(&mut self, src: &Canvas, x: i32, y: i32) {
        for sy in 0..src.h {
            let dy = y + sy as i32;
            if dy < 0 || dy >= self.h as i32 {
                continue;
            }
            for sx in 0..src.w {
                let dx = x + sx as i32;
                if dx < 0 || dx >= self.w as i32 {
                    continue;
                }
                let off = (sy * src.w + sx) * 4;
                let a = src.data[off + 3];
                if a == 0 {
                    continue;
                }
                let c = Rgba(src.data[off + 2], src.data[off + 1], src.data[off], a);
                self.blend_px(dx, dy, c, 255);
            }
        }
    }

    pub fn paste_ink_mask(&mut self, mask: &[u8], mw: usize, mh: usize, x: i32, y: i32, oy: usize, vh: usize, ink: Rgba) {
        for row in 0..vh {
            let my = oy + row;
            if my >= mh {
                break;
            }
            let dy = y + row as i32;
            if dy < 0 || dy >= self.h as i32 {
                continue;
            }
            let mrow = my * mw;
            for col in 0..mw {
                let cov = mask[mrow + col];
                if cov > 0 {
                    self.blend_px(x + col as i32, dy, ink, cov);
                }
            }
        }
    }

    pub fn copy_canvas(&mut self, src: &Canvas, x: i32, y: i32) {
        let sx0 = (-x).max(0) as usize;
        let sy0 = (-y).max(0) as usize;
        let dx0 = x.max(0) as usize;
        let dy0 = y.max(0) as usize;
        if dx0 >= self.w || dy0 >= self.h || sx0 >= src.w || sy0 >= src.h {
            return;
        }
        let cw = (src.w - sx0).min(self.w - dx0);
        let ch = (src.h - sy0).min(self.h - dy0);
        let row_bytes = cw * 4;
        for row in 0..ch {
            let soff = ((sy0 + row) * src.w + sx0) * 4;
            let doff = ((dy0 + row) * self.w + dx0) * 4;
            self.data[doff..doff + row_bytes].copy_from_slice(&src.data[soff..soff + row_bytes]);
        }
    }

    pub fn shift(self, dx: i32, dy: i32, bg: Rgba) -> Self {
        if dx == 0 && dy == 0 {
            return self;
        }
        let mut out = Canvas::new(self.w, self.h, bg);
        out.copy_canvas(&self, dx, dy);
        out
    }
}

pub fn load_png_rgba(path: &str, target_w: usize, target_h: usize) -> Option<Canvas> {
    let f = File::open(path).ok()?;
    let dec = png::Decoder::new(f);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (sw, sh) = (info.width as usize, info.height as usize);
    let mut c = Canvas::new(target_w, target_h, Rgba(0, 0, 0, 0));
    let bpp = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        _ => return None,
    };
    for y in 0..target_h {
        let sy = y * sh / target_h;
        for x in 0..target_w {
            let sx = x * sw / target_w;
            let idx = (sy * sw + sx) * bpp;
            let px = match bpp {
                4 => Rgba(buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]),
                3 => Rgba(buf[idx], buf[idx + 1], buf[idx + 2], 255),
                2 => Rgba(buf[idx], buf[idx], buf[idx], buf[idx + 1]),
                _ => Rgba(buf[idx], buf[idx], buf[idx], 255),
            };
            c.set_px(x as i32, y as i32, px);
        }
    }
    Some(c)
}

pub fn load_png_luma(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    let f = File::open(path).ok()?;
    let dec = png::Decoder::new(f);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let out = match info.color_type {
        png::ColorType::Grayscale => buf[..w * h].to_vec(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).take(w * h).map(|c| c[0]).collect(),
        png::ColorType::Rgb => buf.chunks_exact(3).take(w * h).map(|c| c[0]).collect(),
        png::ColorType::Rgba => buf.chunks_exact(4).take(w * h).map(|c| c[3]).collect(),
        _ => return None,
    };
    Some((w, h, out))
}
