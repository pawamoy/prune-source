// SPDX-License-Identifier: ISC
// Copyright (c) 2021, Timothée Mazzucotelli and contributors

use pyo3::prelude::*;
use pyo3::pybacked::PyBackedStr;
use ruff_python_ast::token::{TokenFlags, TokenKind};
use ruff_python_parser::{Mode, lexer};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Scope {
    Module,
    Class,
    Function,
}

#[derive(Clone, Copy)]
struct ScopeFrame {
    scope: Scope,
    depth: u32,
}

struct BodyEdit {
    start: usize,
    end: usize,
    indent_start: usize,
    indent_end: usize,
    inline: bool,
}

#[derive(Clone, Copy)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
    flags: TokenFlags,
}

#[derive(Clone, Copy)]
enum DefinitionKind {
    Class,
    Function,
}

struct DefinitionHeader {
    kind: DefinitionKind,
    parent_scope: Scope,
    name_seen: bool,
    is_init: bool,
    delimiter_depth: u32,
    parameters_started: bool,
    parameters_closed: bool,
    return_annotation: bool,
    pending_lambda_colons: u32,
}

impl DefinitionHeader {
    fn new(kind: DefinitionKind, parent_scope: Scope) -> Self {
        Self {
            kind,
            parent_scope,
            name_seen: false,
            is_init: false,
            delimiter_depth: 0,
            parameters_started: false,
            parameters_closed: false,
            return_annotation: false,
            pending_lambda_colons: 0,
        }
    }

    fn suite_action(&self) -> SuiteAction {
        match self.kind {
            DefinitionKind::Class => SuiteAction::Enter(Scope::Class),
            DefinitionKind::Function if self.parent_scope == Scope::Class && self.is_init => {
                SuiteAction::Enter(Scope::Function)
            }
            DefinitionKind::Function => SuiteAction::Prune,
        }
    }

    /// Consume a token from a `class` or `def` header and report its suite colon.
    fn advance(&mut self, token: Token, source: &str) -> bool {
        if !self.name_seen && token.kind == TokenKind::Name {
            self.name_seen = true;
            self.is_init = &source[token.start..token.end] == "__init__";
            return false;
        }

        if matches!(
            token.kind,
            TokenKind::Comment | TokenKind::NonLogicalNewline
        ) {
            return false;
        }

        match self.kind {
            DefinitionKind::Class => self.advance_class(token.kind),
            DefinitionKind::Function => self.advance_function(token.kind),
        }
    }

    fn advance_class(&mut self, kind: TokenKind) -> bool {
        if kind == TokenKind::Colon && self.delimiter_depth == 0 {
            return true;
        }
        self.update_delimiter_depth(kind);
        false
    }

    fn advance_function(&mut self, kind: TokenKind) -> bool {
        if is_opening_delimiter(kind) {
            if kind == TokenKind::Lpar && self.delimiter_depth == 0 && !self.parameters_started {
                self.parameters_started = true;
            }
            self.delimiter_depth += 1;
            return false;
        }

        if is_closing_delimiter(kind) {
            self.delimiter_depth = self.delimiter_depth.saturating_sub(1);
            if kind == TokenKind::Rpar && self.delimiter_depth == 0 && self.parameters_started {
                self.parameters_closed = true;
            }
            return false;
        }

        if self.delimiter_depth != 0 || !self.parameters_closed {
            return false;
        }

        match kind {
            TokenKind::Rarrow => self.return_annotation = true,
            TokenKind::Lambda if self.return_annotation => self.pending_lambda_colons += 1,
            TokenKind::Colon if self.pending_lambda_colons > 0 => {
                self.pending_lambda_colons -= 1;
            }
            TokenKind::Colon => return true,
            _ => {}
        }
        false
    }

    fn update_delimiter_depth(&mut self, kind: TokenKind) {
        if is_opening_delimiter(kind) {
            self.delimiter_depth += 1;
        } else if is_closing_delimiter(kind) {
            self.delimiter_depth = self.delimiter_depth.saturating_sub(1);
        }
    }
}

const fn is_opening_delimiter(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Lpar | TokenKind::Lsqb | TokenKind::Lbrace)
}

const fn is_closing_delimiter(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Rpar | TokenKind::Rsqb | TokenKind::Rbrace)
}

