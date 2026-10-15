//! Turning recognized LaTeX into the text that gets pasted.

use pulldown_latex::{Parser, RenderConfig, Storage, config::DisplayMode};

/// Fill a format template.
/// `{latex}` raw · `{latex_str}` escaped for "…" literals · `{latex_sq}` with ' doubled (MATLAB) · `{mathml}`
pub fn render(template: &str, latex: &str) -> String {
    let mut out = template.to_string();
    if out.contains("{mathml}") {
        out = out.replace("{mathml}", &to_mathml(latex));
    }
    if out.contains("{latex_clean}") {
        out = out.replace("{latex_clean}", &tex_clean(latex));
    }
    if out.contains("{latex_wl") {
        let wl = wolfram_tex(latex);
        out = out.replace("{latex_wl_str}", &escape_str(&wl)).replace("{latex_wl}", &wl);
    }
    out.replace("{latex_str}", &escape_str(latex))
        .replace("{latex_sq}", &latex.replace('\'', "''"))
        .replace("{latex}", latex)
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// LaTeX that Mathematica's TeX importer accepts. OCR output often wraps lines in an `array`
/// environment, writes words as `\mathrm{~Take~}`, and writes differentials as a bare `d x`,
/// which Mathematica reads as a symbol d (integrals need `\,d x`).
pub fn wolfram_tex(latex: &str) -> String {
    implicit_products(&function_args(&euler(&tex_clean(latex))))
}

/// Structural cleanup only, for typesetting (Mathematica's Inline TeX Input): unwrap `array`
/// environments, turn `\mathrm{~word~}` into `\text{ word }`, and mark integral differentials
/// with `\,` so they become ⅆ. The math itself is left as written (e stays e).
pub fn tex_clean(latex: &str) -> String {
    let mut s = latex.trim().to_string();
    // Unwrap single-formula environments; row breaks become wide spaces.
    for env in ["array", "aligned", "align*", "align", "gathered", "split"] {
        let begin = format!("\\begin{{{env}}}");
        let close = format!("\\end{{{env}}}");
        while let Some(i) = s.find(&begin) {
            let mut end = i + begin.len();
            if s[end..].starts_with('{') {
                // column spec such as {r} or {lcl}
                if let Some(c) = s[end..].find('}') {
                    end += c + 1;
                }
            }
            s.replace_range(i..end, "");
            if let Some(j) = s.find(&close) {
                s.replace_range(j..j + close.len(), "");
            }
            s = s.replace("\\\\", " \\quad ").replace('&', "");
        }
    }
    // {\mathrm{~Take~}} → {\text{ Take }}
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(i) = rest.find("\\mathrm{") {
        let body = i + "\\mathrm{".len();
        let Some(len) = rest[body..].find('}') else { break };
        out.push_str(&rest[..i]);
        let text = &rest[body..body + len];
        if text.contains('~') {
            out.push_str(&format!("\\text{{{}}}", text.replace('~', " ")));
        } else {
            out.push_str(&rest[i..body + len + 1]);
        }
        rest = &rest[body + len + 1..];
    }
    out.push_str(rest);
    if out.contains("\\int") { differentials(&out) } else { out }
}

/// `e^{…}` → `\exp\left(…\right)`. Mathematica's TeX reader treats a bare e as a variable
/// (so `e^{-x}` would evaluate wrongly); `\exp` is the only ASCII spelling it reads as E.
/// (Unicode ⅇ would work, but Inline TeX Input garbles non-ASCII pastes.)
fn euler(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 16);
    let mut i = 0;
    while i < c.len() {
        let prev = if i > 0 { c[i - 1] } else { ' ' };
        if c[i] == 'e' && c.get(i + 1) == Some(&'^') && !prev.is_alphanumeric() && prev != '\\' {
            let (arg, end) = atom(&c, i + 2);
            if !arg.is_empty() {
                out.push_str(&format!("\\exp\\left({arg}\\right)"));
                i = end;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `\sin x` → `\sin\left(x\right)`: Mathematica only reads functions applied with brackets.
fn function_args(s: &str) -> String {
    const FUNCS: [&str; 17] = [
        "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh", "sin", "cos", "tan", "sec", "csc", "cot", "log", "ln", "exp", "lg", "det",
    ];
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 16);
    let mut i = 0;
    'outer: while i < c.len() {
        if c[i] == '\\' {
            for f in FUNCS {
                let name_end = i + 1 + f.len();
                if !s_at(&c, i + 1, f) || c.get(name_end).is_some_and(|ch| ch.is_ascii_alphabetic()) {
                    continue;
                }
                let mut j = name_end;
                // optional power, e.g. \sin^{2}
                let mut power = String::new();
                if c.get(j) == Some(&'^') {
                    let (p, end) = atom(&c, j + 1);
                    power = format!("^{{{p}}}");
                    j = end;
                }
                while c.get(j) == Some(&' ') {
                    j += 1;
                }
                if c.get(j) == Some(&'(') || s_at(&c, j, "\\left") || j >= c.len() {
                    break; // already bracketed (or nothing follows)
                }
                let (arg, end) = atom(&c, j);
                if arg.is_empty() {
                    break;
                }
                out.push_str(&format!("\\{f}{power}\\left({arg}\\right)"));
                i = end;
                continue 'outer;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// One argument atom starting at `i`: a `{…}` group, a `\command`, or a run like `2x`. Returns (text, end).
fn atom(c: &[char], i: usize) -> (String, usize) {
    match c.get(i) {
        Some('{') => {
            let mut depth = 0;
            let mut j = i;
            while j < c.len() {
                match c[j] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return (c[i + 1..j].iter().collect(), j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            (c[i..].iter().collect(), c.len())
        }
        Some('\\') => {
            let mut j = i + 1;
            while c.get(j).is_some_and(|ch| ch.is_ascii_alphabetic()) {
                j += 1;
            }
            (c[i..j].iter().collect(), j)
        }
        Some(ch) if ch.is_ascii_alphanumeric() => {
            let mut j = i;
            while c.get(j).is_some_and(|ch| ch.is_ascii_digit()) {
                j += 1;
            }
            if c.get(j).is_some_and(|ch| ch.is_ascii_alphabetic()) {
                j += 1;
            }
            (c[i..j.max(i + 1)].iter().collect(), j.max(i + 1))
        }
        _ => (String::new(), i),
    }
}

/// A standalone `d x` / `dx` (not part of a word or command) becomes `\,d x`, so Mathematica
/// recognizes the differential of an integral.
fn differentials(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < c.len() {
        let prev = if i > 0 { c[i - 1] } else { ' ' };
        if c[i] == 'd' && !prev.is_alphanumeric() && prev != '\\' {
            let mut j = i + 1;
            while c.get(j) == Some(&' ') {
                j += 1;
            }
            let next = c.get(j + 1).copied().unwrap_or(' ');
            if let Some(v) = c.get(j).copied().filter(|v| v.is_ascii_alphabetic() && !next.is_ascii_alphabetic()) {
                if !out.trim_end().ends_with("\\,") {
                    out.push_str("\\,");
                }
                out.push_str("d ");
                out.push(v);
                i = j + 1;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// Mathematica's TeX reader joins space-separated letters into one name (`r x` → `rx`), so
/// spaces between plain identifiers become `\,` (explicit multiplication). Spaces after a
/// command (`\sin x`) and inside `\text{…}` are left alone.
fn implicit_products(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 16);
    let mut text_depth: Option<i32> = None; // brace depth inside \text{…}
    let mut i = 0;
    while i < c.len() {
        if text_depth.is_none() && s_at(&c, i, "\\text{") {
            text_depth = Some(0);
            out.push_str("\\text{");
            i += 6;
            continue;
        }
        if let Some(d) = text_depth.as_mut() {
            match c[i] {
                '{' => *d += 1,
                '}' if *d == 0 => text_depth = None,
                '}' => *d -= 1,
                _ => {}
            }
            out.push(c[i]);
            i += 1;
            continue;
        }
        if c[i] == ' ' {
            let mut j = i;
            while c.get(j) == Some(&' ') {
                j += 1;
            }
            let prev = if i > 0 { c[i - 1] } else { ' ' };
            let next = c.get(j).copied().unwrap_or(' ');
            let after_command = {
                let mut k = i;
                while k > 0 && c[k - 1].is_ascii_alphabetic() {
                    k -= 1;
                }
                k < i && k > 0 && c[k - 1] == '\\'
            };
            if (prev.is_alphanumeric() || prev == '}' || prev == ')') && next.is_alphabetic() && !after_command {
                out.push_str("\\,");
                i = j;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

fn s_at(c: &[char], i: usize, pat: &str) -> bool {
    pat.chars().enumerate().all(|(k, p)| c.get(i + k) == Some(&p))
}

/// Presentation MathML. Word, PowerPoint and OneNote turn pasted MathML text into a native equation.
pub fn to_mathml(latex: &str) -> String {
    let storage = Storage::new();
    let parser = Parser::new(latex, &storage);
    let config = RenderConfig {
        display_mode: DisplayMode::Block,
        ..Default::default()
    };
    let mut mathml = String::new();
    match pulldown_latex::push_mathml(&mut mathml, parser, config) {
        Ok(()) if !mathml.contains("merror") => {
            // Office only recognizes the namespaced root element.
            mathml.replacen("<math", "<math xmlns=\"http://www.w3.org/1998/Math/MathML\"", 1)
        }
        _ => {
            log::warn!("MathML conversion failed for {latex:?}; pasting LaTeX");
            latex.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        assert_eq!(render("{latex}", r"\frac{a}{b}"), r"\frac{a}{b}");
        assert_eq!(
            render(r#"ToExpression["{latex_str}", TeXForm]"#, r"\frac{a}{b}"),
            r#"ToExpression["\\frac{a}{b}", TeXForm]"#
        );
        assert_eq!(render("'${latex_sq}$'", "f'(x)"), "'$f''(x)$'");
        assert_eq!(render("${latex}$", "x^2"), "$x^2$");
    }

    #[test]
    fn wolfram() {
        assert_eq!(wolfram_tex(r"\int x e^{-x}d x"), r"\int x \exp\left(-x\right)\,d\,x");
        assert_eq!(
            wolfram_tex(r"\int_{0}^{\infty}x^{a-1}e^{-r x}\,d x"),
            r"\int_{0}^{\infty}x^{a-1}\exp\left(-r\,x\right)\,d\,x"
        );
        assert_eq!(wolfram_tex(r"e^x + y e^{2}"), r"\exp\left(x\right) + y \exp\left(2\right)");
        assert_eq!(wolfram_tex(r"\int u\,d v=u v-\int v\,d u"), r"\int u\,d\,v=u\,v-\int v\,d\,u");
        assert_eq!(wolfram_tex(r"\sin x \cos y"), r"\sin\left(x\right) \cos\left(y\right)");
        assert_eq!(wolfram_tex(r"\sin^{2}\theta+\ln(x)"), r"\sin^{2}\left(\theta\right)+\ln(x)");
        assert_eq!(wolfram_tex(r"\log 2x"), r"\log\left(2x\right)");
        assert_eq!(wolfram_tex(r"x^{2}+2a c"), r"x^{2}+2a\,c");
        assert_eq!(
            wolfram_tex(r"\begin{array}{r}{E X=x.{\mathrm{~Take~}}u=x}\end{array}"),
            r"{E\,X=x.{\text{ Take }}u=x}"
        );
        assert_eq!(
            render(r#"ToExpression["{latex_wl_str}", TeXForm]"#, r"a b"),
            r#"ToExpression["a\\,b", TeXForm]"#
        );
    }

    #[test]
    fn clean_for_display() {
        assert_eq!(tex_clean(r"\int x e^{-x}d x"), r"\int x e^{-x}\,d x");
        assert_eq!(
            tex_clean(r"\begin{array}{r}{a=b.{\mathrm{~Take~}}c}\end{array}"),
            r"{a=b.{\text{ Take }}c}"
        );
        assert_eq!(render("{latex_clean}", r"\int f d x"), r"\int f \,d x");
    }

    #[test]
    fn mathml() {
        let m = to_mathml(r"\int_{0}^{1} \frac{a}{b} \, dx");
        assert!(m.starts_with("<math xmlns=\"http://www.w3.org/1998/Math/MathML\""), "{m}");
        assert!(m.contains("<mfrac>"), "{m}");
    }
}
