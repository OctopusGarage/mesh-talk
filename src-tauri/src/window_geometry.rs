use tauri::LogicalSize;

/// Returns a replacement for a restored window size only when it violates the
/// configured minimum. Valid user-selected sizes remain untouched.
pub(crate) fn repaired_startup_size(
    restored: LogicalSize<f64>,
    minimum: LogicalSize<f64>,
    preferred: LogicalSize<f64>,
) -> Option<LogicalSize<f64>> {
    let width = if restored.width < minimum.width {
        preferred.width.max(minimum.width)
    } else {
        restored.width
    };
    let height = if restored.height < minimum.height {
        preferred.height.max(minimum.height)
    } else {
        restored.height
    };
    (width != restored.width || height != restored.height)
        .then_some(LogicalSize::new(width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_collapsed_height_to_preferred_size() {
        assert_eq!(
            repaired_startup_size(
                LogicalSize::new(932.0, 24.0),
                LogicalSize::new(760.0, 520.0),
                LogicalSize::new(1040.0, 720.0),
            ),
            Some(LogicalSize::new(932.0, 720.0))
        );
    }

    #[test]
    fn preserves_valid_user_size() {
        assert_eq!(
            repaired_startup_size(
                LogicalSize::new(900.0, 650.0),
                LogicalSize::new(760.0, 520.0),
                LogicalSize::new(1040.0, 720.0),
            ),
            None
        );
    }
}
