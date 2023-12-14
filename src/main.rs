use std::ops::Range;

use chumsky::{input::Stream, prelude::*};
use logos::Logos;
use miette::{miette, LabeledSpan};
use parser::{parser, LogosToken};
mod analyzer;
mod parser;

fn main() {
    const SRC: &str = r##"
// this is a function
f x = {
    f = x + 1
    &f
}
    "##;
    let token_iter = LogosToken::lexer(SRC)
        .spanned()
        .map(|(tok, span)| match tok {
            Ok(tok) => (tok, span.into()),
            Err(()) => (LogosToken::Error, span.into()),
        });
    let token_stream = Stream::from_iter(token_iter)
        .spanned::<LogosToken, SimpleSpan>((SRC.len()..SRC.len()).into());

    match parser().parse(token_stream).into_result() {
        Ok(stuff) => analyzer::analyzer(stuff),
        Err(errs) => {
            for err in errs {
                let span: Range<usize> = (*err.span()).into();
                let reason = err.reason().to_string();
                println!(
                    "{:?}",
                    miette!(
                        labels = vec![LabeledSpan::at(span, reason)],
                        "Parsing error"
                    )
                    .with_source_code(SRC)
                );
            }
        }
    }
}
