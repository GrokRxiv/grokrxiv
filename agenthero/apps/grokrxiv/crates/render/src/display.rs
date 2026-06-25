use grokrxiv_schemas::{MetaReview, PaperExtract};
use once_cell::sync::Lazy;
use regex::Regex;

static LAYOUT_COMMAND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\\(?:vspace|hspace|kern|mkern|mskip|hskip|vskip|raisebox|phantom|hphantom|vphantom)\*?(?:\s*\[[^\]]*\])?(?:\s*\{[^{}\n]*\}){1,2}",
    )
    .expect("layout command regex compiles")
});
static MULTISPACE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[ \t]{2,}").expect("multispace regex compiles"));
static TEX_EXPR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\\[A-Za-z]+(?:\*|\{[^{}\n]*\}|\[[^\]\n]*\]|[_^][A-Za-z0-9{}\\]+|\([A-Za-z0-9{}\\_^,+\-*/=. ]+\)|[A-Za-z0-9{}\\_^,+\-*/=.])*",
    )
    .expect("tex expression regex compiles")
});
static ALGEBRA_EXPR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"[A-Za-z][A-Za-z0-9]*(?:[_^](?:\{[^{}\s]+\}|\([A-Za-z0-9_^\{\}\\,+\-=.*()]+\)|[A-Za-z0-9]+)|\([A-Za-z0-9_^\{\}\\,+\-=.*()]+\)|[A-Za-z0-9])*(?:=[A-Za-z0-9_^\{\}\\,+\-*/=.()]+)?",
    )
    .expect("algebra expression regex compiles")
});

#[derive(Debug, Clone)]
struct Candidate {
    start: usize,
    end: usize,
}

pub(crate) fn display_paper(paper: &PaperExtract) -> PaperExtract {
    let mut paper = paper.clone();
    paper.title = display_text(&paper.title);
    paper
}

pub(crate) fn display_meta(meta: &MetaReview) -> MetaReview {
    let mut meta = meta.clone();
    meta.summary = display_text(&meta.summary);
    meta.strengths = meta.strengths.iter().map(|s| display_text(s)).collect();
    meta.weaknesses = meta.weaknesses.iter().map(|s| display_text(s)).collect();
    meta.questions = meta.questions.iter().map(|s| display_text(s)).collect();
    for target in &mut meta.revision_targets {
        target.locator = target.locator.as_deref().map(display_text);
        target.evidence = target.evidence.as_deref().map(display_text);
        target.required_update = display_text(&target.required_update);
        target.verification_check = display_text(&target.verification_check);
    }
    meta
}

pub fn display_text(input: &str) -> String {
    let stripped = strip_layout_commands(input);
    transform_protected_spans(&stripped, wrap_math_candidates)
        .trim()
        .to_string()
}

fn strip_layout_commands(input: &str) -> String {
    let stripped = LAYOUT_COMMAND_RE.replace_all(input, " ");
    MULTISPACE_RE.replace_all(&stripped, " ").to_string()
}

fn transform_protected_spans(input: &str, transform: fn(&str) -> String) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let rest = &input[cursor..];
        let protected_end = if rest.starts_with('`') {
            find_after(rest, "`").map(|n| cursor + n)
        } else if rest.starts_with('$') {
            find_after(rest, "$").map(|n| cursor + n)
        } else if rest.starts_with(r"\(") {
            find_after(rest, r"\)").map(|n| cursor + n)
        } else if rest.starts_with(r"\[") {
            find_after(rest, r"\]").map(|n| cursor + n)
        } else {
            None
        };
        if let Some(end) = protected_end {
            out.push_str(&input[cursor..end]);
            cursor = end;
            continue;
        }
        let next = next_protected_start(input, cursor).unwrap_or(input.len());
        out.push_str(&transform(&input[cursor..next]));
        cursor = next;
    }
    out
}

fn find_after(rest: &str, delimiter: &str) -> Option<usize> {
    rest[delimiter.len()..]
        .find(delimiter)
        .map(|idx| delimiter.len() + idx + delimiter.len())
}

fn next_protected_start(input: &str, cursor: usize) -> Option<usize> {
    let rest = &input[cursor..];
    ["`", "$", r"\(", r"\["]
        .iter()
        .filter_map(|needle| rest.find(needle).map(|idx| cursor + idx))
        .min()
}

fn wrap_math_candidates(segment: &str) -> String {
    let mut candidates = Vec::new();
    collect_candidates(&TEX_EXPR_RE, segment, &mut candidates);
    collect_candidates(&ALGEBRA_EXPR_RE, segment, &mut candidates);
    candidates.sort_by_key(|c| (c.start, std::cmp::Reverse(c.end - c.start)));

    let mut out = String::with_capacity(segment.len() + candidates.len() * 2);
    let mut cursor = 0;
    for candidate in candidates {
        if candidate.start < cursor {
            continue;
        }
        let end = trim_trailing_sentence_punctuation(segment, candidate.start, candidate.end);
        let expr = &segment[candidate.start..end];
        if !is_wrappable_math(segment, candidate.start, end, expr) {
            continue;
        }
        out.push_str(&segment[cursor..candidate.start]);
        out.push('$');
        out.push_str(expr);
        out.push('$');
        cursor = end;
    }
    out.push_str(&segment[cursor..]);
    out
}

fn trim_trailing_sentence_punctuation(segment: &str, start: usize, mut end: usize) -> usize {
    while end > start {
        let Some(ch) = segment[start..end].chars().next_back() else {
            break;
        };
        if !matches!(ch, '.' | ',' | ';' | ':') {
            break;
        }
        end -= ch.len_utf8();
    }
    end
}

fn collect_candidates(re: &Regex, segment: &str, out: &mut Vec<Candidate>) {
    for m in re.find_iter(segment) {
        out.push(Candidate {
            start: m.start(),
            end: m.end(),
        });
    }
}

fn is_wrappable_math(segment: &str, start: usize, end: usize, expr: &str) -> bool {
    if expr.len() < 3 || expr.len() > 180 {
        return false;
    }
    if expr.contains('$') || expr.contains("://") || expr.contains('@') {
        return false;
    }
    if is_plain_snake_case_identifier(expr) {
        return false;
    }
    let prev = segment[..start].chars().next_back();
    let next = segment[end..].chars().next();
    if matches!(prev, Some('/' | '`' | '$' | '#' | '&')) || matches!(next, Some('/' | '`' | '$')) {
        return false;
    }
    expr.starts_with('\\') || expr.contains('_') || expr.contains('^')
}

fn is_plain_snake_case_identifier(expr: &str) -> bool {
    if expr.contains('\\') || expr.contains('^') || expr.contains('{') || expr.contains('}') {
        return false;
    }
    if expr
        .chars()
        .any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        return false;
    }
    let Some((base, rest)) = expr.split_once('_') else {
        return false;
    };
    if base.chars().count() <= 1 {
        return false;
    }
    rest.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')
}
