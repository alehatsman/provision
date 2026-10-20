//! Failure-block presentation policy (spec §9.1): how much of a captured
//! stream the text renderer shows. Only `text.rs` reads it.

/// The failure block's view of one captured stream: its first `head` lines,
/// its last `tail`, how many fell between them, and how many there were.
/// Spec §9.1 shows 10 and 20.
///
/// Two windows rather than one, because the two ends answer different
/// questions. A linter names the rule it broke first and tallies files last,
/// so a tail alone showed the tally and dropped the rule — the one line the
/// failure was about (#59). The cap itself stays: a step that printed ten
/// thousand lines must not bury the summary under them.
///
/// A stream short enough to fit comes back whole with nothing elided, and the
/// caller prints no marker. That absence is how a reader knows nothing was
/// cut, which is why the elided count is returned rather than inferred.
pub(crate) fn window(text: &str, head: usize, tail: usize) -> Window<'_> {
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = lines.len();
    if total <= head + tail {
        return Window {
            head: lines,
            tail: Vec::new(),
            elided: 0,
            total,
        };
    }
    let rest = lines.split_off(total - tail);
    lines.truncate(head);
    Window {
        head: lines,
        tail: rest,
        elided: total - head - tail,
        total,
    }
}

/// What `window` returns: the two ends it kept, and the two numbers the
/// elision marker names — what was dropped, out of what there was. Either
/// number alone is a half-answer, so they travel together.
pub(crate) struct Window<'a> {
    pub head: Vec<&'a str>,
    pub tail: Vec<&'a str>,
    pub elided: usize,
    pub total: usize,
}
