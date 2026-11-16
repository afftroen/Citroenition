//! Row selection for the profile table: click / ctrl+click / shift+click ranges, select-all, arrow keys.
//! Pure logic over the *visible* id order, so it is unit-tested without a UI.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    ids: HashSet<String>,
    /// Where a shift-range starts.
    anchor: Option<String>,
    /// The row the keyboard moves from (the last one clicked).
    cursor: Option<String>,
}

impl Selection {
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
        self.cursor = None;
    }

    /// Selected ids in display order.
    pub fn ordered(&self, visible: &[String]) -> Vec<String> {
        visible.iter().filter(|id| self.ids.contains(*id)).cloned().collect()
    }

    /// Drop selected ids that are not in `visible` (filtered out or deleted) so bulk actions never touch hidden rows.
    pub fn retain_visible(&mut self, visible: &[String]) {
        if self.ids.is_empty() {
            return;
        }
        let vis: HashSet<&str> = visible.iter().map(String::as_str).collect();
        self.ids.retain(|id| vis.contains(id.as_str()));
        if self.anchor.as_deref().is_some_and(|a| !vis.contains(a)) {
            self.anchor = None;
        }
        if self.cursor.as_deref().is_some_and(|c| !vis.contains(c)) {
            self.cursor = None;
        }
    }

    /// A click on row `idx` of `visible`.
    pub fn click(&mut self, visible: &[String], idx: usize, mods: Mods) {
        let Some(id) = visible.get(idx) else { return };
        match (mods.shift, mods.ctrl) {
            (true, ctrl) => {
                let anchor_idx = self.anchor.as_ref().and_then(|a| visible.iter().position(|v| v == a));
                let Some(a) = anchor_idx else {
                    self.ids.clear();
                    self.ids.insert(id.clone());
                    self.anchor = Some(id.clone());
                    self.cursor = Some(id.clone());
                    return;
                };
                if !ctrl {
                    self.ids.clear();
                }
                let (lo, hi) = if a <= idx { (a, idx) } else { (idx, a) };
                self.ids.extend(visible[lo..=hi].iter().cloned());
                self.cursor = Some(id.clone());
            }
            (false, true) => {
                self.toggle(id);
                self.anchor = Some(id.clone());
                self.cursor = Some(id.clone());
            }
            (false, false) => {
                self.ids.clear();
                self.ids.insert(id.clone());
                self.anchor = Some(id.clone());
                self.cursor = Some(id.clone());
            }
        }
    }

    /// The row checkbox: toggles one row without touching the others.
    pub fn toggle(&mut self, id: &str) {
        if !self.ids.remove(id) {
            self.ids.insert(id.to_owned());
        }
        self.anchor = Some(id.to_owned());
        self.cursor = Some(id.to_owned());
    }

    pub fn select_all(&mut self, visible: &[String]) {
        self.ids = visible.iter().cloned().collect();
        self.anchor = visible.first().cloned();
        self.cursor = visible.last().cloned();
    }

    /// True when every visible row is selected (and there is at least one).
    pub fn all_selected(&self, visible: &[String]) -> bool {
        !visible.is_empty() && visible.iter().all(|id| self.ids.contains(id))
    }

    /// The header checkbox.
    pub fn toggle_all(&mut self, visible: &[String]) {
        if self.all_selected(visible) {
            self.clear();
        } else {
            self.select_all(visible);
        }
    }

    /// Up/Down: move the cursor by `delta` rows. `extend` (shift) grows the range from the anchor.
    pub fn move_cursor(&mut self, visible: &[String], delta: isize, extend: bool) {
        if visible.is_empty() {
            return;
        }
        let cur = self.cursor.as_ref().and_then(|c| visible.iter().position(|v| v == c));
        let next = match cur {
            Some(i) => (i as isize + delta).clamp(0, visible.len() as isize - 1) as usize,
            None if delta >= 0 => 0,
            None => visible.len() - 1,
        };
        if extend {
            let a = self.anchor.as_ref().and_then(|a| visible.iter().position(|v| v == a)).unwrap_or(next);
            let (lo, hi) = if a <= next { (a, next) } else { (next, a) };
            self.ids = visible[lo..=hi].iter().cloned().collect();
            self.cursor = Some(visible[next].clone());
        } else {
            self.ids.clear();
            self.ids.insert(visible[next].clone());
            self.anchor = Some(visible[next].clone());
            self.cursor = Some(visible[next].clone());
        }
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("p{i}")).collect()
    }

    const PLAIN: Mods = Mods { ctrl: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, shift: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true };
    const BOTH: Mods = Mods { ctrl: true, shift: true };

    fn sel(s: &Selection, v: &[String]) -> Vec<String> {
        s.ordered(v)
    }

    #[test]
    fn plain_click_replaces_selection() {
        let v = ids(6);
        let mut s = Selection::default();
        s.click(&v, 2, PLAIN);
        s.click(&v, 4, PLAIN);
        assert_eq!(sel(&s, &v), ["p4"]);
        assert_eq!(s.cursor(), Some("p4"));
    }

    #[test]
    fn ctrl_click_toggles() {
        let v = ids(6);
        let mut s = Selection::default();
        s.click(&v, 1, PLAIN);
        s.click(&v, 3, CTRL);
        s.click(&v, 5, CTRL);
        assert_eq!(sel(&s, &v), ["p1", "p3", "p5"]);
        s.click(&v, 3, CTRL);
        assert_eq!(sel(&s, &v), ["p1", "p5"]);
    }

    #[test]
    fn shift_click_selects_a_range_in_either_direction() {
        let v = ids(8);
        let mut s = Selection::default();
        s.click(&v, 2, PLAIN);
        s.click(&v, 5, SHIFT);
        assert_eq!(sel(&s, &v), ["p2", "p3", "p4", "p5"]);
        // the anchor stays at p2: shifting upwards re-spans from it
        s.click(&v, 0, SHIFT);
        assert_eq!(sel(&s, &v), ["p0", "p1", "p2"]);
    }

    #[test]
    fn ctrl_shift_adds_a_range_to_the_existing_selection() {
        let v = ids(10);
        let mut s = Selection::default();
        s.click(&v, 0, PLAIN);
        s.click(&v, 6, CTRL); // anchor = p6
        s.click(&v, 8, BOTH);
        assert_eq!(sel(&s, &v), ["p0", "p6", "p7", "p8"]);
    }

    #[test]
    fn shift_without_anchor_behaves_like_a_click() {
        let v = ids(4);
        let mut s = Selection::default();
        s.click(&v, 2, SHIFT);
        assert_eq!(sel(&s, &v), ["p2"]);
    }

    #[test]
    fn header_checkbox_toggles_all_visible() {
        let v = ids(4);
        let mut s = Selection::default();
        assert!(!s.all_selected(&v));
        s.toggle_all(&v);
        assert!(s.all_selected(&v));
        assert_eq!(s.len(), 4);
        s.toggle_all(&v);
        assert!(s.is_empty());
        assert!(!s.all_selected(&[]));
    }

    #[test]
    fn retain_visible_drops_filtered_out_rows() {
        let v = ids(6);
        let mut s = Selection::default();
        s.select_all(&v);
        let filtered: Vec<String> = vec!["p1".into(), "p4".into()];
        s.retain_visible(&filtered);
        assert_eq!(sel(&s, &filtered), ["p1", "p4"]);
        assert_eq!(s.len(), 2);
        s.retain_visible(&[]);
        assert!(s.is_empty());
    }

    #[test]
    fn arrows_move_and_extend() {
        let v = ids(5);
        let mut s = Selection::default();
        s.move_cursor(&v, 1, false); // nothing selected: first row
        assert_eq!(sel(&s, &v), ["p0"]);
        s.move_cursor(&v, 1, false);
        s.move_cursor(&v, 1, false);
        assert_eq!(sel(&s, &v), ["p2"]);
        s.move_cursor(&v, 1, true);
        s.move_cursor(&v, 1, true);
        assert_eq!(sel(&s, &v), ["p2", "p3", "p4"]);
        s.move_cursor(&v, 1, true); // clamped at the end
        assert_eq!(sel(&s, &v), ["p2", "p3", "p4"]);
        s.move_cursor(&v, -10, false);
        assert_eq!(sel(&s, &v), ["p0"]);
    }

    #[test]
    fn toggle_sets_anchor_for_following_shift_click() {
        let v = ids(6);
        let mut s = Selection::default();
        s.toggle("p1");
        s.click(&v, 3, SHIFT);
        assert_eq!(sel(&s, &v), ["p1", "p2", "p3"]);
    }
}
