use similar::{ChangeTag, TextDiff};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffTag {
    Equal,
    Delete,
    Insert,
}

/// One line of a line diff; line numbers are 0-based and present on the side(s) the line exists in.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub tag: DiffTag,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    /// The line without its terminator.
    pub text: String,
}

pub fn diff_text(old: &str, new: &str) -> Vec<DiffLine> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|change| DiffLine {
            tag: match change.tag() {
                ChangeTag::Equal => DiffTag::Equal,
                ChangeTag::Delete => DiffTag::Delete,
                ChangeTag::Insert => DiffTag::Insert,
            },
            old_line: change.old_index(),
            new_line: change.new_index(),
            text: change.value().trim_end_matches(['\n', '\r']).to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(tag: DiffTag, old_line: Option<usize>, new_line: Option<usize>, text: &str) -> DiffLine {
        DiffLine { tag, old_line, new_line, text: text.to_string() }
    }

    #[test]
    fn line_diff_with_numbers() {
        let diff = diff_text("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(
            diff,
            vec![
                line(DiffTag::Equal, Some(0), Some(0), "a"),
                line(DiffTag::Delete, Some(1), None, "b"),
                line(DiffTag::Insert, None, Some(1), "B"),
                line(DiffTag::Equal, Some(2), Some(2), "c"),
                line(DiffTag::Insert, None, Some(3), "d"),
            ]
        );
    }

    #[test]
    fn identical_and_empty_inputs() {
        assert!(diff_text("x\r\ny\r\n", "x\r\ny\r\n").iter().all(|l| l.tag == DiffTag::Equal));
        assert_eq!(diff_text("x\r\n", "x\r\n")[0].text, "x");
        assert!(diff_text("", "").is_empty());
        assert_eq!(diff_text("", "new"), vec![line(DiffTag::Insert, None, Some(0), "new")]);
        assert_eq!(diff_text("old\n", ""), vec![line(DiffTag::Delete, Some(0), None, "old")]);
    }
}
