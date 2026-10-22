//! Parsing of version-control conflict files (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`).

pub struct ConflictParts {
    pub mine: Vec<String>,
    pub base: Vec<String>,
    pub theirs: Vec<String>,
    pub has_base: bool,
    pub conflicts: usize,
}

#[derive(PartialEq)]
enum State {
    Common,
    Mine,
    Base,
    Theirs,
}

/// Splits a conflict file into "mine", "base" and "theirs" versions.
/// When the file has no base sections (diff2 style), the base contains only the common lines,
/// which makes every conflict show up as a conflicting change in a 3-way compare.
pub fn parse(lines: &[String]) -> Option<ConflictParts> {
    let mut p = ConflictParts { mine: vec![], base: vec![], theirs: vec![], has_base: false, conflicts: 0 };
    let mut state = State::Common;
    for l in lines {
        if l.starts_with("<<<<<<<") && state == State::Common {
            state = State::Mine;
            p.conflicts += 1;
        } else if l.starts_with("|||||||") && state == State::Mine {
            state = State::Base;
            p.has_base = true;
        } else if l.starts_with("=======") && (state == State::Mine || state == State::Base) {
            state = State::Theirs;
        } else if l.starts_with(">>>>>>>") && state == State::Theirs {
            state = State::Common;
        } else {
            match state {
                State::Common => {
                    p.mine.push(l.clone());
                    p.base.push(l.clone());
                    p.theirs.push(l.clone());
                }
                State::Mine => p.mine.push(l.clone()),
                State::Base => p.base.push(l.clone()),
                State::Theirs => p.theirs.push(l.clone()),
            }
        }
    }
    if p.conflicts == 0 || state != State::Common {
        return None;
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_conflict() {
        let text = "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> branch\nz";
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let p = parse(&lines).unwrap();
        assert_eq!(p.mine, vec!["a", "mine", "z"]);
        assert_eq!(p.theirs, vec!["a", "theirs", "z"]);
        assert_eq!(p.base, vec!["a", "z"]);
        assert_eq!(p.conflicts, 1);
    }
}
