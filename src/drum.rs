// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// The drum spectrogram's own arithmetic: which windows, which axis, which
// shade and how wide a pen.
//
// WHY THIS IS HERE AND NOT IN THE WINDOW. All of it was written in the
// window, where the drum was first drawn, and none of it is about a window:
// where a period stands across the paper, how hot a power is, how wide the
// pen. The terminal monitor draws the same paper in character cells, and a
// second copy of these would be a second drum that agreed with the first
// until somebody changed one. So they are here, once, and both front ends
// are a way of putting ink where this says.

/// THE OVERLAY, AND WHY THESE THREE. A spectrum resolves periods up to its own
/// window and no further: you cannot see an hourly rhythm in ten minutes of
/// listening. One window is therefore always the wrong question for somebody
/// hunting an unknown period, so three are run at once and drawn on top of one
/// another against a shared logarithmic period axis -- a real line stands in
/// the same place in all three, which is most of what tells it from a fluke.
///
/// Powers of two because the transform is radix-2. 512 s is eight minutes and
/// answers quickly; 4096 s is an hour and change; 32768 s is nine hours, which
/// is the working day the longest averaging window already watches.
pub const LADDER: [usize; 3] = [512, 4096, 32768];

/// The trail: seconds in a row, seconds between rows, rows kept.
///
/// THE SPECTRUM'S OWN SHORTEST WINDOW, 512 seconds, so that a row is that
/// window taken once, the joins to the longer windows are the spectrum's
/// own ladder, and the axis is cut where the spectrum's windows are and
/// nowhere else. It was 300, five minutes, for an afternoon, and the
/// spectrum's line and the rows' were cut to different grids. A row every
/// eight seconds -- on the same grid of twos -- is sixty-three in
/// sixty-four of its seconds shared with the row before, so a ridge is a
/// line and not a row of dots; ninety-six of them are the same twelve
/// minutes of paper that forty-eight rows every sixteen seconds were, at
/// twice the resolution in time.
pub const FALL_WINDOW: usize = 512;
pub const FALL_HOP: usize = 8;
pub const FALL_DEPTH: usize = 96;
/// The seconds of the average a row is held against: the fourth of the
/// panel's five averages. See `Waterfall::leveled`.
pub const FALL_LEVEL: usize = 3000;

/// How much of the panel's width each stretch of long periods has.
///
/// THE LEFT OF THE AXIS IS COMPRESSED, because there is next to nothing in
/// it. On an axis that gave every doubling of the period the same width,
/// the periods over a row's length had the left two fifths of the panel and
/// fourteen bins to put there -- a window holds one bin at its own length,
/// one at half, one at a third -- while the right-hand fifth held a
/// hundred. So the axis is cut where the spectrum's windows are, as the
/// counts' strip is cut into tiers, and each stretch is a logarithm of its
/// own: 512 seconds and under has most of the width, and what is longer
/// has these.
pub const STRETCH_LONG: f32 = 0.10;
pub const STRETCH_MIDDLE: f32 = 0.14;

/// A stretch of the period axis: the periods at its two ends, and where it
/// begins and ends across the panel, from nought to one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stretch {
    pub long: f64,
    pub short: f64,
    pub left: f32,
    pub right: f32,
}

/// The axis the spectrum and its trail are both drawn on, from the longest
/// window that has answered down to `floor`.
pub fn stretches(floor: f64, longest: usize) -> Vec<Stretch> {
    let base = FALL_WINDOW as f64;
    // The cuts: each of the spectrum's windows longer than a row, that has
    // answered, with its share -- the longest of the ladder's, or the
    // middle's.
    let cuts: Vec<(f64, f32)> = LADDER
        .iter()
        .rev()
        .filter(|w| **w <= longest && **w as f64 > base)
        .map(|w| {
            let share = if *w == LADDER[LADDER.len() - 1] { STRETCH_LONG } else { STRETCH_MIDDLE };
            (*w as f64, share)
        })
        .collect();
    let mut out = Vec::new();
    let mut left = 0.0f32;
    for (k, (long, share)) in cuts.iter().enumerate() {
        let short = cuts.get(k + 1).map(|c| c.0).unwrap_or(base);
        out.push(Stretch { long: *long, short, left, right: left + share });
        left += share;
    }
    out.push(Stretch { long: base.min(longest as f64), short: floor.min(base), left, right: 1.0 });
    out
}

/// Where a period is across the panel, from nought to one; None where it
/// is longer than the axis goes, or shorter.
pub fn place(axis: &[Stretch], period: f64) -> Option<f32> {
    let s = axis.iter().find(|s| period <= s.long && period >= s.short)?;
    let span = (s.long / s.short).ln();
    let t = if span > 0.0 { ((s.long / period).ln() / span) as f32 } else { 0.0 };
    Some(s.left + t.clamp(0.0, 1.0) * (s.right - s.left))
}

/// How many shades a trace is drawn in: enough that the eye sees one
/// gradient and not bands. Sixteen was bands, at the hot end above all,
/// where yellow, orange, red and white were four steps apart.
pub const SHADES: usize = 64;

