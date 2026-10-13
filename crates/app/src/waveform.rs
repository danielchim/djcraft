use std::sync::Arc;

use djcraft_engine::Track;
use gpui_kit::*;

/// Length of track visible across the full width of the waveform.
const VISIBLE_SECONDS: f64 = 8.;
const COLUMN_WIDTH: f32 = 2.;

/// A waveform that scrolls under a fixed, centred playhead.
pub fn waveform(
    track: Arc<Track>,
    position_seconds: f64,
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
