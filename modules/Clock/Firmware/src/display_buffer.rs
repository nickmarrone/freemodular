use display_interface::{DisplayError, WriteOnlyDataCommand};
use embedded_graphics::pixelcolor::BinaryColor;
use ssd1306::{mode::BasicMode, size::DisplaySize, Ssd1306};

use crate::font::{get_font_buffer_size, get_glyph_size_bytes, CharSet, ProgmemBitmapFont};

const BYTE_SIZE: usize = u8::BITS as usize;

/**
Storing the full screen buffer in memory takes a lot of memory and is also relatively
slow, since the full buffer hast to be transmitted to the display driver over SPI
every time it updates. But, drawing directly to the screen can be very difficult if
the shapes you're drawing don't exactly line up with the underlying pages of the
display driver, and could also lead to flickering from non-sequential updates.

As a compromise, the mini buffer is a variable-size buffer that can back just a small
portion of the screen. It can be drawn to and then efficiently copied to the display
driver using the blit function.

All the drawing code lives in the non-generic `Canvas` so that it is only compiled
once, no matter how many different buffer sizes are used. Flash is very tight.
 */
#[repr(transparent)]
pub struct MiniBuffer<const WIDTH: usize, const HEIGHT: usize>([u8; WIDTH * HEIGHT / BYTE_SIZE])
where
    [(); WIDTH * HEIGHT / BYTE_SIZE]: Sized;

impl<const WIDTH: usize, const HEIGHT: usize> MiniBuffer<WIDTH, HEIGHT>
where
    [(); WIDTH * HEIGHT / BYTE_SIZE]: Sized,
{
    pub const fn new() -> Self {
        if HEIGHT % BYTE_SIZE != 0 {
            panic!()
        }
        MiniBuffer([0u8; WIDTH * HEIGHT / BYTE_SIZE])
    }

    #[inline(always)]
    fn canvas(&mut self) -> Canvas<'_> {
        Canvas {
            data: &mut self.0,
            width: WIDTH,
            height: HEIGHT,
        }
    }

    pub fn clear(&mut self, color: BinaryColor) {
        self.0.fill(match color {
            BinaryColor::Off => 0u8,
            BinaryColor::On => 0xffu8,
        });
    }

    /**
    Efficiently copy the contents of the buffer to the SSD1306 driver in BasicMode
    */
    #[inline(always)]
    pub fn blit<DI, SIZE>(
        &self,
        display: &mut Ssd1306<DI, SIZE, BasicMode>,
        x: u8,
        y: u8,
    ) -> Result<(), DisplayError>
    where
        DI: WriteOnlyDataCommand,
        SIZE: DisplaySize,
    {
        blit_raw(&self.0, WIDTH as u8, HEIGHT as u8, display, x, y)
    }

    #[inline(always)]
    pub fn fast_draw_image(
        &mut self,
        x: usize,
        y: usize,
        img_width: u8,
        img_height: u8,
        raw_data: &[u8],
        color: &TextColor,
    ) {
        self.canvas()
            .draw_image(x, y, img_width, img_height, raw_data, color)
    }

    /**
    Drawing text with embedded_graphics requires loading the full font into memory,
    which there isn't room for in the atmega. Also, it is relatively slow. This
    function takes advantage of specifically formatted font data stored in PROGMEM
    and the column-major layout of the buffer to draw text very efficiently.
    */
    #[inline(always)]
    pub fn fast_draw_ascii_text<
        const GLYPH_WIDTH: u8,
        const GLYPH_HEIGHT: u8,
        const CHARSET: CharSet,
    >(
        &mut self,
        horizontal: Justify,
        vertical: Justify,
        text: &[u8],
        font: &'static ProgmemBitmapFont<GLYPH_WIDTH, GLYPH_HEIGHT, CHARSET>,
        color: &TextColor,
    ) where
        [(); get_font_buffer_size(GLYPH_WIDTH, GLYPH_HEIGHT, CHARSET)]: Sized,
        [(); get_glyph_size_bytes(GLYPH_WIDTH, GLYPH_HEIGHT)]: Sized,
    {
        draw_text(&mut self.canvas(), horizontal, vertical, text, font, color)
    }

    #[inline(always)]
    pub fn fast_rect(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: BinaryColor,
        thickness: usize,
    ) {
        self.canvas().rect(x, y, width, height, color, thickness)
    }

    #[inline(always)]
    pub fn fast_fill(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: BinaryColor,
    ) {
        self.canvas().fill(x, y, width, height, color)
    }
}

#[inline(never)]
fn blit_raw<DI, SIZE>(
    data: &[u8],
    width: u8,
    height: u8,
    display: &mut Ssd1306<DI, SIZE, BasicMode>,
    x: u8,
    y: u8,
) -> Result<(), DisplayError>
where
    DI: WriteOnlyDataCommand,
    SIZE: DisplaySize,
{
    if y % 8 != 0 {
        return Err(DisplayError::OutOfBoundsError);
    }
    let (display_width, display_height) = display.dimensions();
    if x + width > display_width as u8 || y + height > display_height as u8 {
        return Err(DisplayError::OutOfBoundsError);
    }
    display.set_draw_area((x, y), (x + width, y + height))?;
    display.draw(data)?;
    Ok(())
}

/// Column-major 1-bit bitmap matching the SSD1306 page layout
pub struct Canvas<'a> {
    data: &'a mut [u8],
    width: usize,
    height: usize,
}

