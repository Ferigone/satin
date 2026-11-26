use super::*;

#[test]
fn comment_runs_keep_delimiters_and_line_positions_across_pages() {
    for newline in ["\r\n", "\n", "\r", "\u{2028}", "\u{2029}"] {
        let body = format!("/*{}é*\n{}🙂*/ //line{newline}  true",
            "x".repeat(252), "y".repeat(65_536));
        let mut lexer = Lexer::new(&body, usize::MAX, true);
        let token = lexer.next();
        assert_eq!(token.kind, TokenKind::Word("true".into()));
        assert_eq!((token.line, token.column, token.newline), (3, 3, true));
    }
}

#[test]
fn scanning_a_shared_subspan_cannot_consume_a_delimiter_beyond_its_end() {
    let text = format!("{}/*{}é*/", "p".repeat(100), "a".repeat(70_000));
    let source = SourceSpan::from_text(&text);
    let inner = source.span(100..source.len() - 2);
    let mut lexer = Lexer::from_span((*inner).clone(), usize::MAX, true);
    assert_eq!(lexer.next().error.unwrap().kind, ErrorKind::SyntaxError);
    assert_eq!(lexer.pos, inner.len());
    let source = [47, 47, 0xd800, 0x2028, 49];
    let token = Lexer::from_utf16(&source, usize::MAX).next();
    assert_eq!((token.line, token.column, token.kind), (2, 1, TokenKind::Number(1.0)));
}
