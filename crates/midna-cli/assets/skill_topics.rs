// The guide's topics, in the order `SKILL.md` lists them. midna-cli and midnad both include this
// file, because midnad can't depend on midna-cli and both have to serve the same set.
pub const SKILL_TOPICS: &[(&str, &str)] = &[
    ("triggers", include_str!("skill-triggers.md")),
    ("keep-awake", include_str!("skill-keep-awake.md")),
    ("load", include_str!("skill-load.md")),
    ("secrets", include_str!("skill-secrets.md")),
    ("queue", include_str!("skill-queue.md")),
    ("terminals", include_str!("skill-terminals.md")),
    ("links", include_str!("skill-links.md")),
];

// What a topic's file is called, in the assets and beside `$MIDNA_SKILL`.
pub fn skill_topic_file(name: &str) -> String {
    format!("skill-{name}.md")
}
