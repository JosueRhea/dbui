//! Line-level diff for the change bubble.
//!
//! A JSON cell is a document, not a word: showing `old → new` for one means
//! printing the whole blob twice to convey that one key moved. This reduces a
//! before/after pair to the lines that actually differ.

/// One line of a rendered diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Removed(String),
    Added(String),
}

/// Diffing is O(n·m); past this many lines on a side we stop and say so rather
/// than allocate a matrix for a megabyte of JSON.
const MAX_LINES: usize = 400;

/// The changed lines between `old` and `new`, removals before additions within
/// each run. Context lines are left out: the bubble is a summary, not a review.
///
/// Returns `None` when either side is too large to diff, so the caller can
/// fall back to a plain before/after preview.
pub fn line_diff(old: &str, new: &str) -> Option<Vec<DiffLine>> {
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    if old_lines.len() > MAX_LINES || new_lines.len() > MAX_LINES {
        return None;
    }

    let keep = longest_common_subsequence(&old_lines, &new_lines);

    let mut out = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    for (keep_i, keep_j) in keep {
        // Everything before the next common line is a change. Removals are
        // emitted first so a replaced line reads `-old` then `+new`.
        let mut removed = Vec::new();
        while i < keep_i {
            removed.push(DiffLine::Removed(old_lines[i].to_string()));
            i += 1;
        }
        let mut added = Vec::new();
        while j < keep_j {
            added.push(DiffLine::Added(new_lines[j].to_string()));
            j += 1;
        }
        out.extend(removed);
        out.extend(added);
        i += 1;
        j += 1;
    }
    while i < old_lines.len() {
        out.push(DiffLine::Removed(old_lines[i].to_string()));
        i += 1;
    }
    while j < new_lines.len() {
        out.push(DiffLine::Added(new_lines[j].to_string()));
        j += 1;
    }

    Some(out)
}

/// Indices of lines common to both sides, as `(old index, new index)` pairs in
/// increasing order.
fn longest_common_subsequence(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let rows = old.len() + 1;
    let cols = new.len() + 1;
    let mut table = vec![0u32; rows * cols];

    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i * cols + j] = if old[i] == new[j] {
                table[(i + 1) * cols + j + 1] + 1
            } else {
                table[(i + 1) * cols + j].max(table[i * cols + j + 1])
            };
        }
    }

    let mut pairs = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[(i + 1) * cols + j] >= table[i * cols + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(old: &str, new: &str) -> Vec<String> {
        line_diff(old, new)
            .expect("small enough to diff")
            .into_iter()
            .map(|line| match line {
                DiffLine::Removed(text) => format!("-{text}"),
                DiffLine::Added(text) => format!("+{text}"),
            })
            .collect()
    }

    #[test]
    fn one_changed_line_out_of_many_is_the_whole_diff() {
        let old = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}";
        let new = "{\n  \"a\": 1,\n  \"b\": 99,\n  \"c\": 3\n}";
        assert_eq!(rendered(old, new), vec!["-  \"b\": 2,", "+  \"b\": 99,"]);
    }

    #[test]
    fn an_added_line_has_no_removal_beside_it() {
        let old = "{\n  \"a\": 1\n}";
        let new = "{\n  \"a\": 1,\n  \"b\": 2\n}";
        assert_eq!(
            rendered(old, new),
            vec!["-  \"a\": 1", "+  \"a\": 1,", "+  \"b\": 2"]
        );
    }

    #[test]
    fn a_removed_line_stands_alone() {
        let old = "keep\ndrop\nkeep too";
        let new = "keep\nkeep too";
        assert_eq!(rendered(old, new), vec!["-drop"]);
    }

    #[test]
    fn identical_text_has_an_empty_diff() {
        assert!(rendered("same\nlines", "same\nlines").is_empty());
    }

    #[test]
    fn a_single_line_change_still_reads_as_a_replacement() {
        assert_eq!(rendered("before", "after"), vec!["-before", "+after"]);
    }

    #[test]
    fn an_oversized_value_refuses_rather_than_allocating() {
        let big = (0..MAX_LINES + 1)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(line_diff(&big, "x").is_none());
        assert!(line_diff("x", &big).is_none());
    }
}
