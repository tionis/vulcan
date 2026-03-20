use std::ops::Range;

#[must_use]
pub fn scan_comment_regions(source: &str) -> Vec<Range<usize>> {
    let bytes = source.as_bytes();
    let mut regions = Vec::new();
    let mut open_region = None;
    let mut index = 0;

    while index + 1 < bytes.len() {
        if bytes[index] == b'%' && bytes[index + 1] == b'%' {
            if let Some(start) = open_region.take() {
                regions.push(start..index + 2);
            } else {
                open_region = Some(index);
            }
            index += 2;
        } else {
            index += 1;
        }
    }

    regions
}

#[must_use]
pub fn overlaps_comment(range: &Range<usize>, comment_regions: &[Range<usize>]) -> bool {
    comment_regions
        .iter()
        .any(|comment| comment.start < range.end && range.start < comment.end)
}

#[must_use]
pub fn visible_subranges(
    range: Range<usize>,
    comment_regions: &[Range<usize>],
) -> Vec<Range<usize>> {
    let mut visible = Vec::new();
    let mut cursor = range.start;

    for comment in comment_regions
        .iter()
        .filter(|comment| comment.start < range.end && range.start < comment.end)
    {
        if cursor < comment.start {
            visible.push(cursor..comment.start.min(range.end));
        }
        cursor = cursor.max(comment.end.min(range.end));
        if cursor >= range.end {
            break;
        }
    }

    if cursor < range.end {
        visible.push(cursor..range.end);
    }

    visible
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_comments_are_recorded() {
        let source = "alpha %%secret%% beta";

        assert_eq!(scan_comment_regions(source), vec![6..16]);
    }

    #[test]
    fn nested_markers_pair_linearly() {
        let source = "%%outer %% inner %%";

        assert_eq!(scan_comment_regions(source), vec![0..10]);
    }

    #[test]
    fn unclosed_comments_are_treated_as_literal_text() {
        let source = "alpha %%secret";

        assert!(scan_comment_regions(source).is_empty());
    }

    #[test]
    fn adjacent_comments_are_recorded_as_single_region() {
        let source = "%%%%";

        assert_eq!(scan_comment_regions(source), vec![0..4]);
    }
}
