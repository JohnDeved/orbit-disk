//! Editing model of single-line text inputs (the toolbar search field), independent of GPUI:
//! the text, the selection, cursor movement by grapheme cluster and by word (⌥← / ⌥→, ⌥⌫, ⌥⌦)
//! and undo/redo (⌘Z / ⇧⌘Z). Like a native text field, a run of typing or of deleting at the
//! cursor is one undo step; moving the cursor, pasting or clearing starts a new one.
//!
//! Offsets are UTF-8 byte offsets on grapheme-cluster boundaries.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// Undo steps kept per field.
const UNDO_LIMIT: usize = 100;

/// What an edit was; consecutive typing or consecutive deletions at the cursor share one undo
/// step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Deletion,
    /// Paste, cut, clear: always its own step.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    text: String,
    selected: Range<usize>,
    reversed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TextEdit {
    text: String,
    selected: Range<usize>,
    /// The cursor is at the selection's start (extending backwards).
    reversed: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The last edit, while the next one at the cursor may join its undo step.
    coalesce: Option<EditKind>,
    /// The state before an input-method composition began (composition is not recorded).
    composition_base: Option<Snapshot>,
}

impl TextEdit {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selected(&self) -> Range<usize> {
        self.selected.clone()
    }

    pub fn reversed(&self) -> bool {
        self.reversed
    }

