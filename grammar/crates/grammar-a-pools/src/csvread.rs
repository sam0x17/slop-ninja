//! A minimal RFC 4180 reader: comma separators, double-quoted fields with
//! doubled quotes, embedded newlines inside quotes, and LF or CRLF record
//! ends. A leading UTF-8 byte-order mark is skipped. Field text is returned
//! exactly as written (no trimming or type conversion).
use anyhow::{Result, bail};

pub struct Reader<'a> {
    s: &'a str,
    pos: usize,
    pub records: u64,
}

impl<'a> Reader<'a> {
    pub fn new(s: &'a str) -> Self {
        let pos = if s.starts_with('\u{FEFF}') { '\u{FEFF}'.len_utf8() } else { 0 };
        Reader { s, pos, records: 0 }
    }

    /// Next record, or `None` at end of input. A final empty line is not a
    /// record.
    pub fn next_record(&mut self) -> Result<Option<Vec<String>>> {
        let b = self.s.as_bytes();
        if self.pos >= b.len() {
            return Ok(None);
        }
        let mut fields = Vec::new();
        loop {
            // One field.
            let mut field = String::new();
            if b.get(self.pos) == Some(&b'"') {
                self.pos += 1;
                loop {
                    let Some(rel) = self.s[self.pos..].find('"') else {
                        bail!("unterminated quoted field in record {}", self.records + 1);
                    };
                    field.push_str(&self.s[self.pos..self.pos + rel]);
                    self.pos += rel + 1;
                    if b.get(self.pos) == Some(&b'"') {
                        field.push('"');
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                match b.get(self.pos) {
                    None | Some(b',') | Some(b'\n') => {}
                    Some(b'\r') if b.get(self.pos + 1) == Some(&b'\n') => {}
                    Some(_) => bail!("text after closing quote in record {}", self.records + 1),
                }
            } else {
                let rest = &self.s[self.pos..];
                let end = rest.find([',', '\n']).unwrap_or(rest.len());
                let mut f = &rest[..end];
                if end < rest.len() && rest.as_bytes()[end] == b'\n' {
                    f = f.strip_suffix('\r').unwrap_or(f);
                }
                if f.contains('"') {
                    bail!("quote inside unquoted field in record {}", self.records + 1);
                }
                field.push_str(f);
                // A CRLF record end leaves `pos` on the CR, consumed below.
                self.pos += f.len();
            }
            fields.push(field);
            match b.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'\r') => {
                    self.pos += 2;
                    break;
                }
                Some(b'\n') => {
                    self.pos += 1;
                    break;
                }
                None => break,
                Some(_) => bail!("unexpected byte in record {}", self.records + 1),
            }
        }
        self.records += 1;
        Ok(Some(fields))
    }
}

/// Header plus records as rows of the header's width.
pub struct Table<'a> {
    pub header: Vec<String>,
    reader: Reader<'a>,
}

impl<'a> Table<'a> {
    pub fn new(s: &'a str) -> Result<Self> {
        let mut reader = Reader::new(s);
        let Some(header) = reader.next_record()? else { bail!("empty CSV") };
        Ok(Table { header, reader })
    }
    pub fn col(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }
    pub fn next_row(&mut self) -> Result<Option<Vec<String>>> {
        let Some(r) = self.reader.next_record()? else { return Ok(None) };
        if r.len() != self.header.len() {
            bail!("record {} has {} fields, header has {}", self.reader.records, r.len(), self.header.len());
        }
        Ok(Some(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn all(s: &str) -> Vec<Vec<String>> {
        let mut r = Reader::new(s);
        let mut v = Vec::new();
        while let Some(x) = r.next_record().unwrap() {
            v.push(x);
        }
        v
    }
    #[test]
    fn quoted_fields_newlines_and_crlf() {
        let v = all("\u{FEFF}a,b,c\r\n1,\"x, \"\"y\"\"\nz\",\r\n\"\",q,\"r\"\n");
        assert_eq!(v, vec![
            vec!["a", "b", "c"],
            vec!["1", "x, \"y\"\nz", ""],
            vec!["", "q", "r"],
        ]);
    }
    #[test]
    fn no_trailing_newline_and_errors() {
        assert_eq!(all("a,b\n1,2"), vec![vec!["a", "b"], vec!["1", "2"]]);
        assert!(Reader::new("\"abc").next_record().is_err());
        assert!(Reader::new("\"a\"b,c\n").next_record().is_err());
        let mut t = Table::new("a,b\n1,2,3\n").unwrap();
        assert!(t.next_row().is_err());
    }
}
