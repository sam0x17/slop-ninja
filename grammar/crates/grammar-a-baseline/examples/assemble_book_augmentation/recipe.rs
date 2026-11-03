//! Exact frozen default-v2 span/prompt and PG excerpt algorithms.
//! Producer and extractor source hashes are pinned in the assembly plan.
use super::*;
fn words(s: &str) -> usize {
    s.split_whitespace().count()
}
pub(super) fn span(text: &str, percent: usize, position: usize) -> (usize, usize) {
    if percent == 100 {
        return (0, text.len());
    }
    let desired = (words(text) * percent / 100).max(1);
    let mut starts = vec![0];
    let mut boundaries = vec![text.len()];
    for (i, c) in text.char_indices() {
        if matches!(c, '.' | '?' | '!') {
            let end = i + c.len_utf8();
            if end == text.len() || text[end..].starts_with(char::is_whitespace) {
                boundaries.push(end);
                let next = end + text[end..].len() - text[end..].trim_start().len();
                if next < text.len() {
                    starts.push(next);
                }
            }
        }
    }
    if boundaries.len() <= 2 {
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                boundaries.push(i);
                let next = i + text[i..].len() - text[i..].trim_start().len();
                if next < text.len() {
                    starts.push(next);
                }
            }
        }
    }
    let total = words(text);
    starts
        .iter()
        .flat_map(|&s| boundaries.iter().map(move |&e| (s, e)))
        .filter(|&(s, e)| s < e && (s > 0 || e < text.len()))
        .filter(|&(s, e)| match position {
            0 => s == 0,
            2 => e == text.len(),
            _ => true,
        })
        .min_by_key(|&(s, e)| {
            let count = words(&text[s..e]);
            let midpoint = 2 * words(&text[..s]) + count;
            (
                count.abs_diff(desired),
                if position == 1 {
                    midpoint.abs_diff(total)
                } else {
                    0
                },
                s,
                e,
            )
        })
        .unwrap_or((0, text.len()))
}

pub(super) fn scratch_prompt(id: &str) -> String {
    let d = Sha256::digest(id.as_bytes());
    let places = [
        "a coastal village",
        "a railway town",
        "a remote farm",
        "a crowded city",
        "a mountain inn",
        "a small port",
        "a rural school",
        "a family home",
    ];
    let events = [
        "an unexpected visitor",
        "a missing letter",
        "a misunderstanding",
        "a difficult promise",
        "a sudden storm",
        "a secret discovered",
        "a change of plans",
        "a reunion",
    ];
    let objects = [
        "a broken clock",
        "an old map",
        "a lost key",
        "a worn coat",
        "a wooden box",
        "a faded photograph",
        "a borrowed bicycle",
        "a silver spoon",
    ];
    let roles = [
        "a gardener",
        "a teacher",
        "a shopkeeper",
        "a carpenter",
        "a musician",
        "a sailor",
        "a baker",
        "a librarian",
    ];
    let times = [
        "before dawn",
        "on a winter evening",
        "during a summer afternoon",
        "late at night",
        "on a rainy morning",
        "at the end of autumn",
        "at noon",
        "on a spring evening",
    ];
    let conflicts = [
        "an unwelcome obligation",
        "a hopeful invitation",
        "an uncertain memory",
        "a quiet rivalry",
        "a difficult departure",
        "a generous gesture",
        "a mistaken accusation",
        "a change of heart",
    ];
    format!(
        "Write an original English fictional scene of about 220 words set in {} involving {}, {}, {}, {}, and {}. Use natural narrative prose. Return only the scene, with no title, analysis, introduction, or explanation.",
        places[d[0] as usize % 8],
        events[d[1] as usize % 8],
        objects[d[2] as usize % 8],
        roles[d[3] as usize % 8],
        times[d[4] as usize % 8],
        conflicts[d[5] as usize % 8]
    )
}

fn body(text: &str) -> Result<(usize, usize)> {
    let marker = text.find("*** START OF").context("PG start marker")?;
    let start = marker + text[marker..].find('\n').context("PG marker line")? + 1;
    let end = text[start..].find("*** END OF").context("PG end marker")? + start;
    ensure!(start < end, "PG body bounds");
    Ok((start, end))
}
pub(super) fn excerpt(text: &str, fraction: usize) -> Result<(usize, usize)> {
    let (b, e) = body(text)?;
    let t = &text[b..e];
    let offsets = t
        .char_indices()
        .filter_map(|(i, c)| {
            (!c.is_whitespace() && (i == 0 || t[..i].ends_with(char::is_whitespace))).then_some(i)
        })
        .collect::<Vec<_>>();
    ensure!(offsets.len() > 1500, "short source work");
    let desired = offsets[offsets.len() * fraction / 100];
    let boundary = t[desired..]
        .char_indices()
        .find_map(|(i, c)| {
            let end = desired + i + c.len_utf8();
            (matches!(c, '.' | '?' | '!')
                && end < t.len()
                && t[end..].starts_with(char::is_whitespace))
            .then_some(end)
        })
        .context("sentence start boundary")?;
    let start = boundary + (t[boundary..].len() - t[boundary..].trim_start().len());
    let mut count = 0usize;
    let mut end = None;
    for (i, c) in t[start..].char_indices() {
        if !c.is_whitespace() && (i == 0 || t[start..start + i].ends_with(char::is_whitespace)) {
            count += 1;
        }
        let p = start + i + c.len_utf8();
        if count >= 220
            && matches!(c, '.' | '?' | '!')
            && (p == t.len() || t[p..].starts_with(char::is_whitespace))
        {
            end = Some(p);
            break;
        }
    }
    let end = end.context("sentence end boundary")?;
    ensure!(
        t[start..end].split_whitespace().count() <= 600,
        "excerpt exceeds fixed bound"
    );
    Ok((b + start, b + end))
}