    pub fn cursor(&self) -> usize {
        if self.reversed { self.selected.start } else { self.selected.end }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Takes text that changed elsewhere (not an edit of this field): the cursor goes to the end
    /// and the history no longer applies.
    pub fn reset(&mut self, text: &str) {
        self.text = text.to_owned();
        self.selected = self.text.len()..self.text.len();
        self.reversed = false;
        self.undo.clear();
        self.redo.clear();
        self.coalesce = None;
        self.composition_base = None;
    }

    /// Sets the selection (a range of the platform input handler, a mouse selection).
    pub fn set_selection(&mut self, range: Range<usize>, reversed: bool) {
        let start = self.snap(range.start.min(range.end));
        let end = self.snap(range.start.max(range.end));
        self.selected = start..end;
        self.reversed = reversed && start != end;
        self.coalesce = None;
    }

    pub fn select_all(&mut self) {
        self.set_selection(0..self.text.len(), false);
    }

    /// Collapses the selection at `offset`.
    pub fn move_to(&mut self, offset: usize) {
        let offset = self.snap(offset);
        self.selected = offset..offset;
        self.reversed = false;
        self.coalesce = None;
    }

    /// Moves the cursor end of the selection to `offset`, keeping its anchor.
    pub fn select_to(&mut self, offset: usize) {
        let offset = self.snap(offset);
        let anchor = if self.reversed { self.selected.end } else { self.selected.start };
        self.selected = anchor.min(offset)..anchor.max(offset);
        self.reversed = offset < anchor;
        self.coalesce = None;
    }

    /// Replaces `range` with `text` as one edit of `kind` and leaves the cursor after it.
    /// Returns whether the text changed.
    pub fn replace(&mut self, range: Range<usize>, text: &str, kind: EditKind) -> bool {
        let range = range.start.min(self.text.len())..range.end.min(self.text.len());
        let joins = kind != EditKind::Other
            && self.coalesce == Some(kind)
            && self.selected.is_empty()
            && (range.end == self.cursor() || range.start == self.cursor());
        let before = self.composition_base.take().unwrap_or_else(|| self.snapshot());
        let mut next = String::with_capacity(self.text.len() + text.len());
        next.push_str(&self.text[..range.start]);
        next.push_str(text);
        next.push_str(&self.text[range.end..]);
        let changed = next != before.text;
        if changed && !joins {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        if changed {
            self.redo.clear();
        }
        self.text = next;
        let cursor = range.start + text.len();
        self.selected = cursor..cursor;
        self.reversed = false;
        self.coalesce = (kind != EditKind::Other).then_some(kind);
        changed
    }

    /// An input-method composition changed the text (shown, not recorded): undo goes back to
    /// the text before the composition began.
    pub fn compose(&mut self, text: String, selected: Range<usize>) {
        if self.composition_base.is_none() {
            self.composition_base = Some(self.snapshot());
        }
        self.text = text;
        self.selected = selected;
        self.reversed = false;
        self.coalesce = None;
    }

    /// The composition was abandoned without committing.
    pub fn end_composition(&mut self) {
        self.composition_base = None;
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else { return false };
        let current = self.snapshot();
        self.restore(previous);
        self.redo.push(current);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        let current = self.snapshot();
        self.restore(next);
        self.undo.push(current);
        true
    }

    // -----------------------------------------------------------------------------------------
    // Boundaries.

    /// The grapheme-cluster boundary before `offset`.
    pub fn previous_grapheme(&self, offset: usize) -> usize {
        let offset = self.snap(offset);
        self.text[..offset].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
    }

    /// The grapheme-cluster boundary after `offset`.
    pub fn next_grapheme(&self, offset: usize) -> usize {
        let offset = self.snap(offset);
        self.text[offset..].graphemes(true).next().map_or(self.text.len(), |g| offset + g.len())
    }

    /// The start of the word before `offset` (⌥←): skips separators, then the word.
    pub fn previous_word(&self, offset: usize) -> usize {
        self.words().take_while(|range| range.start < offset).last().map_or(0, |range| range.start)
    }

    /// The end of the word after `offset` (⌥→): skips separators, then the word.
    pub fn next_word(&self, offset: usize) -> usize {
        self.words().find(|range| range.end > offset).map_or(self.text.len(), |range| range.end)
    }

    /// The nearest grapheme-cluster boundary at or before `offset`.
    pub fn snap(&self, offset: usize) -> usize {
        let offset = offset.min(self.text.len());
        if offset == self.text.len() {
            return offset;
        }
        self.text.grapheme_indices(true).map(|(i, _)| i).take_while(|&i| i <= offset).last().unwrap_or(0)
    }

    fn words(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.text
            .split_word_bound_indices()
            .filter(|(_, segment)| segment.chars().any(char::is_alphanumeric))
            .map(|(start, segment)| start..start + segment.len())
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), selected: self.selected.clone(), reversed: self.reversed }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.selected = snapshot.selected;
        self.reversed = snapshot.reversed;
        self.coalesce = None;
        self.composition_base = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> TextEdit {
        let mut edit = TextEdit::default();
        for (i, c) in text.char_indices() {
            edit.replace(i..i, c.encode_utf8(&mut [0; 4]), EditKind::Typing);
        }
        edit
    }

    /// Cursor and word motion never split a grapheme cluster (combining marks, flags, ZWJ
    /// families) and treat non-ASCII letters as word characters.
    #[test]
    fn motion_steps_over_whole_clusters_and_words_in_non_ascii_text() {
        let family = "👨\u{200d}👩\u{200d}👧";
        let text = format!("Größe e\u{301}té {family}🇩🇪 日本語.txt");
        let mut edit = TextEdit::default();
        edit.reset(&text);
        let end = text.len();
        let flag_start = text.find("🇩🇪").unwrap();
        // Grapheme by grapheme from the end: every cluster stays whole.
        let mut offset = end;
        let mut clusters = Vec::new();
        while offset > 0 {
            let previous = edit.previous_grapheme(offset);
            clusters.push(&text[previous..offset]);
            offset = previous;
        }
        clusters.reverse();
        assert_eq!(clusters.concat(), text);
        assert!(clusters.contains(&family) && clusters.contains(&"🇩🇪") && clusters.contains(&"e\u{301}"));
        assert_eq!(edit.next_grapheme(flag_start), flag_start + "🇩🇪".len());
        // A position inside a cluster snaps to its start.
        assert_eq!(edit.snap(flag_start + 4), flag_start);

        // ⌥→ from the start: to the end of each word; ⌥← back: to the start of each word.
        let ends: Vec<&str> = std::iter::successors(Some(0), |&o| (o < end).then(|| edit.next_word(o)))
            .skip(1)
            .map(|o| &text[..o])
            .collect();
        assert_eq!(ends[0], "Größe");
        assert_eq!(ends[1], "Größe e\u{301}té");
        assert!(ends.last().is_some_and(|last| *last == text));
        let txt = text.rfind("txt").unwrap();
        assert_eq!(edit.previous_word(end), txt);
        assert_eq!(edit.previous_word(text.find("é ").unwrap() + 2), text.find("e\u{301}").unwrap());
        assert_eq!(edit.previous_word(5), 0);
    }

    /// Typing and deleting runs are one undo step each; ⌥⌫ deletes a word; a new edit drops the
    /// redo history; moving the cursor ends a typing run.
    #[test]
    fn undo_redo_restore_coalesced_runs_and_word_deletion() {
        let mut edit = typed("naïve café");
        assert_eq!(edit.text(), "naïve café");
        // ⌥⌫: delete the previous word.
        let cursor = edit.cursor();
        edit.replace(edit.previous_word(cursor)..cursor, "", EditKind::Deletion);
        assert_eq!(edit.text(), "naïve ");
        assert!(edit.undo());
        assert_eq!((edit.text(), edit.cursor()), ("naïve café", "naïve café".len()));
        assert!(edit.undo(), "the whole typing run is one step");
        assert_eq!(edit.text(), "");
        assert!(!edit.undo());
        assert!(edit.redo() && edit.redo());
        assert_eq!(edit.text(), "naïve ");
        assert!(!edit.redo());

        // Moving the cursor ends the run: typing after it is a separate step.
        edit.move_to(0);
        edit.replace(0..0, "ü", EditKind::Typing);
        edit.move_to(edit.text().len());
        edit.replace(edit.cursor()..edit.cursor(), "x", EditKind::Typing);
        assert_eq!(edit.text(), "ünaïve x");
        assert!(edit.undo());
        assert_eq!(edit.text(), "ünaïve ");
        assert!(edit.undo());
        assert_eq!(edit.text(), "naïve ");
        // A new edit after undo drops the redo history.
        edit.replace(0..0, "-", EditKind::Other);
        assert!(!edit.can_redo());

        // A composition is undone as a whole, back to the text before it began.
        let mut edit = typed("a");
        edit.compose("aに".into(), 2..2);
        edit.compose("aにほ".into(), 4..4);
        edit.replace(1..edit.text().len(), "日本", EditKind::Typing);
        assert_eq!(edit.text(), "a日本");
        assert!(edit.undo());
        assert_eq!(edit.text(), "a");
    }
}
