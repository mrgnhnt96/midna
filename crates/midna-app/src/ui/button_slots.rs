//! How a `notify.send` notification's buttons fit a card: like ones fold into a split button
//! (its face the first of them, its menu the rest), and past `SLOTS` the rest go under More.
//!
//! Buttons are like when they share a group: the text before `": "` when a label has one (the
//! sender grouping them, `Later: 1 hour`), else its first word (`Snooze 15 min`), either way
//! ignoring case. Only the look changes: a pick still reports its full label.

/// Places in a card's row of buttons, More included.
pub const SLOTS: usize = 3;

/// One place in the row. Indices are into the notification's actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    One(usize),
    /// A split button: the first is its face; the menu lists every one, by `names`.
    Group { members: Vec<usize>, names: Vec<String> },
    /// What didn't fit, by full label.
    More(Vec<usize>),
}

impl Slot {
    /// The actions it holds.
    pub fn members(&self) -> Vec<usize> {
        match self {
            Slot::One(i) => vec![*i],
            Slot::Group { members, .. } | Slot::More(members) => members.clone(),
        }
    }
}

/// A label's group and the rest of it (empty when the label is just the group).
fn split(label: &str) -> (String, &str) {
    let (group, rest) = match label.split_once(": ") {
        Some((g, r)) if !g.trim().is_empty() => (g.trim(), r.trim()),
        _ => label.split_once(char::is_whitespace).map_or((label, ""), |(g, r)| (g, r.trim())),
    };
    (group.to_lowercase(), rest)
}

/// Lay out `actions` as the card's row.
pub fn slots(actions: &[String]) -> Vec<Slot> {
    let mut groups: Vec<(String, Vec<usize>)> = vec![];
    for (i, a) in actions.iter().enumerate() {
        let (key, _) = split(a);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, m)) => m.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    let mut out: Vec<Slot> = groups
        .into_iter()
        .map(|(_, members)| {
            if members.len() == 1 {
                return Slot::One(members[0]);
            }
            let rests: Vec<&str> = members.iter().map(|&i| split(&actions[i]).1).collect();
            // Each by what follows the group, unless one would be blank.
            let names = if rests.iter().all(|r| !r.is_empty()) { rests.iter().map(|r| r.to_string()).collect() } else { members.iter().map(|&i| actions[i].clone()).collect() };
            Slot::Group { members, names }
        })
        .collect();
    if out.len() > SLOTS {
        let rest = out.split_off(SLOTS - 1);
        out.push(Slot::More(rest.iter().flat_map(Slot::members).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(labels: &[&str]) -> Vec<Slot> {
        slots(&labels.iter().map(|l| l.to_string()).collect::<Vec<_>>())
    }

    fn group(members: &[usize], names: &[&str]) -> Slot {
        Slot::Group { members: members.to_vec(), names: names.iter().map(|n| n.to_string()).collect() }
    }

    #[test]
    fn like_buttons_fold_by_first_word() {
        assert_eq!(of(&["Snooze 15 min", "Snooze 30 min", "snooze 1 hour"]), vec![group(&[0, 1, 2], &["15 min", "30 min", "1 hour"])]);
    }

    #[test]
    fn unlike_ones_stay_apart() {
        assert_eq!(of(&["Approve", "Skip"]), vec![Slot::One(0), Slot::One(1)]);
    }

    #[test]
    fn a_group_sits_where_its_first_member_was() {
        assert_eq!(of(&["Snooze 15 min", "Merge", "Snooze 1 hour"]), vec![group(&[0, 2], &["15 min", "1 hour"]), Slot::One(1)]);
        assert_eq!(of(&["Merge", "Snooze 15 min", "Snooze 1 hour"]), vec![Slot::One(0), group(&[1, 2], &["15 min", "1 hour"])]);
    }

    #[test]
    fn the_sender_groups_with_a_colon() {
        assert_eq!(of(&["Later: 1 hour", "Later: tomorrow 9am", "Done"]), vec![group(&[0, 1], &["1 hour", "tomorrow 9am"]), Slot::One(2)]);
        // A colon with no group before it doesn't make one.
        assert_eq!(split(": odd"), (":".to_string(), "odd"));
    }

    #[test]
    fn a_label_that_is_just_the_group_keeps_full_names() {
        assert_eq!(of(&["Approve", "Approve all"]), vec![group(&[0, 1], &["Approve", "Approve all"])]);
    }

    #[test]
    fn past_the_slots_the_rest_go_under_more() {
        assert_eq!(of(&["A", "B", "C"]), vec![Slot::One(0), Slot::One(1), Slot::One(2)]);
        assert_eq!(of(&["A", "B", "C", "D"]), vec![Slot::One(0), Slot::One(1), Slot::More(vec![2, 3])]);
        // A group past them goes under More whole, each by its full label.
        assert_eq!(of(&["A", "B", "Snooze 1", "Snooze 2"]), vec![Slot::One(0), Slot::One(1), group(&[2, 3], &["1", "2"])]);
        assert_eq!(of(&["A", "B", "C", "Snooze 1", "Snooze 2"]), vec![Slot::One(0), Slot::One(1), Slot::More(vec![2, 3, 4])]);
        assert_eq!(of(&["Snooze 1", "Snooze 2", "B", "C", "D"]), vec![group(&[0, 1], &["1", "2"]), Slot::One(2), Slot::More(vec![3, 4])]);
    }
}