#[derive(Clone, Copy)]
enum SuiteAction {
    Enter(Scope),
    Prune,
}

#[derive(Clone, Copy)]
enum PendingSuiteKind {
    Undetermined,
    Indented,
}

#[derive(Clone, Copy)]
struct PendingSuite {
    action: SuiteAction,
    kind: PendingSuiteKind,
}

impl PendingSuite {
    const fn new(action: SuiteAction) -> Self {
        Self {
            action,
            kind: PendingSuiteKind::Undetermined,
        }
    }
}

#[derive(Clone, Copy)]
enum FunctionSuite {
    Inline,
    Indented { depth: u32 },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DocstringPhase {
    OpeningParentheses,
    Strings,
    ClosingParentheses,
}

struct FunctionBody {
    suite: FunctionSuite,
    first_statement_start: Option<usize>,
    indent_start: usize,
    candidate_docstring: bool,
    saw_string: bool,
    docstring_phase: DocstringPhase,
    parenthesis_depth: u32,
    first_statement_finished: bool,
    edit_start: Option<usize>,
    last_code_end: Option<usize>,
}

impl FunctionBody {
    const fn new(suite: FunctionSuite) -> Self {
        Self {
            suite,
            first_statement_start: None,
            indent_start: 0,
            candidate_docstring: true,
            saw_string: false,
            docstring_phase: DocstringPhase::OpeningParentheses,
            parenthesis_depth: 0,
            first_statement_finished: false,
            edit_start: None,
            last_code_end: None,
        }
    }

    fn closes_at(&self, token: Token, indentation_depth: u32) -> bool {
        match self.suite {
            FunctionSuite::Inline => token.kind == TokenKind::Newline,
            FunctionSuite::Indented { depth } => {
                token.kind == TokenKind::Dedent && indentation_depth < depth
            }
        }
    }

    fn consume(&mut self, token: Token, source: &[u8]) {
        if matches!(
            token.kind,
            TokenKind::Comment
                | TokenKind::NonLogicalNewline
                | TokenKind::Indent
                | TokenKind::Dedent
                | TokenKind::EndOfFile
        ) {
            return;
        }

        if matches!(token.kind, TokenKind::Newline | TokenKind::Semi) {
            if !self.first_statement_finished {
                let is_docstring =
                    self.candidate_docstring && self.saw_string && self.parenthesis_depth == 0;
                self.first_statement_finished = true;
                if !is_docstring && self.edit_start.is_none() {
                    self.edit_start = self.first_statement_start;
                }
            }
            return;
        }

        if self.first_statement_start.is_none() {
            self.first_statement_start = Some(token.start);
            self.indent_start = line_start(source, token.start);
        }

        self.last_code_end = Some(token.end);

        if self.first_statement_finished {
            if self.edit_start.is_none() {
                self.edit_start = Some(token.start);
            }
            return;
        }

        match token.kind {
            TokenKind::String
                if !token.flags.contains(TokenFlags::BYTE_STRING)
                    && self.docstring_phase != DocstringPhase::ClosingParentheses =>
            {
                self.saw_string = true;
                self.docstring_phase = DocstringPhase::Strings;
            }
            TokenKind::Lpar if self.docstring_phase == DocstringPhase::OpeningParentheses => {
                self.parenthesis_depth += 1;
            }
            TokenKind::Rpar if self.saw_string && self.parenthesis_depth > 0 => {
                self.parenthesis_depth -= 1;
                self.docstring_phase = DocstringPhase::ClosingParentheses;
            }
            _ => {
                self.candidate_docstring = false;
                if self.edit_start.is_none() {
                    self.edit_start = self.first_statement_start;
                }
            }
        }
    }

    fn into_edit(self) -> Option<BodyEdit> {
        Some(BodyEdit {
            start: self.edit_start?,
            end: self.last_code_end?,
            indent_start: self.indent_start,
            indent_end: self.first_statement_start?,
            inline: matches!(self.suite, FunctionSuite::Inline),
        })
    }
}

struct BodyCollector<'source> {
    source: &'source str,
    indentation_depth: u32,
    scopes: Vec<ScopeFrame>,
    header: Option<DefinitionHeader>,
    pending_suite: Option<PendingSuite>,
    function_body: Option<FunctionBody>,
    edits: Vec<BodyEdit>,
}

