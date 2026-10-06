#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Text(String),
    Extension(String),
    File(String),
    Folder(String),
    Path(String),
}

#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub terms: Vec<Term>,
}
impl Query {
    pub fn parse(input: &str) -> Self {
        let text = input.trim().to_owned();
        let mut terms = Vec::new();
        let mut plain = Vec::new();
        // Keep an ordinary multi-word query as one literal substring, as in v0.1.0.
        for token in text.split_whitespace() {
            let term = token.split_once(':').and_then(|(key, value)| {
                let value = value.to_lowercase();
                match key.to_ascii_lowercase().as_str() {
                    "ext" => Some(Term::Extension(value.trim_start_matches('.').into())),
                    "file" => Some(Term::File(value)),
                    "folder" => Some(Term::Folder(value)),
                    "path" => Some(Term::Path(value)),
                    _ => None,
                }
            });
            if let Some(term) = term {
                terms.push(term);
            } else {
                plain.push(token);
            }
        }
        if terms.is_empty() {
            if !text.is_empty() {
                terms.push(Term::Text(text.to_lowercase()));
            }
        } else if !plain.is_empty() {
            terms.insert(0, Term::Text(plain.join(" ").to_lowercase()));
        }
        Self { text, terms }
    }
    pub fn rank_text(&self) -> &str {
        self.terms
            .iter()
            .find_map(|term| match term {
                Term::Text(value) | Term::File(value) | Term::Folder(value) => Some(value.as_str()),
                _ => None,
            })
            .unwrap_or("")
    }
    pub fn like_pattern(&self) -> String {
        like_pattern(&self.text)
    }
    pub fn fts_phrase(&self) -> Option<String> {
        fts_phrase(&self.text)
    }
}
pub fn like_pattern(text: &str) -> String {
    format!(
        "%{}%",
        text.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}
pub fn fts_phrase(text: &str) -> Option<String> {
    (text.chars().count() >= 3).then(|| format!("\"{}\"", text.replace('"', "\"\"")))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_query() {
        let q = Query::parse(" a_%\\\" ");
        assert_eq!(q.like_pattern(), "%a\\_\\%\\\\\"%");
        assert_eq!(q.fts_phrase(), Some("\"a_%\\\"\"\"".into()));
        assert!(Query::parse("中文").fts_phrase().is_none());
        assert_eq!(
            Query::parse("foo:bar two  words").terms,
            vec![Term::Text("foo:bar two  words".into())]
        );
    }
    #[test]
    fn filters_and_unicode() {
        assert_eq!(
            Query::parse("erasmus ext:.ZIP path:Galgame").terms,
            vec![
                Term::Text("erasmus".into()),
                Term::Extension("zip".into()),
                Term::Path("galgame".into())
            ]
        );
        assert_eq!(
            Query::parse("file:补丁 folder:日本 path:_%\\\"").terms,
            vec![
                Term::File("补丁".into()),
                Term::Folder("日本".into()),
                Term::Path("_%\\\"".into())
            ]
        );
        assert_eq!(
            Query::parse("folder:").terms,
            vec![Term::Folder(String::new())]
        );
    }
}
