//! ⌥⌘← / ⌥⌘→: back and forward through the terminals you've shown in a window, like a
//! browser's history. Selecting a terminal records the one you left; going back or forward
//! moves along that list without recording, and skips terminals that are gone.

/// Most terminals remembered each way.
const LIMIT: usize = 100;

#[derive(Default)]
pub struct TabHistory {
    back: Vec<String>,
    forward: Vec<String>,
}

impl TabHistory {
    /// You went from `from` to another terminal yourself: `from` is where back goes, and
    /// forward starts over.
    pub fn left(&mut self, from: String) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
        }
        if self.back.len() > LIMIT {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// Back from `current`: the latest terminal behind it that `alive` still accepts.
    pub fn back(&mut self, current: Option<String>, alive: impl Fn(&str) -> bool) -> Option<String> {
        Self::step(&mut self.back, &mut self.forward, current, alive)
    }

    /// Forward from `current`, after going back.
    pub fn forward(&mut self, current: Option<String>, alive: impl Fn(&str) -> bool) -> Option<String> {
        Self::step(&mut self.forward, &mut self.back, current, alive)
    }

    fn step(from: &mut Vec<String>, to: &mut Vec<String>, current: Option<String>, alive: impl Fn(&str) -> bool) -> Option<String> {
        while let Some(id) = from.pop() {
            if alive(&id) && current.as_deref() != Some(&id) {
                to.extend(current);
                return Some(id);
            }
        }
        None
    }

    /// Terminal `old` was swapped for `new` (⌘R): keep its place in the history.
    pub fn replaced(&mut self, old: &str, new: &str) {
        for id in self.back.iter_mut().chain(self.forward.iter_mut()).filter(|id| *id == old) {
            *id = new.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Option<String> {
        Some(x.to_string())
    }

    #[test]
    fn back_and_forward() {
        let mut h = TabHistory::default();
        h.left("a".into());
        h.left("b".into());
        // on c, having come a → b → c
        assert_eq!(h.back(s("c"), |_| true), s("b"));
        assert_eq!(h.back(s("b"), |_| true), s("a"));
        assert_eq!(h.back(s("a"), |_| true), None);
        assert_eq!(h.forward(s("a"), |_| true), s("b"));
        assert_eq!(h.forward(s("b"), |_| true), s("c"));
        assert_eq!(h.forward(s("c"), |_| true), None);
    }

    #[test]
    fn selecting_clears_forward() {
        let mut h = TabHistory::default();
        h.left("a".into());
        assert_eq!(h.back(s("b"), |_| true), s("a"));
        h.left("a".into());
        assert_eq!(h.forward(s("c"), |_| true), None);
        assert_eq!(h.back(s("c"), |_| true), s("a"));
    }

    #[test]
    fn skips_closed_and_current() {
        let mut h = TabHistory::default();
        h.left("a".into());
        h.left("gone".into());
        h.left("c".into());
        assert_eq!(h.back(s("c"), |id| id != "gone"), s("a"));
        assert_eq!(h.forward(s("a"), |id| id != "gone"), s("c"));
    }

    #[test]
    fn replaced_keeps_place() {
        let mut h = TabHistory::default();
        h.left("a".into());
        h.replaced("a", "a2");
        assert_eq!(h.back(s("b"), |_| true), s("a2"));
    }
}
