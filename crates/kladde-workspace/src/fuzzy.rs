const SCORE_MATCH: i32 = 16;
const BONUS_PATH_START: i32 = 24;
const BONUS_WORD_START: i32 = 20;
const BONUS_CONSECUTIVE: i32 = 24;
const BONUS_BASENAME: i32 = 10;
const IMPOSSIBLE: i32 = i32::MIN / 2;

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn is_separator(c: char) -> bool {
    matches!(c, '/' | '\\')
}

/// Score for a match at `j`, depending on what precedes it.
fn position_bonus(chars: &[char], j: usize, basename_start: usize) -> i32 {
    let start = match j.checked_sub(1).map(|p| chars[p]) {
        None => BONUS_PATH_START,
        Some(prev) if is_separator(prev) => BONUS_PATH_START,
        Some('_' | '-' | '.' | ' ') => BONUS_WORD_START,
        Some(prev) if prev.is_lowercase() && chars[j].is_uppercase() => BONUS_WORD_START,
        Some(_) => 0,
    };
    let base = if j >= basename_start { BONUS_BASENAME } else { 0 };
    SCORE_MATCH + start + base
}

/// Best subsequence alignment score of `pattern` in `candidate` (case-insensitive), `None` if no match.
/// Rewards word starts, consecutive runs and matches in the file name; penalizes gaps between matches.
pub fn fuzzy_score(pattern: &str, candidate: &str) -> Option<i32> {
    let pat: Vec<char> = pattern.chars().map(fold).collect();
    let chars: Vec<char> = candidate.chars().collect();
    let folded: Vec<char> = chars.iter().copied().map(fold).collect();
    let basename_start = chars.iter().rposition(|c| is_separator(*c)).map_or(0, |p| p + 1);
    let n = chars.len();
    let first_char = pat.first().copied().unwrap_or('\0');
    // row[j]: best score with the current pattern char matched at j
    let first: Vec<i32> = (0..n)
        .map(|j| if folded[j] == first_char { position_bonus(&chars, j, basename_start) } else { IMPOSSIBLE })
        .collect();
    let last = pat.iter().skip(1).fold(first, |prev, &pc| {
        // best_gap = max over k < j-1 of prev[k] + k; 1 point per skipped char
        let mut best_gap = IMPOSSIBLE;
        (0..n)
            .map(|j| {
                let score = if j > 0 && folded[j] == pc {
                    let consecutive = prev[j - 1] + BONUS_CONSECUTIVE;
                    let gapped = best_gap - (j as i32 - 1);
                    consecutive.max(gapped) + position_bonus(&chars, j, basename_start)
                } else {
                    IMPOSSIBLE
                };
                if j > 0 {
                    best_gap = best_gap.max(prev[j - 1] + j as i32 - 1);
                }
                score
            })
            .collect()
    });
    if pat.is_empty() {
        Some(0)
    } else {
        last.into_iter().filter(|s| *s > IMPOSSIBLE / 2).max()
    }
}

/// Indices and scores of matching candidates, best first; ties go to the shorter candidate, then lower index.
pub fn fuzzy_rank(pattern: &str, candidates: &[&str], limit: usize) -> Vec<(usize, i32)> {
    let mut ranked: Vec<(usize, i32)> =
        candidates.iter().enumerate().filter_map(|(i, c)| fuzzy_score(pattern, c).map(|s| (i, s))).collect();
    ranked.sort_by(|a, b| {
        let len = |i: usize| candidates[i].chars().count();
        b.1.cmp(&a.1).then(len(a.0).cmp(&len(b.0))).then(a.0.cmp(&b.0))
    });
    ranked.truncate(limit);
    ranked
}
