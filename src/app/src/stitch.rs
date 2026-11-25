//! Scrolling capture alignment and stitching algorithm (ported from legacy images.cjs).
//! Uses row-profile candidate detection, tiled overlap verification, and sticky header/footer detection.

use crate::i18n;
use crate::images::Rgba;

#[derive(Clone, Copy, Debug)]
pub struct Alignment {
    pub shift: u32,
    pub top: u32,
    pub bottom: u32,
}

/// Sample mean RGB difference (0-255 scale) to determine whether the screen is static.
pub fn difference(a: &Rgba, b: &Rgba) -> f64 {
    if a.width != b.width || a.height != b.height {
        return f64::INFINITY;
    }
    let mut sum = 0u64;
    let mut count = 0u64;
    let step_x = ((a.width / 128).max(1)) as usize;
    for y in 0..a.height as usize {
        let mut x = 0usize;
        while x < a.width as usize {
            let p = (y * a.width as usize + x) * 4;
            sum += (a.pixels[p].abs_diff(b.pixels[p]) as u32
                + a.pixels[p + 1].abs_diff(b.pixels[p + 1]) as u32
                + a.pixels[p + 2].abs_diff(b.pixels[p + 2]) as u32) as u64;
            count += 3;
            x += step_x;
        }
    }
    if count == 0 {
        return 0.0;
    }
    sum as f64 / count as f64
}

/// Identify columns that change while scrolling; static sidebars and blank margins do not provide scroll evidence.
fn content_columns(previous: &Rgba, next: &Rgba) -> Vec<u32> {
    let width = previous.width as usize;
    let height = previous.height as usize;
    let edge = ((width as f64 * 0.02).ceil() as usize).max(1);
    let step = ((width as f64 / 128.0).ceil() as usize).max(1);
    let mut columns = Vec::new();
    let mut x = edge;
    while x < width - edge {
        let mut changed = 0usize;
        for y in 0..height {
            let p = (y * width + x) * 4;
            if previous.pixels[p].abs_diff(next.pixels[p]) as u32
                + previous.pixels[p + 1].abs_diff(next.pixels[p + 1]) as u32
                + previous.pixels[p + 2].abs_diff(next.pixels[p + 2]) as u32
                > 18
            {
                changed += 1;
            }
        }
        // Require at least 1% of rows to change to count as a content column
        if changed >= ((height as f64 * 0.01).floor() as usize).max(2) {
            columns.push(x as u32);
        }
        x += step;
    }
    columns
}

/// Detect sticky header/footer height: consecutive rows nearly identical to the previous frame with texture.
fn fixed_edge(previous: &Rgba, next: &Rgba, columns: &[u32], from_bottom: bool) -> u32 {
    if columns.is_empty() {
        return 0;
    }
    let width = previous.width as usize;
    let height = previous.height as usize;
    let mut length = 0usize;
    let mut texture = 0u64;
    for row in 0..(height / 3) {
        let y = if from_bottom { height - row - 1 } else { row };
        let mut error = 0u64;
        let mut row_texture = 0u64;
        for (index, column) in columns.iter().enumerate() {
            let p = (y * width + *column as usize) * 4;
            for c in 0..3 {
                error += previous.pixels[p + c].abs_diff(next.pixels[p + c]) as u64;
                if index > 0 {
                    let q = (y * width + columns[index - 1] as usize) * 4;
                    row_texture +=
                        previous.pixels[p + c].abs_diff(previous.pixels[q + c]) as u64;
                }
            }
        }
        if (error as f64) / ((columns.len() * 3) as f64) > 0.75 {
            break;
        }
        texture += row_texture;
        length += 1;
    }
    // Solid blank margins may belong to document content and must not be treated as sticky edges
    if length >= 4 && texture > (columns.len() as u64) * 12 {
        length as u32
    } else {
        0
    }
}

