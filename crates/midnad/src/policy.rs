//! Pure policy evaluation: glob matching, scope precedence and the defaults table.
//! Most specific scope wins (session > project > global); within a scope deny > ask > allow.
use midna_proto::*;

/// Glob over the whole string: `*` any run (including empty), `?` one char, `\x` literal x.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Iterative matcher with single-star backtracking.
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ti));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '\\' if pi + 1 < p.len() && p[pi + 1] == t[ti] => {
                    pi += 2;
                    ti += 1;
                    continue;
                }
                c if c != '\\' && c == t[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, st)) => {
                pi = sp + 1;
                ti = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Escape glob metacharacters so `value` matches only itself.
pub fn escape_glob(value: &str) -> String {
    let mut s = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '*' | '?' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

pub fn is_expired(r: &Rule, now: i64) -> bool {
    r.expires_at.as_deref().and_then(time::parse_rfc3339).is_some_and(|t| t <= now)
}

fn scope_applies(scope: &RuleScope, a: &PolicyAction) -> bool {
    match scope {
        RuleScope::Global => true,
        RuleScope::Project(p) => a.project.as_deref() == Some(p.as_str()),
        RuleScope::Session(s) => a.session.as_deref() == Some(s.as_str()),
    }
}

fn scope_label(s: &RuleScope) -> String {
    match s {
        RuleScope::Global => "global".into(),
        RuleScope::Project(p) => format!("project {p}"),
        RuleScope::Session(s) => format!("session {s}"),
    }
}

/// CLI verbs that ask by default (when `policy.default` is `auto`).
const DESTRUCTIVE_CLI: &[&str] = &["close --force*", "restart*", "replace*", "project remove*", "rules remove*", "settings reset*"];

/// Decision when no rule matches. `setting` is the `policy.default` value.
pub fn default_decision(a: &PolicyAction, setting: &str) -> Effect {
    match setting {
        "allow" => Effect::Allow,
        "ask" => Effect::Ask,
        "deny" => Effect::Deny,
        _ => {
            if a.kind == ActionKind::Cli && DESTRUCTIVE_CLI.iter().any(|p| glob_match(p, &a.value)) {
                Effect::Ask
            } else {
                Effect::Allow
            }
        }
    }
}

/// Within one scope: deny > an allow the human gave by approving this exact action ("always")
/// > ask > allow. Without the approval step, "approve always" on an `ask` rule would add an
/// > allow at the same scope that the ask rule then always beat, so the human would be asked
/// > again every time. A deny still wins.
fn effect_rank(r: &Rule) -> u8 {
    match r.effect {
        Effect::Deny => 3,
        Effect::Allow if r.origin.is_some() && r.added_by.kind == ActorKind::Human => 2,
        Effect::Ask => 1,
        Effect::Allow => 0,
    }
}

pub fn evaluate(rules: &[Rule], a: &PolicyAction, now: i64, default_setting: &str) -> CheckResult {
    let mut trace = Vec::with_capacity(rules.len());
    let mut best: Option<&Rule> = None;
    for r in rules {
        let (matched, reason) = if r.matcher.kind != a.kind {
            (false, format!("matches {:?} actions, not {:?}", r.matcher.kind, a.kind).to_lowercase())
        } else if is_expired(r, now) {
            (false, "expired".into())
        } else if !scope_applies(&r.scope, a) {
            (false, format!("{} scope does not apply", scope_label(&r.scope)))
        } else if !glob_match(&r.matcher.pattern, &a.value) {
            (false, format!("pattern `{}` does not match", r.matcher.pattern))
        } else {
            (true, format!("{} `{}` matches ({} scope)", r.effect.as_str(), r.matcher.pattern, scope_label(&r.scope)))
        };
        if matched {
            let better = match best {
                None => true,
                Some(b) => (r.scope.specificity(), effect_rank(r)) > (b.scope.specificity(), effect_rank(b)),
            };
            if better {
                best = Some(r);
            }
        }
        trace.push(TraceEntry { rule_id: r.id.clone(), matched, reason });
    }
    match best {
        Some(r) => {
            for t in trace.iter_mut().filter(|t| t.rule_id == r.id) {
                t.reason = format!("{} (winner)", t.reason);
            }
            CheckResult { decision: r.effect, rule: Some(r.clone()), trace, source: DecisionSource::Rule }
        }
        None => CheckResult { decision: default_decision(a, default_setting), rule: None, trace, source: DecisionSource::Default },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, effect: Effect, pattern: &str, scope: RuleScope) -> Rule {
        Rule {
            id: id.into(),
            effect,
            matcher: Matcher { kind: ActionKind::Command, pattern: pattern.into() },
            scope,
            expires_at: None,
            added_by: Actor::human(),
            added_at: time::now_rfc3339(),
            origin: None,
            fired: 0,
            last_fired_at: None,
            removal_request: None,
        }
    }

    #[test]
    fn globs() {
        assert!(glob_match("git push --force*", "git push --force origin main"));
        assert!(!glob_match("git push --force*", "git push origin"));
        assert!(glob_match("Bash(rm -rf*)", "Bash(rm -rf /tmp/x)"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("*", ""));
        assert!(glob_match(&escape_glob("ls *.rs"), "ls *.rs"));
        assert!(!glob_match(&escape_glob("ls *.rs"), "ls a.rs"));
    }

    #[test]
    fn precedence() {
        let a = PolicyAction { kind: ActionKind::Command, value: "git push --force".into(), session: Some("s1".into()), project: Some("p_1".into()) };
        let rules = vec![
            rule("r_g_deny", Effect::Deny, "git push*", RuleScope::Global),
            rule("r_p_allow", Effect::Allow, "git push*", RuleScope::Project("p_1".into())),
            rule("r_p_ask", Effect::Ask, "git push --force*", RuleScope::Project("p_1".into())),
        ];
        // project beats global; within project ask beats allow
        let r = evaluate(&rules, &a, time::now_unix(), "auto");
        assert_eq!((r.decision, r.rule.unwrap().id.as_str()), (Effect::Ask, "r_p_ask"));
        // session beats project
        let mut rules2 = rules.clone();
        rules2.push(rule("r_s_allow", Effect::Allow, "*", RuleScope::Session("s1".into())));
        assert_eq!(evaluate(&rules2, &a, time::now_unix(), "auto").decision, Effect::Allow);
        // other project: only global applies
        let b = PolicyAction { project: Some("p_2".into()), session: None, ..a.clone() };
        assert_eq!(evaluate(&rules, &b, time::now_unix(), "auto").decision, Effect::Deny);
        // no match -> default
        let c = PolicyAction { value: "ls".into(), ..a };
        let r = evaluate(&rules, &c, time::now_unix(), "auto");
        assert_eq!((r.decision, r.source), (Effect::Allow, DecisionSource::Default));
        assert_eq!(r.trace.len(), 3);
    }

    #[test]
    fn approved_always_beats_the_ask_it_answered_but_not_a_deny() {
        let a = PolicyAction { kind: ActionKind::Command, value: "git push origin feature".into(), session: None, project: Some("p_1".into()) };
        let mut approved = rule("r_ok", Effect::Allow, "git push origin feature", RuleScope::Project("p_1".into()));
        approved.origin = Some(RuleOrigin { needs_you_id: "n_1".into(), approval_scope: ApprovalScope::Always });
        let mut rules = vec![rule("r_ask", Effect::Ask, "git push*", RuleScope::Project("p_1".into())), approved.clone()];
        assert_eq!(evaluate(&rules, &a, time::now_unix(), "auto").rule.unwrap().id, "r_ok");
        // An agent-added allow (no approval) still loses to the ask.
        let mut agent_allow = approved.clone();
        agent_allow.id = "r_agent".into();
        agent_allow.origin = None;
        agent_allow.added_by = Actor::agent(None);
        assert_eq!(evaluate(&[rules[0].clone(), agent_allow], &a, time::now_unix(), "auto").decision, Effect::Ask);
        rules.push(rule("r_deny", Effect::Deny, "git push*", RuleScope::Project("p_1".into())));
        assert_eq!(evaluate(&rules, &a, time::now_unix(), "auto").decision, Effect::Deny);
    }
}
