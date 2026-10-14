//! kond's RFC-822-style message files (headers, blank line, verbatim body).
//! Not live storage: the import/export format for `conde import` later.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailMessage {
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl MailMessage {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn headers_named<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers.iter().filter(move |(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn parse(raw: &str) -> MailMessage {
        let (head, body) = raw.split_once("\n\n").unwrap_or((raw, ""));
        let headers = head
            .lines()
            .map(|line| match line.split_once(": ") {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (line.to_string(), String::new()),
            })
            .collect();
        MailMessage { headers, body: body.to_string() }
    }

    pub fn render(&self) -> String {
        let mut out: String = self.headers.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
        out.push('\n');
        out.push_str(&self.body);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let raw = "From: a\nTo: #c\nAttachment: /x\nAttachment: /y\n\nbody\n\nwith blank\n";
        let m = MailMessage::parse(raw);
        assert_eq!(m.header("To"), Some("#c"));
        assert_eq!(m.headers_named("Attachment").count(), 2);
        assert_eq!(m.body, "body\n\nwith blank\n");
        assert_eq!(m.render(), raw);
    }
}