/// Row profile: average color across 8 horizontal bins (24 channels) for fast candidate detection.
fn row_profiles(image: &Rgba, columns: &[u32]) -> Vec<f32> {
    const BINS: usize = 24;
    let width = image.width as usize;
    let height = image.height as usize;
    let mut rows = vec![0f32; height * BINS];
    let mut counts = [0u16; 8];
    for column in columns {
        let bin = ((usize::try_from(*column).unwrap_or(0) * 8) / width).min(7);
        counts[bin] += 1;
    }
    for y in 0..height {
        for column in columns {
            let bin = ((usize::try_from(*column).unwrap_or(0) * 8) / width).min(7);
            let p = (y * width + usize::try_from(*column).unwrap_or(0)) * 4;
            for c in 0..3 {
                rows[y * BINS + bin * 3 + c] += image.pixels[p + c] as f32 / counts[bin] as f32;
            }
        }
    }
    rows
}

#[derive(Clone, Copy, Debug)]
struct OverlapScore {
    shift: u32,
    score: f64,
    coverage: f64,
    signal: u64,
}

/// Tiled overlap verification; filters out local repaints such as tooltips or video controls.
#[allow(clippy::too_many_lines)]
fn overlap_score(
    previous: &Rgba,
    next: &Rgba,
    shift: u32,
    top: u32,
    bottom: u32,
    columns: &[u32],
) -> OverlapScore {
    let width = previous.width as usize;
    let height = previous.height as usize;
    let step = ((width as f64 / 128.0).floor() as usize).max(1);
    let tile_height = 48usize;
    let shift_usize = shift as usize;
    let top_usize = top as usize;
    let bottom_usize = bottom as usize;
    let usable = height.saturating_sub(bottom_usize + shift_usize + top_usize);
    let tile_rows = usable.div_ceil(tile_height);
    #[derive(Clone, Copy, Default)]
    struct Tile {
        error: u64,
        active: u64,
        matched: u64,
        signal: u64,
    }
    let mut tiles = vec![Tile::default(); tile_rows * 8];
    let mut error = 0u64;
    let mut active = 0u64;
    let mut matched = 0u64;
    let mut signal = 0u64;
    for y in top_usize..height.saturating_sub(bottom_usize + shift_usize) {
        for column in columns {
            let x = *column as usize;
            let p = ((y + shift_usize) * width + x) * 4;
            let q = (y * width + x) * 4;
            let mut delta = 0u32;
            let mut contrast = 0u32;
            for channel in 0..3 {
                delta += previous.pixels[p + channel].abs_diff(next.pixels[q + channel]) as u32;
                contrast += previous.pixels[p + channel]
                    .abs_diff(previous.pixels[p + channel - step * 4]) as u32
                    + next.pixels[q + channel].abs_diff(next.pixels[q + channel - step * 4])
                        as u32;
                if y > top_usize {
                    contrast += previous.pixels[p + channel]
                        .abs_diff(previous.pixels[p + channel - width * 4]) as u32
                        + next.pixels[q + channel]
                            .abs_diff(next.pixels[q + channel - width * 4]) as u32;
                }
            }
            if shift > 0 && delta > 36 {
                let mut unmoved_here = 0u32;
                let mut unmoved_there = 0u32;
                for channel in 0..3 {
                    unmoved_here +=
                        previous.pixels[q + channel].abs_diff(next.pixels[q + channel]) as u32;
                    unmoved_there +=
                        previous.pixels[p + channel].abs_diff(next.pixels[p + channel]) as u32;
                }
                // Fixed players/input bars stay at screen coordinates; their texture must not invalidate document shift
                if unmoved_here <= 6 && unmoved_there <= 6 {
                    continue;
                }
            }
            if contrast > 36 || delta > 36 {
                let tile_index = (y - top_usize) / tile_height * 8
                    + ((x * 8) / width).min(7);
                error += delta as u64;
                active += 1;
                tiles[tile_index].error += delta as u64;
                tiles[tile_index].active += 1;
                if delta <= 36 {
                    matched += 1;
                    tiles[tile_index].matched += 1;
                }
                if contrast > 36 && delta <= 36 {
                    signal += 1;
                    tiles[tile_index].signal += 1;
                }
            }
        }
    }
    let global = OverlapScore {
        shift,
        score: if active > 0 {
            error as f64 / (active * 3) as f64
        } else {
            f64::INFINITY
        },
        coverage: if active > 0 {
            matched as f64 / active as f64
        } else {
            0.0
        },
        signal,
    };
    if global.score <= 10.0 && global.coverage >= 0.88 && global.signal >= 8 {
        return global;
    }
    // When most textured tiles agree, exclude locally repainted outlier tiles
    let mut evidence = 0u64;
    let mut agreed = 0u64;
    let mut kept_active = 0u64;
    let mut kept_error = 0u64;
    let mut kept_matched = 0u64;
    let mut kept_signal = 0u64;
    let mut rows_found: std::collections::BTreeSet<usize> = Default::default();
    let mut bins_found: std::collections::BTreeSet<usize> = Default::default();
    for (index, tile) in tiles.iter().enumerate() {
        if tile.active < 8 {
            continue;
        }
        evidence += 1;
        if tile.error as f64 / (tile.active * 3) as f64 > 10.0
            || (tile.matched as f64 / tile.active as f64) < 0.88
            || tile.signal < 8
        {
            continue;
        }
        agreed += 1;
        kept_active += tile.active;
        kept_error += tile.error;
        kept_matched += tile.matched;
        kept_signal += tile.signal;
        rows_found.insert(index / 8);
        bins_found.insert(index % 8);
    }
    let max_row = rows_found.iter().max().copied().unwrap_or(0);
    let min_row = rows_found.iter().min().copied().unwrap_or(0);
    if agreed < (evidence as f64 * 0.65).ceil() as u64
        || kept_active < (active as f64 * 0.65) as u64
        || kept_signal < 24
        || (rows_found.len() as u32) < (tile_rows as u32).min(4)
        || bins_found.len() < 2
        || ((max_row - min_row + 1) as usize) * tile_height < (usable as f64 * 0.6) as usize
    {
        return global;
    }
    OverlapScore {
        shift,
        score: kept_error as f64 / (kept_active * 3) as f64
            + (1.0 - kept_active as f64 / active as f64) * 12.0,
        coverage: kept_matched as f64 / kept_active as f64,
        signal: kept_signal,
    }
}