impl<'source> BodyCollector<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            indentation_depth: 0,
            scopes: Vec::new(),
            header: None,
            pending_suite: None,
            function_body: None,
            edits: Vec::new(),
        }
    }

    fn current_scope(&self) -> Scope {
        self.scopes
            .last()
            .map_or(Scope::Module, |frame| frame.scope)
    }

    fn consume(&mut self, token: Token) {
        match token.kind {
            TokenKind::Indent => self.indentation_depth += 1,
            TokenKind::Dedent => {
                self.indentation_depth = self.indentation_depth.saturating_sub(1);
                while self
                    .scopes
                    .last()
                    .is_some_and(|frame| frame.depth > self.indentation_depth)
                {
                    self.scopes.pop();
                }
            }
            _ => {}
        }

        if let Some(body) = self.function_body.take() {
            if body.closes_at(token, self.indentation_depth) {
                if let Some(edit) = body.into_edit() {
                    self.edits.push(edit);
                }
            } else {
                let mut body = body;
                body.consume(token, self.source.as_bytes());
                self.function_body = Some(body);
            }
            return;
        }

        if token.kind == TokenKind::Dedent {
            return;
        }

        if let Some(mut pending) = self.pending_suite.take() {
            match pending.kind {
                PendingSuiteKind::Undetermined => match token.kind {
                    TokenKind::Newline => {
                        pending.kind = PendingSuiteKind::Indented;
                        self.pending_suite = Some(pending);
                    }
                    TokenKind::Comment | TokenKind::NonLogicalNewline => {
                        self.pending_suite = Some(pending);
                    }
                    _ => self.start_inline_suite(pending.action, token),
                },
                PendingSuiteKind::Indented => match token.kind {
                    TokenKind::Indent => self.start_indented_suite(pending.action),
                    TokenKind::Comment | TokenKind::Newline | TokenKind::NonLogicalNewline => {
                        self.pending_suite = Some(pending)
                    }
                    _ => {}
                },
            }
            return;
        }

        if let Some(header) = self.header.as_mut() {
            if header.advance(token, self.source) {
                let action = header.suite_action();
                self.header = None;
                self.pending_suite = Some(PendingSuite::new(action));
            }
            return;
        }

        match token.kind {
            TokenKind::Class => {
                self.header = Some(DefinitionHeader::new(
                    DefinitionKind::Class,
                    self.current_scope(),
                ));
            }
            TokenKind::Def => {
                self.header = Some(DefinitionHeader::new(
                    DefinitionKind::Function,
                    self.current_scope(),
                ));
            }
            _ => {}
        }
    }

    fn start_inline_suite(&mut self, action: SuiteAction, token: Token) {
        if matches!(action, SuiteAction::Prune) {
            let mut body = FunctionBody::new(FunctionSuite::Inline);
            body.consume(token, self.source.as_bytes());
            self.function_body = Some(body);
        }
    }

    fn start_indented_suite(&mut self, action: SuiteAction) {
        match action {
            SuiteAction::Enter(scope) => self.scopes.push(ScopeFrame {
                scope,
                depth: self.indentation_depth,
            }),
            SuiteAction::Prune => {
                self.function_body = Some(FunctionBody::new(FunctionSuite::Indented {
                    depth: self.indentation_depth,
                }));
            }
        }
    }

    fn finish(mut self) -> Vec<BodyEdit> {
        if let Some(body) = self.function_body.take()
            && let Some(edit) = body.into_edit()
        {
            self.edits.push(edit);
        }
        self.edits
    }
}

fn physical_line_end(source: &[u8], offset: usize) -> usize {
    source[offset..]
        .iter()
        .position(|byte| matches!(byte, b'\r' | b'\n'))
        .map_or(source.len(), |relative| offset + relative)
}

fn line_start(source: &[u8], offset: usize) -> usize {
    source[..offset]
        .iter()
        .rposition(|byte| matches!(byte, b'\r' | b'\n'))
        .map_or(0, |position| position + 1)
}

