#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub fn fit_rect(source: Size, target: Size) -> Rect {
    let source_width = u64::from(source.width);
    let source_height = u64::from(source.height);
    let target_width = u64::from(target.width);
    let target_height = u64::from(target.height);
    let source_is_wider = source_width * target_height >= source_height * target_width;
    let (width, height) = if source_is_wider {
        let height = scale_rounded(target_width, source_height, source_width);
        (target.width, height.clamp(1, target.height))
    } else {
        let width = scale_rounded(target_height, source_width, source_height);
        (width.clamp(1, target.width), target.height)
    };
    Rect {
        x: (target.width - width) / 2,
        y: (target.height - height) / 2,
        width,
        height,
    }
}

fn scale_rounded(length: u64, numerator: u64, denominator: u64) -> u32 {
    let scaled = (length * numerator + denominator / 2) / denominator;
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn exact_size_covers_the_target() {
        assert_eq!(
            fit_rect(size(1920, 1080), size(1920, 1080)),
            rect(0, 0, 1920, 1080)
        );
    }

    #[test]
    fn same_aspect_scales_to_cover_the_target() {
        assert_eq!(
            fit_rect(size(3840, 2160), size(1920, 1080)),
            rect(0, 0, 1920, 1080)
        );
    }

    #[test]
    fn wider_source_is_letterboxed() {
        assert_eq!(
            fit_rect(size(2400, 1000), size(1920, 1080)),
            rect(0, 140, 1920, 800)
        );
    }

    #[test]
    fn taller_source_is_pillarboxed() {
        assert_eq!(
            fit_rect(size(1080, 1920), size(1920, 1080)),
            rect(656, 0, 608, 1080)
        );
    }

    #[test]
    fn smaller_source_scales_up_to_fit() {
        assert_eq!(
            fit_rect(size(640, 480), size(1920, 1080)),
            rect(240, 0, 1440, 1080)
        );
        assert_eq!(
            fit_rect(size(160, 90), size(1920, 1080)),
            rect(0, 0, 1920, 1080)
        );
    }

    #[test]
    fn odd_sizes_round_and_centre() {
        assert_eq!(fit_rect(size(3, 2), size(7, 5)), rect(0, 0, 7, 5));
        assert_eq!(fit_rect(size(4, 1), size(7, 5)), rect(0, 1, 7, 2));
        assert_eq!(fit_rect(size(1, 3), size(7, 5)), rect(2, 0, 2, 5));
    }

    #[test]
    fn extreme_aspect_keeps_at_least_one_pixel() {
        assert_eq!(
            fit_rect(size(10_000, 1), size(100, 100)),
            rect(0, 49, 100, 1)
        );
    }
}