/// Which shade a power is: the first for nothing, and the last for half
/// as much again as luck reaches, and over.
///
/// BY ITS LOGARITHM, as the heights are. In proportion, everything under
/// three times the mean -- which is nearly everything -- was the first
/// two shades of ten, and the pen had one colour for the floor and nine
/// for what is hardly ever there. By `crate::analysis::gain` what is flat is a
/// quarter of the way up the shades and what luck reaches is six sevenths.
pub fn shade_of(power: f32, luck: f32) -> usize {
    let t = crate::analysis::gain(power as f64, 1.5 * luck.max(1e-6) as f64) as f32;
    ((t * SHADES as f32) as usize).min(SHADES - 1)
}

/// The colour of a shade: the pen's temperature.
///
/// MOST OF IT IS GREEN, because most of what is drawn is the floor and the
/// eye tells more greens apart than it does any other colour. From a teal
/// that is hardly there, through sea green and green to lime, is the run
/// from nothing up to what luck reaches: eleven shades of the sixteen, for
/// the part of the paper where the detail is. Then it is hot. Yellow is
/// just under the luck line; over it orange and red, which no floor ever
/// is; and the last is white, for what is half as much again as luck could
/// do. Between any two of these it is a mixture, and with sixty-four shades
/// the mixtures are a gradient.
///
/// Red, green and blue, each from nought to one, and no colour type of
/// anybody's: the window makes its own of them and the terminal monitor
/// makes an escape code. The same in both of the window's skins, because
/// the ground under the trail is.
pub fn heat(shade: usize) -> [f32; 3] {
    const STOPS: [(f32, [f32; 3]); 9] = [
        (0.00, [0.16, 0.40, 0.37]),
        (0.22, [0.14, 0.60, 0.46]),
        (0.45, [0.27, 0.82, 0.38]),
        (0.62, [0.66, 0.93, 0.30]),
        (0.76, [1.00, 0.86, 0.24]),
        (0.84, [1.00, 0.58, 0.16]),
        (0.90, [1.00, 0.30, 0.20]),
        (0.96, [1.00, 0.72, 0.60]),
        (1.00, [1.00, 0.96, 0.90]),
    ];
    let t = shade.min(SHADES - 1) as f32 / (SHADES - 1) as f32;
    let k = STOPS.iter().rposition(|s| s.0 <= t).unwrap_or(0).min(STOPS.len() - 2);
    let ((t0, a), (t1, b)) = (STOPS[k], STOPS[k + 1]);
    let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

/// How wide the pen is for a shade, in pixels: under one for the floor, and
/// three for the hottest. The shade is a logarithm, so the width is.
pub fn pen(shade: usize) -> f32 {
    0.6 + 1.8 * shade.min(SHADES - 1) as f32 / (SHADES - 1) as f32
}

/// The shortest period the axis goes down to, on `columns` columns.
///
/// NOT TWO SECONDS, WHICH IS ONLY WHERE THE TRANSFORM STOPS. A window of W
/// seconds has a bin at every W/n, so the bins crowd towards the short end,
/// and folding a crowd of them into one column draws the largest of a crowd
/// and not a line. The honest floor is where the SHORTEST window's bins are
/// still a column apart, which depends on that window, on the longest one
/// and on how many columns there are. The floor sets the span and the span
/// sets the floor, so it is solved by going round, which settles in three or
/// four passes.
///
/// THE COLUMNS ARE THE FRONT END'S. The window has had this arithmetic since
/// its first spectrum, with its own six hundred written into it; the
/// terminal monitor has two dots to a character and may have a hundred and
/// sixty of them, so the count is asked for here and the floor is further up
/// on a narrow paper than on a wide one.
pub fn period_floor(shortest: usize, longest: usize, columns: f64) -> f64 {
    let columns = columns.max(1.0);
    let mut p = 2.0f64;
    for _ in 0..8 {
        let span = (longest as f64 / p).ln().max(1e-9);
        // Two seconds is the Nyquist limit of a one-second sample and nothing
        // is resolvable below it at any width.
        p = (shortest as f64 * span / columns).max(2.0);
    }
    p
}

/// The hours-long average as one row of the drum: the front row.
///
/// JOINED FROM THE WINDOWS EXACTLY AS A ROW IS. Every bin of the shortest
/// spectrum that has answered, down to `floor`; and of each longer one only
/// the bins beyond what the window before it could hold. So the average and
/// the rows over it have their points at the same periods, and a peak in one
/// stands over the same peak in the other.
///
/// (period, power) with the longest period first, as `TrailRow::bins` is;
/// empty until a spectrum has answered.
pub fn front_row(spectra: &[&crate::analysis::Spectrum], floor: f64) -> Vec<(f32, f32)> {
    let mut sorted: Vec<&crate::analysis::Spectrum> = spectra.to_vec();
    sorted.sort_by_key(|s| s.window);
    let mut points: Vec<(f32, f32)> = Vec::new();
    let mut before = 0usize;
    for s in sorted {
        let rel = s.relative();
        if rel.is_empty() {
            continue;
        }
        for (i, v) in rel.iter().enumerate() {
            let period = s.period(i);
            if period < floor || (before > 0 && period <= before as f64) {
                continue;
            }
            points.push((period as f32, *v as f32));
        }
        before = s.window;
    }
    points.sort_by(|a, b| b.0.total_cmp(&a.0));
    points
}

/// The top of the paper's scale: the loudest thing on the axis in any row,
/// and never less than luck reaches. Every row and the front row are drawn
/// to it, so the taller of two peaks is the louder wherever they are.
pub fn loudest(rows: &[crate::analysis::TrailRow], axis: &[Stretch], luck: f32) -> f64 {
    rows.iter()
        .flat_map(|row| row.bins.iter())
        .filter(|(period, _)| place(axis, *period as f64).is_some())
        .map(|(_, power)| *power)
        .fold(luck, f32::max) as f64
}

/// How strongly a whole row is inked, from nought to one.
///
/// AS BRIGHT AS THE ROOM COUNTED, and a little fainter for being older. A
/// row has a level -- what was counted in its own seconds against the long
/// average -- and the row that counted most on the paper, `busiest`, which
/// is never taken as less than twice the usual, is as bright as ink is.
/// `away` is how far up the paper the row has gone, nought at the foot and
/// one at the last row kept: further up is further away.
pub fn strength(level: f32, busiest: f64, away: f32) -> f32 {
    let bright = 0.30 + 0.70 * crate::analysis::gain(level as f64, busiest.max(2.0)) as f32;
    bright * (1.0 - 0.35 * away.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Spectrum, TrailRow};

    #[test]
    fn a_narrow_paper_stops_at_a_longer_period_than_a_wide_one() {
        let wide = period_floor(512, 32768, 600.0);
        let narrow = period_floor(512, 32768, 158.0);
        assert!(wide > 2.0 && wide < 10.0, "{}", wide);
        assert!(narrow > wide * 2.0 && narrow < 40.0, "{}", narrow);
        // At the floor the shortest window's bins are a column apart: the
        // rule the floor is solved from.
        let span = (32768.0 / narrow).ln();
        assert!((512.0 * span / 158.0 - narrow).abs() < 0.01);
        assert_eq!(period_floor(512, 512, 1.0e9), 2.0);
    }

    #[test]
    fn the_front_row_is_joined_as_a_row_is() {
        // Flat counts with one beat in them, so the spectra have something.
        let mut short = Spectrum::new(64);
        let mut long = Spectrum::new(256);
        let unheard = Spectrum::new(1024);
        for k in 0..512u32 {
            let c = 3 + (k / 8) % 2;
            short.add(c);
            long.add(c);
        }
        let row = front_row(&[&long, &unheard, &short], 4.0);
        assert!(!row.is_empty());
        assert!(row.windows(2).all(|p| p[0].0 > p[1].0), "longest period first");
        // The long window gives only what the short one cannot hold: 256,
        // 128 and 256/3; the short one everything from 64 down to the floor.
        let beyond: Vec<f32> = row.iter().map(|p| p.0).filter(|p| *p > 64.0).collect();
        assert_eq!(beyond, vec![256.0, 128.0, 256.0 / 3.0]);
        assert_eq!(row.iter().filter(|p| p.0 <= 64.0).count(), 16);
        assert!(row.last().unwrap().0 >= 4.0);
        // The beat is sixteen seconds long and is the tallest thing there.
        let top = row.iter().fold((0.0f32, 0.0f32), |a, b| if b.1 > a.1 { *b } else { a });
        assert_eq!(top.0, 16.0);
        assert!(front_row(&[&unheard], 4.0).is_empty());
    }

    #[test]
    fn the_paper_is_scaled_to_what_is_on_its_axis() {
        let axis = stretches(8.0, 512);
        let rows = vec![
            TrailRow { level: 1.0, bins: vec![(4096.0, 90.0), (256.0, 3.0), (16.0, 12.0)] },
            TrailRow { level: 1.0, bins: vec![(256.0, 2.0), (4.0, 50.0)] },
        ];
        // 4096 seconds and 4 seconds are off this axis and set nothing.
        assert_eq!(loudest(&rows, &axis, 7.0), 12.0);
        assert_eq!(loudest(&rows, &axis, 20.0), 20.0);
        assert_eq!(loudest(&[], &axis, 7.0), 7.0);
    }

    #[test]
    fn a_busy_row_is_bright_and_an_old_one_is_fainter() {
        let usual = strength(1.0, 2.0, 0.0);
        let busy = strength(2.0, 2.0, 0.0);
        assert!((busy - 1.0).abs() < 1e-6);
        assert!(usual > 0.6 && usual < busy);
        assert!(strength(0.0, 2.0, 0.0) < 0.31);
        assert!((strength(2.0, 2.0, 1.0) - 0.65).abs() < 1e-6);
        // A paper with nothing over the usual on it is held against twice
        // the usual all the same.
        assert_eq!(strength(1.0, 1.0, 0.0), usual);
    }
}