/// Find the vertical scroll shift between two frames (matches legacy findAlignment).
pub fn find_alignment(
    previous: &Rgba,
    next: &Rgba,
    sticky_top: u32,
    sticky_bottom: u32,
) -> Result<Alignment, String> {
    if previous.width != next.width || previous.height != next.height {
        return Err(i18n::t("Selection area size changed").into());
    }
    if previous.pixels == next.pixels {
        return Ok(Alignment {
            shift: 0,
            top: 0,
            bottom: 0,
        });
    }
    let h = previous.height;
    let columns = content_columns(previous, next);
    let top = (h / 3)
        .min(if sticky_top > 0 {
            sticky_top
        } else {
            fixed_edge(previous, next, &columns, false)
        })
        .max(0);
    let bottom = (h / 3)
        .min(if sticky_bottom > 0 {
            sticky_bottom
        } else {
            fixed_edge(previous, next, &columns, true)
        })
        .max(0);
    if columns.is_empty() {
        if difference(previous, next) < 0.75 {
            return Ok(Alignment {
                shift: 0,
                top: 0,
                bottom,
            });
        }
        return Err(i18n::t(
            "Unable to align scrolled content; please select an area with distinct visual features",
        ).into());
    }
    let inner = h.saturating_sub(top + bottom);
    let min_overlap = inner.min(((inner as f64 * 0.3).ceil() as u32).max(16));
    let max_shift = inner.saturating_sub(min_overlap);
    let a = row_profiles(previous, &columns);
    let b = row_profiles(next, &columns);
    const BINS: usize = 24;
    struct Candidate {
        shift: u32,
        score: f64,
        robust_score: f64,
    }
    let mut scores = Vec::new();
    for shift in 0..=max_shift {
        let usable = h - bottom - shift - top;
        let mut errors = [0f64; 8];
        for y in top..(h - bottom - shift) {
            for bin in 0..BINS {
                errors[bin / 3] +=
                    (a[((y + shift) as usize) * BINS + bin] - b[y as usize * BINS + bin])
                        .abs() as f64;
            }
        }
        let mut ranked: Vec<f64> = (0..8)
            .filter(|bin| {
                columns.iter().any(|column| {
                    ((*column as usize * 8) / previous.width as usize).min(7) == *bin
                })
            })
            .map(|bin| errors[bin])
            .collect();
        ranked.sort_by(|x, y| x.total_cmp(y));
        let keep = ranked.len().min(3usize.max((ranked.len() as f64 * 0.75).ceil() as usize));
        let sum: f64 = ranked.iter().sum();
        let kept: f64 = ranked[..keep].iter().sum();
        scores.push(Candidate {
            shift,
            score: sum / (usable as f64 * ranked.len() as f64 * 3.0),
            robust_score: if keep > 0 {
                kept / (usable as f64 * keep as f64 * 3.0)
            } else {
                f64::INFINITY
            },
        });
    }
    // Collect local minima from both metrics as candidate shifts
    let mut candidates: std::collections::BTreeMap<u32, (f64, f64)> = Default::default();
    for key in [0usize, 1usize] {
        let metric = |entry: &Candidate| {
            if key == 0 { entry.score } else { entry.robust_score }
        };
        let mut entries: Vec<&Candidate> = scores
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                let value = metric(entry);
                let previous_ok = *index == 0 || value <= metric(&scores[index - 1]);
                let next_ok = *index == scores.len() - 1 || value <= metric(&scores[index + 1]);
                previous_ok && next_ok
            })
            .map(|(_, entry)| entry)
            .collect();
        entries.sort_by(|x, y| {
            metric(x).total_cmp(&metric(y)).then(x.shift.cmp(&y.shift))
        });
        for entry in entries.into_iter().take(24) {
            candidates
                .entry(entry.shift)
                .or_insert((entry.score, entry.robust_score));
        }
    }
    if !candidates.contains_key(&0) && !scores.is_empty() {
        candidates.insert(scores[0].shift, (scores[0].score, scores[0].robust_score));
    }
    let mut matches: Vec<OverlapScore> = candidates
        .keys()
        .map(|shift| overlap_score(previous, next, *shift, top, bottom, &columns))
        .filter(|score| score.score <= 10.0 && score.coverage >= 0.88 && score.signal >= 8)
        .collect();
    matches.sort_by(|x, y| {
        x.score.total_cmp(&y.score).then(x.shift.cmp(&y.shift))
    });
    let best = matches.first().copied();
    let Some(best) = best else {
        return Err(i18n::t(
            "Unable to align scrolled content; scroll down slowly or return to the previous view and retry (set header/footer heights for fixed bars)",
        ).into());
    };
    if best.shift == 0 {
        return Ok(Alignment {
            shift: 0,
            top,
            bottom,
        });
    }
    // Another candidate with a very close score indicates an ambiguous seam (repetitive layout)
    if matches.iter().any(|entry| {
        entry.shift.abs_diff(best.shift) > 1
            && entry.score <= best.score + (0.75f64).max(best.score * 0.15)
    }) {
        return Err(i18n::t(
            "Scrolled content is too repetitive to determine the seam; scroll a shorter distance or select an area with distinct features",
        ).into());
    }
    Ok(Alignment {
        shift: best.shift,
        top,
        bottom,
    })
}

