//! 8x8 ingestion activity sprite supplied for the TUI, adapted to the Nord
//! palette. Transparent pixels leave the underlying panel intact.

use ratatui::{buffer::Buffer, layout::Position, style::Color};

pub const FRAME_COUNT: usize = 8;
pub const WIDTH: u16 = 16;
pub const HEIGHT: u16 = 8;

const PALETTE: [Option<Color>; 8] = [
    None,
    Some(Color::Rgb(136, 192, 208)), // nord8 body
    Some(Color::Rgb(94, 129, 172)),  // nord10 shadow
    Some(Color::Rgb(236, 239, 244)), // nord6 eye
    Some(Color::Rgb(46, 52, 64)),    // nord0 pupil
    Some(Color::Rgb(59, 66, 82)),    // nord1 mouth
    Some(Color::Rgb(235, 203, 139)), // nord13 activity
    Some(Color::Rgb(163, 190, 140)), // nord14 completed work
];

const FRAMES: [[[u8; 8]; 8]; FRAME_COUNT] = [
    [
        [0, 0, 1, 1, 1, 1, 0, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 3, 1, 1, 3, 1, 0],
        [0, 1, 4, 1, 1, 4, 1, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 0, 1, 2, 2, 1, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
    ],
    [
        [0, 0, 1, 1, 1, 1, 0, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 3, 1, 1, 3, 1, 0],
        [0, 1, 4, 1, 1, 4, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 0, 1, 2, 2, 1, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
    ],
    [
        [0, 0, 1, 1, 1, 1, 0, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 3, 1, 1, 3, 1, 0],
        [0, 1, 4, 1, 1, 4, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 0, 1, 5, 5, 1, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
    ],
    [
        [0, 6, 1, 1, 1, 1, 0, 0],
        [0, 1, 1, 1, 1, 1, 1, 6],
        [6, 1, 3, 1, 1, 3, 1, 0],
        [0, 1, 4, 1, 1, 4, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 6],
        [0, 0, 1, 5, 5, 1, 0, 0],
        [0, 0, 0, 6, 0, 0, 0, 0],
    ],
    [
        [0, 0, 1, 1, 1, 1, 0, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 3, 1, 1, 3, 1, 0],
        [1, 1, 4, 1, 1, 4, 1, 1],
        [1, 1, 1, 5, 5, 1, 1, 1],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 0, 1, 2, 2, 1, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
    ],
    [
        [0, 0, 1, 7, 7, 1, 0, 0],
        [0, 1, 7, 1, 1, 7, 1, 0],
        [0, 1, 3, 1, 1, 3, 1, 0],
        [0, 1, 4, 1, 1, 4, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 1, 7, 7, 7, 7, 1, 0],
        [0, 0, 1, 7, 7, 1, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
    ],
    [
        [0, 0, 6, 1, 1, 6, 0, 0],
        [0, 0, 1, 1, 1, 1, 0, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 3, 4, 4, 3, 1, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 0, 1, 2, 2, 1, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
    ],
    [
        [0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 1, 1, 1, 1, 0, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 3, 1, 1, 3, 1, 0],
        [0, 1, 4, 1, 1, 4, 1, 0],
        [0, 1, 1, 1, 1, 1, 1, 0],
        [0, 1, 1, 5, 5, 1, 1, 0],
        [0, 0, 1, 2, 2, 1, 0, 0],
    ],
];

pub fn draw(buf: &mut Buffer, x: u16, y: u16, frame: usize) {
    let grid = &FRAMES[frame % FRAME_COUNT];
    for (dy, row) in grid.iter().enumerate() {
        for (dx, &pixel) in row.iter().enumerate() {
            let Some(color) = PALETTE[pixel as usize] else {
                continue;
            };
            let px = x.saturating_add(dx as u16 * 2);
            let py = y.saturating_add(dy as u16);
            for offset in 0..2 {
                if let Some(cell) = buf.cell_mut(Position::new(px + offset, py)) {
                    cell.set_char(' ').set_bg(color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn every_frame_draws_and_clips_safely() {
        for frame in 0..FRAME_COUNT {
            let mut buffer = Buffer::empty(Rect::new(4, 2, 10, 5));
            draw(&mut buffer, 10, 5, frame);
            assert_eq!(buffer.area, Rect::new(4, 2, 10, 5));
        }
    }
}
