//! Validate packed video buffers and normalize them to tightly packed BGRA.

use pipewire::spa::sys as spa;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameFormat {
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
}

impl FrameFormat {
    pub fn frame_len(self) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(4)
    }
}

pub(crate) fn copy_frame(
    memory: &[u8],
    chunk: &spa::spa_chunk,
    format: FrameFormat,
) -> Result<Option<Vec<u8>>, &'static str> {
    if chunk.size == 0
        || chunk.flags & (spa::SPA_CHUNK_FLAG_CORRUPTED | spa::SPA_CHUNK_FLAG_EMPTY) as i32 != 0
    {
        return Ok(None);
    }
    if memory.is_empty() || format.width == 0 || format.height == 0 {
        return Err("Empty video buffer or dimensions");
    }
    let (swap_red_blue, opaque) = match format.pixel_format {
        spa::SPA_VIDEO_FORMAT_BGRA => (false, false),
        spa::SPA_VIDEO_FORMAT_BGRx => (false, true),
        spa::SPA_VIDEO_FORMAT_RGBA => (true, false),
        spa::SPA_VIDEO_FORMAT_RGBx => (true, true),
        _ => return Err("Unsupported packed video format"),
    };
    let row_bytes = (format.width as usize)
        .checked_mul(4)
        .ok_or("Video row size overflow")?;
    // A zero stride denotes tightly packed data. Reject negative strides rather
    // than interpreting them as huge unsigned offsets into mapped memory.
    let stride = match chunk.stride {
        0 => row_bytes,
        n if n > 0 => n as usize,
        _ => return Err("Negative video stride is unsupported"),
    };
    if stride < row_bytes {
        return Err("Video stride is shorter than a row");
    }
    let required = (format.height as usize - 1)
        .checked_mul(stride)
        .and_then(|n| n.checked_add(row_bytes))
        .ok_or("Video buffer size overflow")?;
    // SPA specifies offset modulo maxsize, and size clamped to maxsize.
    let offset = chunk.offset as usize % memory.len();
    let size = (chunk.size as usize).min(memory.len());
    if required > size || required > memory.len() - offset {
        return Err("Video chunk extends beyond the mapped buffer");
    }
    let frame_len = format.frame_len().ok_or("Video frame size overflow")?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(frame_len)
        .map_err(|_| "Could not allocate video frame")?;
    for y in 0..format.height as usize {
        let start = offset + y * stride;
        frame.extend_from_slice(&memory[start..start + row_bytes]);
    }
    if swap_red_blue || opaque {
        for pixel in frame.chunks_exact_mut(4) {
            if swap_red_blue {
                pixel.swap(0, 2);
            }
            if opaque {
                pixel[3] = 255;
            }
        }
    }
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(pixel_format: u32) -> FrameFormat {
        FrameFormat {
            width: 1,
            height: 2,
            pixel_format,
        }
    }

    fn chunk() -> spa::spa_chunk {
        spa::spa_chunk {
            offset: 0,
            size: 8,
            stride: 4,
            flags: 0,
        }
    }

    #[test]
    fn copies_padded_rows_from_nonzero_offset_without_padding() {
        let memory = [99, 99, 3, 2, 1, 255, 88, 88, 6, 5, 4, 128, 77, 77];
        let chunk = spa::spa_chunk {
            offset: 2,
            size: 12,
            stride: 6,
            flags: 0,
        };
        assert_eq!(
            copy_frame(&memory, &chunk, format(spa::SPA_VIDEO_FORMAT_BGRA))
                .unwrap()
                .unwrap(),
            [3, 2, 1, 255, 6, 5, 4, 128]
        );
    }

    #[test]
    fn normalizes_all_negotiated_formats_to_bgra() {
        for (pixel_format, expected) in [
            (spa::SPA_VIDEO_FORMAT_BGRA, vec![1, 2, 3, 4, 5, 6, 7, 8]),
            (spa::SPA_VIDEO_FORMAT_BGRx, vec![1, 2, 3, 255, 5, 6, 7, 255]),
            (spa::SPA_VIDEO_FORMAT_RGBA, vec![3, 2, 1, 4, 7, 6, 5, 8]),
            (spa::SPA_VIDEO_FORMAT_RGBx, vec![3, 2, 1, 255, 7, 6, 5, 255]),
        ] {
            assert_eq!(
                copy_frame(&[1, 2, 3, 4, 5, 6, 7, 8], &chunk(), format(pixel_format))
                    .unwrap()
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn rejects_truncated_rows_and_invalid_strides() {
        let memory = [0; 8];
        for invalid in [
            spa::spa_chunk { size: 7, ..chunk() },
            spa::spa_chunk {
                offset: 1,
                ..chunk()
            },
            spa::spa_chunk {
                stride: 3,
                ..chunk()
            },
            spa::spa_chunk {
                stride: -4,
                ..chunk()
            },
            spa::spa_chunk {
                stride: i32::MAX,
                size: u32::MAX,
                ..chunk()
            },
        ] {
            assert!(copy_frame(&memory, &invalid, format(spa::SPA_VIDEO_FORMAT_BGRA)).is_err());
        }
    }

    #[test]
    fn handles_spa_offset_and_size_rules() {
        let memory = [1; 8];
        let chunk = spa::spa_chunk {
            offset: 8,
            size: u32::MAX,
            ..chunk()
        };
        assert_eq!(
            copy_frame(&memory, &chunk, format(spa::SPA_VIDEO_FORMAT_BGRA))
                .unwrap()
                .unwrap(),
            memory
        );
    }

    #[test]
    fn ignores_empty_and_corrupted_chunks() {
        for chunk in [
            spa::spa_chunk { size: 0, ..chunk() },
            spa::spa_chunk {
                flags: spa::SPA_CHUNK_FLAG_CORRUPTED as i32,
                ..chunk()
            },
            spa::spa_chunk {
                flags: spa::SPA_CHUNK_FLAG_EMPTY as i32,
                ..chunk()
            },
        ] {
            assert!(copy_frame(&[], &chunk, format(spa::SPA_VIDEO_FORMAT_BGRA))
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn rejects_invalid_dimensions_and_formats() {
        for invalid in [
            FrameFormat {
                width: 0,
                ..format(spa::SPA_VIDEO_FORMAT_BGRA)
            },
            FrameFormat {
                width: u32::MAX,
                height: u32::MAX,
                ..format(spa::SPA_VIDEO_FORMAT_BGRA)
            },
            format(spa::SPA_VIDEO_FORMAT_UNKNOWN),
        ] {
            assert!(copy_frame(&[0; 8], &chunk(), invalid).is_err());
        }
    }
}