/// Stitch a new frame onto the assembled scrolling image (matches legacy appendFrame; refreshes overlapping suffix).
pub fn append_frame(
    assembled: &Rgba,
    previous: &Rgba,
    next: &Rgba,
    sticky_top: u32,
    sticky_bottom: u32,
) -> Result<(Rgba, u32), String> {
    let alignment = find_alignment(previous, next, sticky_top, sticky_bottom)?;
    let shift = alignment.shift;
    if shift == 0 {
        return Ok((assembled.clone(), 0));
    }
    if assembled.height + shift > 30_000
        || (assembled.height + shift) as u64 * assembled.width as u64 > 80_000_000
    {
        return Err(i18n::t("Scrolling capture reached the maximum size limit").into());
    }
    let new_height = assembled.height + shift;
    let mut output = vec![0u8; new_height as usize * assembled.width as usize * 4];
    // Refresh the overlapping suffix as well so fixed input bars / floating players do not burn into every seam
    let seam = alignment.top
        + ((next.height.saturating_sub(alignment.top + alignment.bottom + shift)) / 4).min(32);
    let retained = assembled.height - previous.height + shift + seam;
    let width_bytes = assembled.width as usize * 4;
    // Retained rows from the existing assembled image
    let kept_rows = retained.min(assembled.height) as usize;
    output[..kept_rows as usize * width_bytes]
        .copy_from_slice(&assembled.pixels[..kept_rows * width_bytes]);
    // Append new frame starting from the seam
    for row in 0..(next.height - seam.min(next.height)) as usize {
        let src_start = (seam as usize + row) * width_bytes;
        let dst_start = (retained as usize + row) * width_bytes;
        output[dst_start..dst_start + width_bytes]
            .copy_from_slice(&next.pixels[src_start..src_start + width_bytes]);
    }
    Ok((Rgba::new(assembled.width, new_height, output), shift))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_frame(width: u32, height: u32, offset: u32) -> Rgba {
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let p = ((y * width + x) * 4) as usize;
                let value = (((y + offset) * 7 + x * 3) % 256) as u8;
                pixels[p] = value;
                pixels[p + 1] = ((value as u16 * 2) % 256) as u8;
                pixels[p + 2] = 255 - value;
                pixels[p + 3] = 255;
            }
        }
        Rgba::new(width, height, pixels)
    }

    #[test]
    fn identical_frames_have_zero_difference() {
        let frame = gradient_frame(64, 64, 0);
        assert!(difference(&frame, &frame.clone()) < 0.001);
    }

    #[test]
    fn identical_frames_align_to_zero() {
        let frame = gradient_frame(120, 200, 0);
        let result = find_alignment(&frame, &frame.clone(), 0, 0).unwrap();
        assert_eq!(result.shift, 0);
    }

    #[test]
    fn scrolled_frames_align_to_scroll_distance() {
        // Next frame scrolled upward by 20px relative to the previous frame
        let previous = gradient_frame(160, 300, 0);
        let next = gradient_frame(160, 300, 20);
        let result = find_alignment(&previous, &next, 0, 0).unwrap();
        assert_eq!(result.shift, 20);
    }

    #[test]
    fn append_frame_grows_image_by_shift() {
        let first = gradient_frame(160, 300, 0);
        let second = gradient_frame(160, 300, 20);
        let (assembled, shift) = append_frame(&first, &first, &second, 0, 0).unwrap();
        assert_eq!(shift, 20);
        assert_eq!(assembled.height, 320);
    }

    #[test]
    fn append_frame_rejects_size_limit() {
        // Use small frames for fast alignment; the size limit is triggered by assembled height
        let frame = gradient_frame(160, 300, 0);
        let next = gradient_frame(160, 300, 20);
        let assembled = gradient_frame(160, 29_990, 0);
        let error = append_frame(&assembled, &frame, &next, 0, 0).unwrap_err();
        assert_eq!(error, i18n::t("Scrolling capture reached the maximum size limit"));
    }

    #[test]
    fn mismatched_sizes_are_rejected() {
        let a = gradient_frame(100, 100, 0);
        let b = gradient_frame(100, 120, 0);
        assert!(find_alignment(&a, &b, 0, 0).is_err());
    }
}
