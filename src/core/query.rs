#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
}
impl Query {
    pub fn parse(input: &str) -> Self {
        Self {
            text: input.trim().to_owned(),
        }
    }
    pub fn like_pattern(&self) -> String {
        format!(
            "%{}%",
            self.text
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    }
    pub fn fts_phrase(&self) -> Option<String> {
        (self.text.chars().count() >= 3).then(|| format!("\"{}\"", self.text.replace('"', "\"\"")))
    }
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
    }
}
