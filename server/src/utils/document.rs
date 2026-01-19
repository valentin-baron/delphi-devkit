use tower_lsp::lsp_types::Range;


pub struct Document<'str> {
    pub content: &'str str,
}

impl<'str> Document<'str> {
    pub fn new(content: &'str str) -> Self {
        Document { content }
    }

    pub fn range(&self, range: Range) -> &str {
        let mut offset = 0;
        let mut start_offset = 0;
        let mut end_offset = self.content.len();

        for (i, line) in self.content.lines().enumerate() {
            if i == range.start.line as usize {
                start_offset = offset + range.start.character as usize;
            }
            if i == range.end.line as usize {
                end_offset = offset + range.end.character as usize;
                break;
            }
            offset += line.len() + 1; // +1 for '\n'
        }

        &self.content[start_offset..end_offset]
    }
}