fn copy_newlines(output: &mut Vec<u8>, source: &[u8]) {
    output.extend(
        source
            .iter()
            .copied()
            .filter(|byte| matches!(byte, b'\r' | b'\n')),
    );
}

fn apply_edits(source: &str, edits: Vec<BodyEdit>) -> String {
    let source_bytes = source.as_bytes();
    let mut pruned = Vec::with_capacity(source_bytes.len());
    let mut copied_until = 0;
    debug_assert!(
        edits
            .windows(2)
            .all(|edits| edits[0].start < edits[1].start)
    );

    for edit in edits {
        let end = physical_line_end(source_bytes, edit.end);
        let start_line = line_start(source_bytes, edit.start);
        let last_line_start = line_start(source_bytes, edit.end);
        let replace_at_start = edit.inline || start_line == last_line_start;
        let indentation = &source_bytes[edit.indent_start..edit.indent_end];

        if replace_at_start {
            pruned.extend_from_slice(&source_bytes[copied_until..edit.start]);
            pruned.extend_from_slice(b"pass");
            copy_newlines(&mut pruned, &source_bytes[edit.start..end]);
        } else {
            let prefix = &source_bytes[start_line..edit.start];
            let removal_start = if prefix
                .iter()
                .all(|byte| matches!(byte, b' ' | b'\t' | 0x0c))
            {
                start_line
            } else {
                edit.start
            };

            pruned.extend_from_slice(&source_bytes[copied_until..removal_start]);
            copy_newlines(&mut pruned, &source_bytes[removal_start..last_line_start]);
            pruned.extend_from_slice(indentation);
            pruned.extend_from_slice(b"pass");
        }

        copied_until = end;
    }
    pruned.extend_from_slice(&source_bytes[copied_until..]);

    // We only copy valid UTF-8 ranges, retain newline bytes, and insert ASCII.
    String::from_utf8(pruned).expect("pruned Python source must remain UTF-8")
}

fn prune(source: &str) -> Option<String> {
    let mut collector = BodyCollector::new(source);
    let mut lexer = lexer::lex(source, Mode::Module);

    loop {
        let kind = lexer.next_token();
        let range = lexer.current_range();
        collector.consume(Token {
            kind,
            start: range.start().to_usize(),
            end: range.end().to_usize(),
            flags: lexer.current_flags(),
        });

        if kind.is_eof() {
            break;
        }
    }

    let edits = collector.finish();
    if edits.is_empty() {
        None
    } else {
        Some(apply_edits(source, edits))
    }
}

