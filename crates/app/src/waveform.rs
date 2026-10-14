use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use djcraft_engine::Track;
use djcraft_library::TempoResult;
use gpui_kit::*;

/// Length of track visible across the full width of the waveform.
const VISIBLE_SECONDS: f64 = 8.;
const COLUMN_WIDTH: f32 = 2.;

/// A waveform that scrolls under a fixed, centred playhead, with the beat
/// grid drawn over it when there is one: a faint line on each beat and a
/// stronger one where each bar starts.
pub fn waveform(
    track: Arc<Track>,
    position_seconds: f64,
    grid: Option<TempoResult>,
    played: Hsla,
    upcoming: Hsla,
    playhead: Hsla,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let width = bounds.size.width.as_f32();
            let height = bounds.size.height.as_f32();
            // Peaks arrive as the track decodes, so the waveform fills in.
            let peak_count = track.peak_count() as i64;
            let peaks_per_second = track.sample_rate as f64 / track.frames_per_peak() as f64;

            // Columns cover fixed groups of peaks, and the whole row shifts
            // by a fraction of a column, so bars keep their shape as they
            // scroll.
            let columns = (width / COLUMN_WIDTH).ceil() as i64 + 1;
            let peaks_per_column =
                VISIBLE_SECONDS * peaks_per_second / (width / COLUMN_WIDTH) as f64;
            let centre = position_seconds * peaks_per_second / peaks_per_column;
            let first = centre.floor() as i64 - columns / 2;
            let shift = (centre.fract() as f32) * COLUMN_WIDTH;

            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for column in 0..columns {
                    let start = ((first + column) as f64 * peaks_per_column) as i64;
                    let end = ((first + column + 1) as f64 * peaks_per_column) as i64;
                    let range = start.max(0)..end.max(start + 1).min(peak_count);
                    if range.is_empty() {
                        continue;
                    }
                    let peak = range.fold(0f32, |peak, i| peak.max(track.peak(i as usize)));
                    let bar = (peak * height).max(1.);
                    let x = column as f32 * COLUMN_WIDTH - shift;
                    let color = if first + column < centre.floor() as i64 {
                        played
                    } else {
                        upcoming
                    };
                    window.paint_quad(fill(
                        Bounds::new(
                            bounds.origin + point(px(x), px((height - bar) / 2.)),
                            size(px(COLUMN_WIDTH), px(bar)),
                        ),
                        color,
                    ));
                }
                let x = (columns / 2) as f32 * COLUMN_WIDTH;
                for (line_x, bar_line) in grid_lines(grid, position_seconds, width, x) {
                    let (line_width, opacity) = if bar_line { (2., 0.55) } else { (1., 0.2) };
                    window.paint_quad(fill(
                        Bounds::new(
                            bounds.origin + point(px(line_x), px(0.)),
                            size(px(line_width), px(height)),
                        ),
                        playhead.opacity(opacity),
                    ));
                }
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(x), px(0.)),
                        size(px(2.), px(height)),
                    ),
                    playhead,
                ));
            });
        },
    )
    .size_full()
}

/// Where the beat grid's lines fall across a scrolling waveform `width`
/// pixels wide whose playhead, at `position_seconds`, is `playhead_x` pixels
/// in: each line's x and whether it starts a bar. Beats before the track
/// starts are left out.
fn grid_lines(
    grid: Option<TempoResult>,
    position_seconds: f64,
    width: f32,
    playhead_x: f32,
) -> Vec<(f32, bool)> {
    let Some(grid) = grid.filter(|grid| grid.bpm > 0.) else {
        return Vec::new();
    };
    let pixels_per_second = width as f64 / VISIBLE_SECONDS;
    let beat = grid.beat_seconds();
    let half = VISIBLE_SECONDS / 2.;
    let first = ((position_seconds - half - grid.first_beat_seconds) / beat).ceil() as i64;
    let last = ((position_seconds + half - grid.first_beat_seconds) / beat).floor() as i64;
    (first..=last)
        .filter_map(|index| {
            let seconds = grid.first_beat_seconds + index as f64 * beat;
            let bar_line = (index - grid.downbeat as i64).rem_euclid(4) == 0;
            let x = playhead_x + ((seconds - position_seconds) * pixels_per_second) as f32;
            (seconds >= 0.).then_some((x, bar_line))
        })
        .collect()
}

