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
#[test]
fn budgeted_backing_refunds_only_completed_source_storage() {
    let narrow = "x".repeat(65_536) + "π";
    let paid = narrow.len() * 4;
    let mut lexer = super::Lexer::budgeted_script(&narrow, 50_000, paid);
    let actual = lexer.chars.storage().1;
    assert_eq!(lexer.remaining_compilation_bytes(), 50_000 + paid - actual);
    let before = lexer.remaining_compilation_bytes();
    let token = lexer.next();
    assert_eq!(lexer.lookahead_charge(&token), before - lexer.remaining_compilation_bytes());
    let mut delimiters = super::Lexer::new(" ):x", 1024, true);
    for _ in 0..4 {
        let before = delimiters.remaining_compilation_bytes();
        let token = delimiters.next();
        assert_eq!(delimiters.lookahead_charge(&token), before - delimiters.remaining_compilation_bytes());
    }
}

#[test]
fn retired_literal_reserve_cannot_be_reused_twice_or_created_by_a_plain_lexer() {
    let mut vm_input = Lexer::budgeted_script("abcdefghijkl", 2000, 12);
    let inner = vm_input.span(2, 6);
    let outer = vm_input.span(0, 10);
    let end = vm_input.span(10, 12);
    vm_input.finish_deferred_function(2000, 100, &inner).unwrap();
    assert_eq!(vm_input.remaining_compilation_bytes(), 1900 + 4 * LITERAL_SCRATCH_PER_BYTE);
    vm_input.finish_deferred_function(2000, 200, &outer).unwrap();
    assert_eq!(vm_input.remaining_compilation_bytes(), 1800 + 4 * LITERAL_SCRATCH_PER_BYTE);
    let remaining = vm_input.remaining_compilation_bytes();
    vm_input.finish_deferred_function(remaining, 50, &end).unwrap();
    assert_eq!(vm_input.remaining_compilation_bytes(), remaining - 50 + 2 * LITERAL_SCRATCH_PER_BYTE);
    let mut other = Lexer::new("abcdefghijkl", 2000, true);
    other.finish_deferred_function(2000, 100, &inner).unwrap();
    assert_eq!(other.remaining_compilation_bytes(), 1900);
}