impl<'a> Canvas<'a> {
    fn get_byte_at(&mut self, col: usize, page: usize) -> &mut u8 {
        let index = col * (self.height / BYTE_SIZE) + page;
        debug_assert!(index < self.data.len());
        unsafe { self.data.get_unchecked_mut(index) }
    }

    fn write_byte_if_in_bounds(&mut self, col: usize, page: usize, value: u8, color: &TextColor) {
        if page >= self.height / BYTE_SIZE || col >= self.width {
            return;
        }
        let current = self.get_byte_at(col, page);
        *current = match color {
            TextColor::BinaryOn => value,
            TextColor::BinaryOff => !value,
            TextColor::BinaryOnTransparent => *current | value,
            TextColor::BinaryOffTransparent => *current & !value,
            TextColor::InvertTransparent => *current ^ value,
        };
    }

    #[inline(never)]
    pub fn draw_image(
        &mut self,
        x: usize,
        y: usize,
        img_width: u8,
        img_height: u8,
        raw_data: &[u8],
        color: &TextColor,
    ) {
        let y_offset_bytes = y / BYTE_SIZE;
        let y_offset_bits = (y % BYTE_SIZE) as u8;
        let img_height_bytes: u8 = img_height.div_ceil(u8::BITS as u8);

        let mut offset_in_glyph = 0usize;
        for col_in_glyph in 0..img_width {
            let col_in_buff = col_in_glyph as usize + x;
            let mut last_byte_in_glyph = 0u8;
            for byte_idx_in_glyph in 0..img_height_bytes {
                debug_assert!(offset_in_glyph < raw_data.len());
                let byte_in_glyph = unsafe { *raw_data.get_unchecked(offset_in_glyph) };
                offset_in_glyph += 1;
                let byte_to_write = if y_offset_bits == 0 {
                    byte_in_glyph
                } else {
                    (byte_in_glyph << y_offset_bits)
                        | (last_byte_in_glyph >> (u8::BITS as u8 - y_offset_bits))
                };

                last_byte_in_glyph = byte_in_glyph;
                self.write_byte_if_in_bounds(
                    col_in_buff,
                    byte_idx_in_glyph as usize + y_offset_bytes,
                    byte_to_write,
                    color,
                )
            }
            if img_height + y_offset_bits > img_height_bytes * (u8::BITS as u8) {
                self.write_byte_if_in_bounds(
                    col_in_buff,
                    img_height_bytes as usize + y_offset_bytes,
                    last_byte_in_glyph >> (u8::BITS as u8 - y_offset_bits),
                    color,
                )
            }
        }
    }

    #[inline(never)]
    pub fn rect(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: BinaryColor,
        thickness: usize,
    ) {
        self.fill(x, y, width, thickness, color);
        self.fill(x, y + height - thickness, width, thickness, color);
        self.fill(x, y, thickness, height, color);
        self.fill(x + width - thickness, y, thickness, height, color);
    }

    /// Fill a rectangle, one pixel at a time per column. Simple and small; the
    /// rectangles drawn here are small enough that speed doesn't matter.
    #[inline(never)]
    pub fn fill(&mut self, x: usize, y: usize, width: usize, height: usize, color: BinaryColor) {
        let right = (x + width).min(self.width);
        let bottom = (y + height).min(self.height);
        for col in x..right {
            for row in y..bottom {
                let mask = 1u8 << (row % BYTE_SIZE);
                let byte = self.get_byte_at(col, row / BYTE_SIZE);
                match color {
                    BinaryColor::Off => *byte &= !mask,
                    BinaryColor::On => *byte |= mask,
                }
            }
        }
    }
}

#[inline(never)]
fn draw_text<const GLYPH_WIDTH: u8, const GLYPH_HEIGHT: u8, const CHARSET: CharSet>(
    canvas: &mut Canvas,
    horizontal: Justify,
    vertical: Justify,
    text: &[u8],
    font: &'static ProgmemBitmapFont<GLYPH_WIDTH, GLYPH_HEIGHT, CHARSET>,
    color: &TextColor,
) where
    [(); get_font_buffer_size(GLYPH_WIDTH, GLYPH_HEIGHT, CHARSET)]: Sized,
    [(); get_glyph_size_bytes(GLYPH_WIDTH, GLYPH_HEIGHT)]: Sized,
{
    let text_width = GLYPH_WIDTH as usize * text.len();
    let x = match horizontal {
        Justify::Start(offset) => offset,
        Justify::Center(offset) => offset.saturating_sub(text_width / 2),
        Justify::End(offset) => offset.saturating_sub(text_width),
    };
    let y = match vertical {
        Justify::Start(offset) => offset,
        Justify::Center(offset) => offset.saturating_sub(GLYPH_HEIGHT as usize / 2),
        Justify::End(offset) => offset.saturating_sub(GLYPH_HEIGHT as usize),
    };

    let mut cursor = x;
    for ascii_char in text {
        let glyph = font.get_glyph(*ascii_char);
        canvas.draw_image(cursor, y, GLYPH_WIDTH, GLYPH_HEIGHT, &glyph, color);
        cursor += GLYPH_WIDTH as usize;
    }
}

#[allow(dead_code)]
pub enum Justify {
    Start(usize),
    Center(usize),
    End(usize),
}

#[allow(dead_code)]
pub enum TextColor {
    BinaryOn,
    BinaryOff,
    BinaryOnTransparent,
    BinaryOffTransparent,
    InvertTransparent,
}