/// Lex Python with Ruff and replace function implementation suites with `pass`.
///
/// Function signatures and docstrings are preserved, as are class `__init__` implementations.
/// The caller promises that `source` is valid for its running Python interpreter. `None` asks the
/// caller to compile the original source and means that there was nothing to prune.
#[pyfunction]
fn prune_source(py: Python<'_>, source: PyBackedStr) -> Option<String> {
    py.detach(move || prune(&source))
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("RUFF_VERSION", "0.16.5")?;
    module.add_function(wrap_pyfunction!(prune_source, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::prune;

    fn pruned(source: &str) -> String {
        prune(source).expect("source should contain a prunable function")
    }

    #[test]
    fn strips_regular_function_implementation_but_keeps_docstring_and_lines() {
        let source = "def f():\n\t\"doc\"\n\tvalue = (\n\t\t1\n\t)\n";
        let output = pruned(source);
        assert_eq!(output, "def f():\n\t\"doc\"\n\n\n\tpass\n");
        assert_eq!(output.lines().count(), source.lines().count());
        assert!(!output.contains("value"));
    }

    #[test]
    fn recognizes_parenthesized_and_concatenated_docstrings() {
        let source = concat!(
            "def f():\n",
            "    (\n",
            "        \"first\"  # retained\n",
            "        r\"second\"\n",
            "    )\n",
            "    implementation = 1\n",
        );
        let output = pruned(source);
        assert!(output.contains("\"first\""));
        assert!(output.contains("r\"second\""));
        assert!(output.contains("    pass"));
        assert!(!output.contains("implementation"));
    }

    #[test]
    fn does_not_treat_bytes_or_string_expressions_as_docstrings() {
        for source in [
            "def f():\n    b\"not a docstring\"\n    implementation = 1\n",
            "def f():\n    \"not a docstring\".upper()\n    implementation = 1\n",
            "def f():\n    (\"not a docstring\",)\n    implementation = 1\n",
            "def f():\n    \"not a docstring\"(\"argument\")\n    implementation = 1\n",
        ] {
            let output = pruned(source);
            assert!(!output.contains("not a docstring"));
            assert!(!output.contains("implementation"));
        }
    }

    #[test]
    fn keeps_class_init_implementation() {
        let source = "class C:\n    def __init__(self):\n        self.value = 1\n";
        assert_eq!(prune(source), None);
    }

    #[test]
    fn strips_nested_functions_inside_init() {
        let source = concat!(
            "class C:\n",
            "    def __init__(self):\n",
            "        def nested():\n",
            "            implementation = 1\n",
            "        self.value = 1\n",
        );
        let output = pruned(source);
        assert!(output.contains("            pass"));
        assert!(output.contains("        self.value = 1"));
    }

    #[test]
    fn respects_semantic_scope_through_control_flow_and_nested_classes() {
        let source = concat!(
            "class Outer:\n",
            "    if enabled:\n",
            "        def __init__(self):\n",
            "            self.kept = 1\n",
            "    def method(self):\n",
            "        removed = 1\n",
            "    def __init__(self):\n",
            "        class Inner:\n",
            "            def __init__(self):\n",
            "                self.also_kept = 1\n",
            "            def method(self):\n",
            "                also_removed = 1\n",
        );
        let output = pruned(source);
        assert!(output.contains("self.kept = 1"));
        assert!(output.contains("self.also_kept = 1"));
        assert!(!output.contains("removed = 1"));
        assert!(!output.contains("also_removed = 1"));
    }

    #[test]
    fn strips_one_line_function_bodies_and_preserves_inline_docstrings() {
        let source = concat!(
            "def f(): value = 1; return value\n",
            "def g(): \"doc\"; return 1  # comment\n",
            "next_value = 2\n",
        );
        let output = pruned(source);
        assert!(output.starts_with("def f(): pass"));
        assert!(output.contains("def g(): \"doc\"; pass"));
        assert!(output.ends_with("next_value = 2\n"));
        assert_eq!(output.lines().count(), source.lines().count());
    }

    #[test]
    fn strips_inline_suite_with_explicit_line_continuation() {
        let source = "def f(): return first or \\\n    second\nnext_value = 2\n";
        let output = pruned(source);
        assert!(output.starts_with("def f(): pass"));
        assert_eq!(output.matches("def f(): pass").count(), 1);
        assert!(output.ends_with("next_value = 2\n"));
    }

    #[test]
    fn finds_suite_colon_after_lambda_return_annotation() {
        let source = concat!(
            "def f() -> lambda: int: value: int = 1\n",
            "def g(x=f\"{value:>{width}}\"):\n",
            "    implementation = 1\n",
        );
        let output = pruned(source);
        assert!(output.starts_with("def f() -> lambda: int: pass"));
        assert!(output.contains("def g(x=f\"{value:>{width}}\"):\n    pass"));
    }

    #[test]
    fn preserves_comments_outside_statement_ranges() {
        let source = concat!(
            "def f():\n",
            "    # before docstring\n",
            "    \"doc\"\n",
            "    # between\n",
            "    implementation = 1  # erased with statement\n",
            "    # trailing\n",
            "next_value = 2\n",
        );
        let output = pruned(source);
        assert!(output.contains("# before docstring"));
        assert!(output.contains("# between"));
        assert!(output.contains("# trailing"));
        assert!(!output.contains("erased with statement"));
    }

    #[test]
    fn preserves_crlf_newlines() {
        let source = "def f():\r\n    value = 1\r\n    return value\r\n";
        let output = pruned(source);
        assert_eq!(output.matches("\r\n").count(), 3);
        assert_eq!(output.replace("\r\n", "").matches('\n').count(), 0);
    }

    #[test]
    fn preserves_carriage_return_newlines() {
        let source = "def f():\r    value = 1\r    return value\r";
        let output = pruned(source);
        assert_eq!(output.matches('\r').count(), 3);
        assert!(!output.contains('\n'));
        assert!(output.contains("    pass"));
    }
}