/// A track's whole waveform, with the part already played in another colour
/// and a line at the playing position.
pub fn overview(
    track: Arc<Track>,
    played_fraction: f64,
    area: Rc<Cell<Bounds<Pixels>>>,
    played: Hsla,
    upcoming: Hsla,
    playhead: Hsla,
) -> impl IntoElement {
    const COLUMN: f32 = 2.;
    canvas(
        move |bounds, _, _| area.set(bounds),
        move |bounds, _, window, _| {
            let (width, height) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
            // Peaks arrive as the track decodes, so the waveform fills in.
            let total = (track.frames() / track.frames_per_peak()).max(1) as f32;
            let ready = track.peak_count();
            let columns = (width / COLUMN).ceil() as usize;
            let played_x = played_fraction as f32 * width;
            for column in 0..columns {
                let x = column as f32 * COLUMN;
                let from = (x / width * total) as usize;
                let to = (((x + COLUMN) / width * total) as usize)
                    .max(from + 1)
                    .min(ready);
                if from >= to {
                    break;
                }
                let peak = (from..to).fold(0f32, |peak, i| peak.max(track.peak(i)));
                let bar = (peak * height).max(1.);
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(x), px((height - bar) / 2.)),
                        size(px(COLUMN - 0.5), px(bar)),
                    ),
                    if x < played_x { played } else { upcoming },
                ));
            }
            window.paint_quad(fill(
                Bounds::new(
                    bounds.origin + point(px(played_x), px(0.)),
                    size(px(2.), px(height)),
                ),
                playhead,
            ));
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in GPUI's own `test` attribute.
    use super::{VISIBLE_SECONDS, grid_lines};
    use djcraft_library::TempoResult;

    #[test]
    fn grid_lines_sit_on_beats_with_bars_every_fourth() {
        // 120 BPM: a beat every half second, the first at 0.25 s, and bars
        // starting on the second beat (0.75 s, 2.75 s, ...).
        let grid = TempoResult {
            bpm: 120.,
            first_beat_seconds: 0.25,
            downbeat: 1,
            confidence: 1.,
        };
        let width = 800.;
        let pixels_per_second = width / VISIBLE_SECONDS as f32;
        let (playhead_x, position) = (400., 10.);
        let lines = grid_lines(Some(grid), position, width, playhead_x);

        // Eight seconds are in view, so sixteen beats.
        assert_eq!(lines.len(), 16);
        for (x, bar_line) in &lines {
            let seconds = position + ((x - playhead_x) / pixels_per_second) as f64;
            let beats = (seconds - 0.25) / 0.5;
            assert!(
                (beats - beats.round()).abs() < 1e-3,
                "{seconds} is not on a beat"
            );
            let bar = ((seconds - 0.75) / 2.).rem_euclid(1.);
            let on_bar = bar < 1e-3 || bar > 1. - 1e-3;
            assert_eq!(*bar_line, on_bar, "at {seconds} s");
        }
        assert_eq!(lines.iter().filter(|(_, bar)| *bar).count(), 4);
        // The beat at 9.75 s is a quarter second, 25 pixels, left of the playhead.
        assert!(lines.iter().any(|(x, _)| (x - 375.).abs() < 0.01));
    }

    #[test]
    fn no_lines_before_the_track_or_without_a_grid() {
        let grid = TempoResult {
            bpm: 120.,
            first_beat_seconds: 0.25,
            downbeat: 0,
            confidence: 1.,
        };
        // At the start, half the view is before the track.
        let lines = grid_lines(Some(grid), 0., 800., 400.);
        assert!(lines.iter().all(|(x, _)| *x >= 400.));
        assert_eq!(lines.first(), Some(&(425., true)));
        assert!(grid_lines(None, 5., 800., 400.).is_empty());
    }
}
