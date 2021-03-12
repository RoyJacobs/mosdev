use crate::errors::MosResult;
use crate::formatting::{format, FormattingOptions};
use crate::impl_request_handler;
use crate::lsp::{LspContext, RequestHandler};
use dissimilar::{diff, Chunk};
use lsp_types::{DocumentFormattingParams, DocumentOnTypeFormattingParams, TextEdit};

pub struct FormattingRequestHandler {}
pub struct OnTypeFormattingRequestHandler {}

impl_request_handler!(FormattingRequestHandler);
impl_request_handler!(OnTypeFormattingRequestHandler);

impl RequestHandler<lsp_types::request::Formatting> for FormattingRequestHandler {
    fn handle(
        &self,
        ctx: &mut LspContext,
        _params: DocumentFormattingParams,
    ) -> MosResult<Option<Vec<TextEdit>>> {
        Ok(do_formatting(ctx))
    }
}

impl RequestHandler<lsp_types::request::OnTypeFormatting> for OnTypeFormattingRequestHandler {
    fn handle(
        &self,
        ctx: &mut LspContext,
        _params: DocumentOnTypeFormattingParams,
    ) -> MosResult<Option<Vec<TextEdit>>> {
        Ok(do_formatting(ctx))
    }
}

fn do_formatting(ctx: &mut LspContext) -> Option<Vec<TextEdit>> {
    ctx.analysis.as_ref().map(|analysis| {
        let old_text = analysis.tree.files().first().unwrap().source();
        let new_text = format(analysis.tree.clone(), FormattingOptions::default());
        get_text_edits(old_text, &new_text)
    })
}

#[derive(Clone)]
struct RangeKeeper {
    line: u32,
    character: u32,
}

impl RangeKeeper {
    fn new() -> Self {
        Self {
            line: 0,
            character: 0,
        }
    }

    fn push(&mut self, mut str: &str) {
        loop {
            match str.find('\n') {
                Some(newline_idx) => {
                    self.line += 1;
                    self.character = 0;
                    str = str.split_at(newline_idx + 1).1;
                }
                None => {
                    self.character += str.len() as u32;
                    break;
                }
            }
        }
    }

    fn to_range(&self, str: &str) -> lsp_types::Range {
        let mut end_rk = self.clone();
        end_rk.push(str);
        rng(self.line, self.character, end_rk.line, end_rk.character)
    }
}

fn get_text_edits(old_text: &str, new_text: &str) -> Vec<TextEdit> {
    let mut rk = RangeKeeper::new();

    let edits = diff(old_text, new_text);
    let mut idx = 0;
    let mut result = vec![];
    while idx < edits.len() {
        match (edits[idx], edits.get(idx + 1)) {
            (Chunk::Equal(str), _) => {
                rk.push(str);
                idx += 1;
            }
            (Chunk::Insert(str), _) => {
                let cur_range = rk.to_range("");
                result.push(TextEdit {
                    range: cur_range,
                    new_text: str.into(),
                });
                idx += 1;
            }
            (Chunk::Delete(del), Some(Chunk::Insert(ins))) if del.len() == ins.len() => {
                let cur_range = rk.to_range(del);
                result.push(TextEdit {
                    range: cur_range,
                    new_text: ins.to_string(),
                });

                idx += 2;
            }
            (Chunk::Delete(str), _) => {
                let cur_range = rk.to_range(str);
                result.push(TextEdit {
                    range: cur_range,
                    new_text: "".to_string(),
                });

                idx += 1;
            }
        }
    }
    result
}

fn rng(start_line: u32, start_column: u32, end_line: u32, end_column: u32) -> lsp_types::Range {
    lsp_types::Range {
        start: lsp_types::Position {
            line: start_line,
            character: start_column,
        },
        end: lsp_types::Position {
            line: end_line,
            character: end_column,
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::lsp::formatting::{get_text_edits, rng, RangeKeeper};
    use lsp_types::TextEdit;

    #[test]
    fn range_keeper() {
        let mut rk = RangeKeeper::new();
        assert_eq!((rk.line, rk.character), (0, 0));
        rk.push("foo");
        assert_eq!((rk.line, rk.character), (0, 3));
        rk.push("bar");
        assert_eq!((rk.line, rk.character), (0, 6));
        rk.push("\nfoo");
        assert_eq!((rk.line, rk.character), (1, 3));
        rk.push("bar\nbar");
        assert_eq!((rk.line, rk.character), (2, 3));
    }

    #[test]
    fn can_get_text_edits() {
        let edits = get_text_edits("foo foo", "foo bar");
        assert_eq!(
            edits,
            vec![TextEdit {
                range: rng(0, 4, 0, 7),
                new_text: "bar".to_string()
            }]
        );

        let edits = get_text_edits("foo\nfoo", "foo\nbar");
        assert_eq!(
            edits,
            vec![TextEdit {
                range: rng(1, 0, 1, 3),
                new_text: "bar".to_string()
            }]
        );

        let edits = get_text_edits("foo foo foo", "foo bar");
        assert_eq!(
            edits,
            vec![
                TextEdit {
                    range: rng(0, 4, 0, 11),
                    new_text: "".to_string()
                },
                TextEdit {
                    range: rng(0, 4, 0, 4),
                    new_text: "bar".to_string()
                }
            ]
        );

        let edits = get_text_edits("{nop}", "{\n    nop\n}");
        assert_eq!(
            edits,
            vec![
                TextEdit {
                    range: rng(0, 1, 0, 1),
                    new_text: "\n    ".to_string()
                },
                TextEdit {
                    range: rng(0, 4, 0, 4),
                    new_text: "\n".to_string()
                }
            ]
        );
    }
}
