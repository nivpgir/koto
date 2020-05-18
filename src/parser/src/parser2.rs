use {
    crate::{error::*, *},
    koto_lexer::{make_end_position, make_span, make_start_position, Lexer, Span, Token},
    std::{collections::HashSet, str::FromStr},
};

macro_rules! internal_error {
    ($error:ident, $parser:expr) => {{
        let extras = &$parser.lexer.extras();
        let error = ParserError::new(
            InternalError::$error.into(),
            make_span(
                $parser.lexer.source(),
                extras.line_number,
                extras.line_start,
                &$parser.lexer.span(),
            ),
        );
        #[cfg(panic_on_parser_error)]
        {
            panic!(error);
        }
        Err(error)
    }};
}

macro_rules! syntax_error {
    ($error:ident, $parser:expr) => {{
        let extras = &$parser.lexer.extras();
        let error = ParserError::new(
            SyntaxError::$error.into(),
            make_span(
                $parser.lexer.source(),
                extras.line_number,
                extras.line_start,
                &$parser.lexer.span(),
            ),
        );
        #[cfg(panic_on_parser_error)]
        {
            panic!(error);
        }
        Err(error)
    }};
}

fn trim_str(s: &str, trim_from_start: usize, trim_from_end: usize) -> &str {
    let start = trim_from_start;
    let end = s.len() - trim_from_end;
    &s[start..end]
}

#[derive(Default)]
struct Frame {
    ids_assigned_in_scope: HashSet<ConstantIndex>,
    captures: HashSet<ConstantIndex>,
    _top_level: bool,
}

impl Frame {
    fn local_count(&self) -> usize {
        self.ids_assigned_in_scope
            .difference(&self.captures)
            .count()
    }
}

pub struct Parser<'source, 'constants> {
    ast: Ast,
    lexer: Lexer<'source>,
    constants: &'constants mut ConstantPool,
    frame_stack: Vec<Frame>,
}

impl<'source, 'constants> Parser<'source, 'constants> {
    pub fn parse(
        source: &'source str,
        constants: &'constants mut ConstantPool,
    ) -> Result<Ast, ParserError> {
        let capacity_guess = source.len() / 4;
        let mut parser = Parser {
            ast: Ast::with_capacity(capacity_guess),
            lexer: Lexer::new(source),
            constants,
            frame_stack: Vec::new(),
        };

        let main_block = parser.parse_main_block()?;
        parser.ast.set_entry_point(main_block);

        Ok(parser.ast)
    }

    fn frame(&self) -> Result<&Frame, ParserError> {
        match self.frame_stack.last() {
            Some(frame) => Ok(frame),
            None => Err(ParserError::new(
                InternalError::MissingScope.into(),
                Span::default(),
            )),
        }
    }

    fn frame_mut(&mut self) -> Result<&mut Frame, ParserError> {
        match self.frame_stack.last_mut() {
            Some(frame) => Ok(frame),
            None => Err(ParserError::new(
                InternalError::MissingScope.into(),
                Span::default(),
            )),
        }
    }

    fn parse_main_block(&mut self) -> Result<AstIndex, ParserError> {
        self.frame_stack.push(Frame {
            _top_level: true,
            ..Frame::default()
        });

        let mut body = Vec::new();
        while self.peek_token().is_some() {
            if let Some(expression) = self.parse_line()? {
                body.push(expression);
            }
        }

        let result = self.ast.push(
            Node::MainBlock {
                body,
                local_count: self.frame()?.local_count(),
            },
            Span::default(),
        )?;

        self.frame_stack.pop();
        Ok(result)
    }

    fn parse_function(
        &mut self,
        primary_expression: bool,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.skip_whitespace_and_peek() != Some(Token::Function) {
            return internal_error!(FunctionParseFailure, self);
        }

        let current_indent = self.lexer.current_indent();

        self.consume_token();

        let start_extras = self.lexer.extras();
        let span_start = make_start_position(
            self.lexer.source(),
            start_extras.line_number,
            start_extras.line_start,
            &self.lexer.span(),
        );

        // args
        let mut args = Vec::new();
        while let Some(constant_index) = self.parse_id() {
            args.push(constant_index);
        }

        if self.skip_whitespace_and_next() != Some(Token::Function) {
            return syntax_error!(ExpectedFunctionArgsEnd, self);
        }

        // body
        let mut function_frame = Frame::default();
        function_frame.ids_assigned_in_scope.extend(args.clone());
        self.frame_stack.push(function_frame);

        let body = match self.skip_whitespace_and_peek() {
            Some(Token::NewLineIndented)
                if primary_expression && self.lexer.next_indent() > current_indent =>
            {
                if let Some(block) = self.parse_indented_block(current_indent)? {
                    block
                } else {
                    return internal_error!(FunctionParseFailure, self);
                }
            }
            _ => {
                if let Some(body) = self.parse_primary_expressions()? {
                    body
                } else {
                    return syntax_error!(ExpectedFunctionBody, self);
                }
            }
        };

        let end_extras = self.lexer.extras();
        let span_end = make_end_position(
            self.lexer.source(),
            end_extras.line_number,
            end_extras.line_start,
            &self.lexer.span(),
        );

        let result = self.ast.push(
            Node::Function(Function {
                args,
                captures: vec![], // TODO
                local_count: self.frame()?.local_count(),
                body,
                is_instance_function: false, // TODO
            }),
            Span {
                start: span_start,
                end: span_end,
            },
        )?;

        self.frame_stack.pop();
        Ok(Some(result))
    }

    fn parse_line(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if let Some(for_loop) = self.parse_for_loop(None)? {
            Ok(Some(for_loop))
        } else if let Some(while_loop) = self.parse_while_loop(None)? {
            Ok(Some(while_loop))
        } else if let Some(until_loop) = self.parse_until_loop(None)? {
            Ok(Some(until_loop))
        } else {
            if let Some(result) = self.parse_primary_expressions()? {
                // parse_primary_expressions may have not consumed the line end, so consume it now
                match self.skip_whitespace_and_peek() {
                    Some(Token::NewLine) | Some(Token::NewLineIndented) => {
                        self.consume_token();
                    }
                    _ => {}
                }
                Ok(Some(result))
            } else {
                return syntax_error!(ExpectedExpression, self);
            }
        }
    }

    fn parse_primary_expressions(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if let Some(first) = self.parse_primary_expression()? {
            let mut expressions = vec![first];
            while let Some(Token::Separator) = self.skip_whitespace_and_peek() {
                self.consume_token();
                if let Some(next_expression) = self.parse_primary_expression()? {
                    expressions.push(next_expression);
                } else {
                    return syntax_error!(ExpectedExpression, self);
                }
            }
            if expressions.len() == 1 {
                Ok(Some(first))
            } else {
                Ok(Some(self.push_node(Node::Expressions(expressions))?))
            }
        } else {
            Ok(None)
        }
    }

    fn parse_primary_expression(&mut self) -> Result<Option<AstIndex>, ParserError> {
        self.parse_expression(0)
    }

    fn parse_non_primary_expression(&mut self) -> Result<Option<AstIndex>, ParserError> {
        self.parse_expression(1)
    }

    fn parse_expression(&mut self, min_precedence: u8) -> Result<Option<AstIndex>, ParserError> {
        let primary_expression = min_precedence == 0;

        let lhs = {
            // ID expressions are broken out to allow function calls in first position
            if let Some(id_expression) = self.parse_id_expression(primary_expression)? {
                id_expression
            } else {
                let term = self.parse_term(primary_expression)?;

                match self.peek_token() {
                    Some(Token::Range) | Some(Token::RangeInclusive) => {
                        return self.parse_range(term)
                    }
                    _ => match term {
                        Some(term) => term,
                        None => return Ok(None),
                    },
                }
            }
        };

        self.parse_expression_with_lhs(lhs, min_precedence)
    }

    fn parse_expression_with_lhs(
        &mut self,
        mut lhs: AstIndex,
        min_precedence: u8,
    ) -> Result<Option<AstIndex>, ParserError> {
        use Token::*;

        while let Some(next) = self.skip_whitespace_and_peek() {
            match next {
                NewLine | NewLineIndented => {
                    break;
                }
                For => {
                    return self.parse_for_loop(Some(lhs));
                }
                While => {
                    return self.parse_while_loop(Some(lhs));
                }
                Until => {
                    return self.parse_until_loop(Some(lhs));
                }
                Assign => match self.ast.node(lhs).node.clone() {
                    Node::Id(id_index) => {
                        self.consume_token();

                        if let Some(rhs) = self.parse_primary_expressions()? {
                            let node = Node::Assign {
                                target: AssignTarget {
                                    target_index: lhs,
                                    scope: Scope::Local, // TODO
                                },
                                expression: rhs,
                            };
                            self.frame_mut()?.ids_assigned_in_scope.insert(id_index);
                            lhs = self.push_node(node)?;
                        } else {
                            return syntax_error!(ExpectedRhsExpression, self);
                        }
                    }
                    Node::Lookup(lookup) => {
                        self.consume_token();

                        let id_index = match lookup.as_slice() {
                            &[LookupNode::Id(id_index), ..] => id_index,
                            _ => return internal_error!(MissingLookupId, self),
                        };

                        if let Some(rhs) = self.parse_primary_expressions()? {
                            let node = Node::Assign {
                                target: AssignTarget {
                                    target_index: lhs,
                                    scope: Scope::Local, // TODO
                                },
                                expression: rhs,
                            };
                            self.frame_mut()?.ids_assigned_in_scope.insert(id_index);
                            lhs = self.push_node(node)?;
                        } else {
                            return syntax_error!(ExpectedRhsExpression, self);
                        }
                    }
                    _ => {
                        return syntax_error!(ExpectedAssignmentTarget, self);
                    }
                },
                AssignAdd | AssignSubtract | AssignMultiply | AssignDivide | AssignModulo => {
                    unimplemented!("Unimplemented assignment operator")
                }
                _ => {
                    if let Some(priority) = operator_precedence(next) {
                        if priority < min_precedence {
                            break;
                        }

                        let op = self.consume_token().unwrap();

                        if let Some(rhs) = self.parse_expression(priority)? {
                            lhs = self.push_ast_op(op, lhs, rhs)?;
                        } else {
                            return syntax_error!(ExpectedRhsExpression, self);
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(Some(lhs))
    }

    fn parse_id(&mut self) -> Option<ConstantIndex> {
        if let Some(Token::Id) = self.skip_whitespace_and_peek() {
            self.consume_token();
            Some(self.constants.add_string(self.lexer.slice()) as u32)
        } else {
            None
        }
    }

    fn parse_id_expression(
        &mut self,
        primary_expression: bool,
    ) -> Result<Option<AstIndex>, ParserError> {
        if let Some(constant_index) = self.parse_id() {
            let result = match self.peek_token() {
                Some(Token::Whitespace) if primary_expression => {
                    self.consume_token();
                    let id_index = self.push_node(Node::Id(constant_index))?;
                    if let Some(expression) = self.parse_non_primary_expression()? {
                        let mut args = vec![expression];

                        while let Some(expression) = self.parse_non_primary_expression()? {
                            args.push(expression);
                        }

                        self.push_node(Node::Call {
                            function: id_index,
                            args,
                        })?
                    } else {
                        id_index
                    }
                }
                Some(Token::ParenOpen) | Some(Token::ListStart) | Some(Token::Dot) => {
                    self.parse_lookup(constant_index)?
                }
                _ => self.push_node(Node::Id(constant_index))?,
            };

            Ok(Some(result))
        } else {
            Ok(None)
        }
    }

    fn parse_lookup(&mut self, id: ConstantIndex) -> Result<AstIndex, ParserError> {
        let mut lookup = Vec::new();

        lookup.push(LookupNode::Id(id));

        loop {
            match self.peek_token() {
                Some(Token::ParenOpen) => {
                    self.consume_token();

                    let mut args = Vec::new();

                    while let Some(expression) = self.parse_primary_expression()? {
                        args.push(expression);
                    }

                    if let Some(Token::ParenClose) = self.peek_token() {
                        self.consume_token();
                        lookup.push(LookupNode::Call(args));
                    } else {
                        return syntax_error!(ExpectedCallArgsEnd, self);
                    }
                }
                Some(Token::ListStart) => {
                    self.consume_token();

                    let index_expression = if let Some(index_expression) =
                        self.parse_non_primary_expression()?
                    {
                        match self.peek_token() {
                            Some(Token::Range) => {
                                self.consume_token();

                                if let Some(end_expression) = self.parse_non_primary_expression()? {
                                    self.push_node(Node::Range {
                                        start: index_expression,
                                        end: end_expression,
                                        inclusive: false,
                                    })?
                                } else {
                                    self.push_node(Node::RangeFrom {
                                        start: index_expression,
                                    })?
                                }
                            }
                            Some(Token::RangeInclusive) => {
                                self.consume_token();

                                if let Some(end_expression) = self.parse_non_primary_expression()? {
                                    self.push_node(Node::Range {
                                        start: index_expression,
                                        end: end_expression,
                                        inclusive: true,
                                    })?
                                } else {
                                    self.push_node(Node::RangeFrom {
                                        start: index_expression,
                                    })?
                                }
                            }
                            _ => index_expression,
                        }
                    } else {
                        match self.skip_whitespace_and_peek() {
                            Some(Token::Range) => {
                                self.consume_token();

                                if let Some(end_expression) = self.parse_non_primary_expression()? {
                                    self.push_node(Node::RangeTo {
                                        end: end_expression,
                                        inclusive: false,
                                    })?
                                } else {
                                    self.push_node(Node::RangeFull)?
                                }
                            }
                            Some(Token::RangeInclusive) => {
                                self.consume_token();

                                if let Some(end_expression) = self.parse_non_primary_expression()? {
                                    self.push_node(Node::RangeTo {
                                        end: end_expression,
                                        inclusive: true,
                                    })?
                                } else {
                                    self.push_node(Node::RangeFull)?
                                }
                            }
                            _ => return syntax_error!(ExpectedIndexExpression, self),
                        }
                    };

                    if let Some(Token::ListEnd) = self.skip_whitespace_and_peek() {
                        self.consume_token();
                        lookup.push(LookupNode::Index(index_expression));
                    } else {
                        return syntax_error!(ExpectedIndexEnd, self);
                    }
                }
                Some(Token::Dot) => {
                    self.consume_token();

                    if let Some(id_index) = self.parse_id() {
                        lookup.push(LookupNode::Id(id_index));
                    } else {
                        return syntax_error!(ExpectedMapKey, self);
                    }
                }
                _ => break,
            }
        }

        Ok(self.push_node(Node::Lookup(lookup))?)
    }

    fn parse_range(&mut self, lhs: Option<AstIndex>) -> Result<Option<AstIndex>, ParserError> {
        use Node::{Range, RangeFrom, RangeFull, RangeTo};

        let inclusive = match self.peek_token() {
            Some(Token::Range) => false,
            Some(Token::RangeInclusive) => true,
            _ => return internal_error!(RangeParseFailure, self),
        };

        self.consume_token();

        let rhs = self.parse_term(false)?;

        let node = match (lhs, rhs) {
            (Some(start), Some(end)) => Range {
                start,
                end,
                inclusive,
            },
            (Some(start), None) => RangeFrom { start },
            (None, Some(end)) => RangeTo { end, inclusive },
            (None, None) => RangeFull,
        };

        return Ok(Some(self.push_node(node)?));
    }

    fn parse_term(&mut self, primary_expression: bool) -> Result<Option<AstIndex>, ParserError> {
        use Node::*;

        let current_indent = self.lexer.current_indent();

        if let Some(token) = self.skip_whitespace_and_peek() {
            let result = match token {
                Token::True => {
                    self.consume_token();
                    self.push_node(BoolTrue)?
                }
                Token::False => {
                    self.consume_token();
                    self.push_node(BoolFalse)?
                }
                Token::ParenOpen => {
                    self.consume_token();

                    let expression = if let Some(expression) = self.parse_primary_expression()? {
                        expression
                    } else {
                        self.push_node(Empty)?
                    };

                    if let Some(Token::ParenClose) = self.peek_token() {
                        self.consume_token();
                        expression
                    } else {
                        return syntax_error!(ExpectedCloseParen, self);
                    }
                }
                Token::Number => {
                    self.consume_token();
                    match f64::from_str(self.lexer.slice()) {
                        Ok(n) => {
                            if n == 0.0 {
                                self.push_node(Number0)?
                            } else if n == 1.0 {
                                self.push_node(Number1)?
                            } else {
                                let constant_index = self.constants.add_f64(n) as u32;
                                self.push_node(Number(constant_index))?
                            }
                        }
                        Err(_) => {
                            return internal_error!(NumberParseFailure, self);
                        }
                    }
                }
                Token::Str => {
                    self.consume_token();
                    let s = trim_str(self.lexer.slice(), 1, 1);
                    let constant_index = self.constants.add_string(s) as u32;
                    self.push_node(Str(constant_index))?
                }
                Token::Id => {
                    self.consume_token();
                    let constant_index = self.constants.add_string(self.lexer.slice()) as u32;
                    self.push_node(Id(constant_index))?
                }
                Token::ListStart => {
                    self.consume_token();
                    let mut entries = Vec::new();
                    while let Some(entry) = self.parse_term(false)? {
                        entries.push(entry);
                    }
                    if self.skip_whitespace_and_next() != Some(Token::ListEnd) {
                        return syntax_error!(ExpectedListEnd, self);
                    }
                    self.push_node(List(entries))?
                }
                Token::MapStart => {
                    self.consume_token();
                    let mut entries = Vec::new();

                    loop {
                        if let Some(key) = self.parse_id() {
                            if self.skip_whitespace_and_next() != Some(Token::Colon) {
                                return syntax_error!(ExpectedMapSeparator, self);
                            }

                            if let Some(value) = self.parse_primary_expression()? {
                                entries.push((key, value));
                            } else {
                                return syntax_error!(ExpectedMapValue, self);
                            }

                            if self.skip_whitespace_and_peek() == Some(Token::Separator) {
                                self.consume_token();
                                continue;
                            } else {
                                break;
                            }
                        }
                    }

                    if self.skip_whitespace_and_next() != Some(Token::MapEnd) {
                        return syntax_error!(ExpectedMapEnd, self);
                    }

                    self.push_node(Map(entries))?
                }
                Token::If => return self.parse_if_expression(),
                Token::Function => return self.parse_function(primary_expression),
                Token::NewLineIndented => return self.parse_map_block(current_indent),
                _ => return Ok(None),
            };

            Ok(Some(result))
        } else {
            Ok(None)
        }
    }

    fn parse_map_block(&mut self, current_indent: usize) -> Result<Option<AstIndex>, ParserError> {
        use Token::{Colon, NewLineIndented};

        if self.skip_whitespace_and_peek() != Some(NewLineIndented) {
            return Ok(None);
        }

        let block_indent = self.lexer.next_indent();

        if block_indent <= current_indent {
            return Ok(None);
        }

        self.consume_token();

        let mut entries = Vec::new();

        while let Some(key) = self.parse_id() {
            if self.skip_whitespace_and_next() != Some(Colon) {
                return syntax_error!(ExpectedMapSeparator, self);
            }

            if let Some(value) = self.parse_primary_expression()? {
                entries.push((key, value));
            } else {
                return syntax_error!(ExpectedMapValue, self);
            }

            self.skip_empty_lines_and_peek();

            let next_indent = self.lexer.next_indent();
            if next_indent < block_indent {
                break;
            } else if next_indent > block_indent {
                return syntax_error!(UnexpectedIndentation, self);
            }
        }

        Ok(Some(self.ast.push(Node::Map(entries), Span::default())?))
    }

    fn parse_for_loop(&mut self, body: Option<AstIndex>) -> Result<Option<AstIndex>, ParserError> {
        if self.skip_whitespace_and_peek() != Some(Token::For) {
            return Ok(None);
        }

        let current_indent = self.lexer.current_indent();

        self.consume_token();

        let mut args = Vec::new();
        while let Some(constant_index) = self.parse_id() {
            args.push(constant_index);
            self.frame_mut()?
                .ids_assigned_in_scope
                .insert(constant_index);
            if self.skip_whitespace_and_peek() == Some(Token::Separator) {
                self.consume_token();
            }
        }
        if args.is_empty() {
            return syntax_error!(ExpectedForArgs, self);
        }

        if self.skip_whitespace_and_next() != Some(Token::In) {
            return syntax_error!(ExpectedForInKeyword, self);
        }

        let mut ranges = Vec::new();
        while let Some(range) = self.parse_non_primary_expression()? {
            ranges.push(range);

            if self.skip_whitespace_and_peek() != Some(Token::Separator) {
                break;
            }

            self.consume_token();
        }
        if ranges.is_empty() {
            return syntax_error!(ExpectedForRanges, self);
        }

        let condition = if self.skip_whitespace_and_peek() == Some(Token::If) {
            self.consume_token();
            if let Some(condition) = self.parse_primary_expression()? {
                Some(condition)
            } else {
                return syntax_error!(ExpectedForCondition, self);
            }
        } else {
            None
        };

        let body = if let Some(body) = body {
            body
        } else if let Some(body) = self.parse_indented_block(current_indent)? {
            body
        } else {
            return syntax_error!(ExpectedForBody, self);
        };

        let result = self.push_node(Node::For(AstFor {
            args,
            ranges,
            condition,
            body,
        }))?;

        Ok(Some(result))
    }

    fn parse_while_loop(
        &mut self,
        body: Option<AstIndex>,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.skip_whitespace_and_peek() != Some(Token::While) {
            return Ok(None);
        }

        let current_indent = self.lexer.current_indent();
        self.consume_token();

        let condition = if let Some(condition) = self.parse_primary_expression()? {
            condition
        } else {
            return syntax_error!(ExpectedWhileCondition, self);
        };

        let body = if let Some(body) = body {
            body
        } else if let Some(body) = self.parse_indented_block(current_indent)? {
            body
        } else {
            return syntax_error!(ExpectedWhileBody, self);
        };

        let result = self.push_node(Node::While { condition, body })?;
        Ok(Some(result))
    }

    fn parse_until_loop(
        &mut self,
        body: Option<AstIndex>,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.skip_whitespace_and_peek() != Some(Token::Until) {
            return Ok(None);
        }

        let current_indent = self.lexer.current_indent();
        self.consume_token();

        let condition = if let Some(condition) = self.parse_primary_expression()? {
            condition
        } else {
            return syntax_error!(ExpectedUntilCondition, self);
        };

        let body = if let Some(body) = body {
            body
        } else if let Some(body) = self.parse_indented_block(current_indent)? {
            body
        } else {
            return syntax_error!(ExpectedUntilBody, self);
        };

        let result = self.push_node(Node::Until { condition, body })?;
        Ok(Some(result))
    }

    fn parse_if_expression(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_token() != Some(Token::If) {
            return Ok(None);
        }

        let current_indent = self.lexer.current_indent();

        self.consume_token();
        let condition = match self.parse_primary_expression()? {
            Some(condition) => condition,
            None => return syntax_error!(ExpectedIfCondition, self),
        };

        let result = if self.skip_whitespace_and_peek() == Some(Token::Then) {
            self.consume_token();
            let then_node = match self.parse_primary_expression()? {
                Some(then_node) => then_node,
                None => return syntax_error!(ExpectedThenExpression, self),
            };
            let else_node = if self.skip_whitespace_and_peek() == Some(Token::Else) {
                self.consume_token();
                match self.parse_primary_expression()? {
                    Some(else_node) => Some(else_node),
                    None => return syntax_error!(ExpectedElseExpression, self),
                }
            } else {
                None
            };

            self.push_node(Node::If(AstIf {
                condition,
                then_node,
                else_if_blocks: vec![],
                else_node,
            }))?
        } else if let Some(then_node) = self.parse_indented_block(current_indent)? {
            let mut else_if_blocks = Vec::new();

            while self.lexer.current_indent() == current_indent {
                if let Some(Token::ElseIf) = self.skip_whitespace_and_peek() {
                    self.consume_token();
                    if let Some(else_if_condition) = self.parse_primary_expression()? {
                        if let Some(else_if_block) = self.parse_indented_block(current_indent)? {
                            else_if_blocks.push((else_if_condition, else_if_block));
                        } else {
                            return syntax_error!(ExpectedElseIfBlock, self);
                        }
                    } else {
                        return syntax_error!(ExpectedElseIfCondition, self);
                    }
                } else {
                    break;
                }
            }

            let else_node = if self.lexer.current_indent() == current_indent {
                if let Some(Token::Else) = self.skip_whitespace_and_peek() {
                    self.consume_token();
                    if let Some(else_block) = self.parse_indented_block(current_indent)? {
                        Some(else_block)
                    } else {
                        return syntax_error!(ExpectedElseBlock, self);
                    }
                } else {
                    None
                }
            } else {
                None
            };

            self.push_node(Node::If(AstIf {
                condition,
                then_node,
                else_if_blocks,
                else_node,
            }))?
        } else {
            return syntax_error!(ExpectedThenKeywordOrBlock, self);
        };

        Ok(Some(result))
    }

    fn parse_indented_block(
        &mut self,
        current_indent: usize,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.skip_whitespace_and_peek() != Some(Token::NewLineIndented) {
            return Ok(None);
        }

        self.consume_token();
        let block_indent = self.lexer.current_indent();

        if block_indent <= current_indent {
            return Ok(None);
        }

        let mut body = Vec::new();
        while let Some(expression) = self.parse_line()? {
            body.push(expression);

            self.skip_empty_lines_and_peek();

            let next_indent = self.lexer.next_indent();
            if next_indent < block_indent {
                break;
            } else if next_indent > block_indent {
                return syntax_error!(UnexpectedIndentation, self);
            }
        }

        // If the body is a single expression then it doesn't need to be wrapped in a block
        if body.len() == 1 {
            Ok(Some(*body.first().unwrap()))
        } else {
            Ok(Some(self.ast.push(Node::Block(body), Span::default())?))
        }
    }

    fn push_ast_op(
        &mut self,
        op: Token,
        lhs: AstIndex,
        rhs: AstIndex,
    ) -> Result<AstIndex, ParserError> {
        use Token::*;
        let ast_op = match op {
            Add => AstOp::Add,
            Subtract => AstOp::Subtract,
            Multiply => AstOp::Multiply,
            Divide => AstOp::Divide,
            Modulo => AstOp::Modulo,

            Equal => AstOp::Equal,
            NotEqual => AstOp::NotEqual,

            Greater => AstOp::Greater,
            GreaterOrEqual => AstOp::GreaterOrEqual,
            Less => AstOp::Less,
            LessOrEqual => AstOp::LessOrEqual,

            And => AstOp::And,
            Or => AstOp::Or,

            _ => unreachable!(),
        };
        self.push_node(Node::Op {
            op: ast_op,
            lhs,
            rhs,
        })
    }

    fn peek_token(&mut self) -> Option<Token> {
        self.lexer.peek()
    }

    fn consume_token(&mut self) -> Option<Token> {
        self.lexer.next()
    }

    fn push_node(&mut self, node: Node) -> Result<AstIndex, ParserError> {
        let extras = self.lexer.extras();
        self.ast.push(
            node,
            make_span(
                self.lexer.source(),
                extras.line_number,
                extras.line_start,
                &self.lexer.span(),
            ),
        )
    }

    fn skip_empty_lines_and_peek(&mut self) -> Option<Token> {
        loop {
            let peeked = self.peek_token();

            match peeked {
                Some(Token::Whitespace) => {}
                Some(Token::NewLine) => {}
                Some(Token::NewLineIndented) => {}
                Some(token) => return Some(token),
                None => return None,
            }

            self.lexer.next();
            continue;
        }
    }

    fn skip_whitespace_and_peek(&mut self) -> Option<Token> {
        loop {
            let peeked = self.peek_token();

            match peeked {
                Some(Token::Whitespace) => {}
                Some(token) => return Some(token),
                None => return None,
            }

            self.lexer.next();
            continue;
        }
    }

    fn skip_whitespace_and_next(&mut self) -> Option<Token> {
        loop {
            let peeked = self.peek_token();

            match peeked {
                Some(Token::Whitespace) => {}
                Some(_) => return self.lexer.next(),
                None => return None,
            }

            self.lexer.next();
            continue;
        }
    }
}

fn operator_precedence(op: Token) -> Option<u8> {
    use Token::*;
    let priority = match op {
        Or => 1,
        And => 2,
        Equal | NotEqual => 3,
        Greater | GreaterOrEqual | Less | LessOrEqual => 4,
        Add | Subtract => 5,
        Multiply | Divide | Modulo => 6,
        _ => return None,
    };
    Some(priority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use {crate::constant_pool::Constant, Node::*};

    fn check_ast(source: &str, expected_ast: &[Node], expected_constants: Option<&[Constant]>) {
        println!("{}", source);

        let mut constants = ConstantPool::default();
        match Parser::parse(source, &mut constants) {
            Ok(ast) => {
                for (i, (ast_node, expected_node)) in
                    ast.nodes().iter().zip(expected_ast.iter()).enumerate()
                {
                    assert_eq!(ast_node.node, *expected_node, "Mismatch at position {}", i);
                }
                assert_eq!(
                    ast.nodes().len(),
                    expected_ast.len(),
                    "Node list length mismatch"
                );

                if let Some(expected_constants) = expected_constants {
                    for (constant, expected_constant) in
                        constants.iter().zip(expected_constants.iter())
                    {
                        assert_eq!(constant, *expected_constant);
                    }
                    assert_eq!(
                        constants.len(),
                        expected_constants.len(),
                        "Constant list length mismatch"
                    );
                }
            }
            Err(error) => panic!("{}", error),
        }
    }

    mod values {
        use super::*;

        #[test]
        fn literals() {
            let source = "\
true
false
1
1.5
\"hello\"
a
()";
            check_ast(
                source,
                &[
                    BoolTrue,
                    BoolFalse,
                    Number1,
                    Number(0),
                    Str(1),
                    Id(2),
                    Empty,
                    MainBlock {
                        body: vec![0, 1, 2, 3, 4, 5, 6],
                        local_count: 0,
                    },
                ],
                Some(&[
                    Constant::Number(1.5),
                    Constant::Str("hello"),
                    Constant::Str("a"),
                ]),
            )
        }

        #[test]
        fn list() {
            let source = "[0 n \"test\" n -1]";
            check_ast(
                source,
                &[
                    Number0,
                    Id(0),
                    Str(1),
                    Id(0),
                    Number(2),
                    List(vec![0, 1, 2, 3, 4]),
                    MainBlock {
                        body: vec![5],
                        local_count: 0,
                    },
                ],
                Some(&[
                    Constant::Str("n"),
                    Constant::Str("test"),
                    Constant::Number(-1.0),
                ]),
            )
        }

        #[test]
        fn map_inline() {
            let source = "{foo: 42, bar: \"hello\"}";
            check_ast(
                source,
                &[
                    Number(1),
                    Str(3),
                    Map(vec![(0, 0), (2, 1)]), // map entries are constant/ast index pairs
                    MainBlock {
                        body: vec![2],
                        local_count: 0,
                    },
                ],
                Some(&[
                    Constant::Str("foo"),
                    Constant::Number(42.0),
                    Constant::Str("bar"),
                    Constant::Str("hello"),
                ]),
            )
        }

        #[test]
        fn map_block() {
            let source = "\
x =
  foo: 42
  bar: \"hello\"
  baz:
    foo: 0
x";
            check_ast(
                source,
                &[
                    Id(0),     // x
                    Number(2), // 42
                    Str(4),    // "hello"
                    Number0,
                    Map(vec![(1, 3)]),                 // baz nested map
                    Map(vec![(1, 1), (3, 2), (5, 4)]), // 5 - map entries are constant/ast pairs
                    Assign {
                        target: AssignTarget {
                            target_index: 0,
                            scope: Scope::Local,
                        },
                        expression: 5,
                    },
                    Id(0),
                    MainBlock {
                        body: vec![6, 7],
                        local_count: 1,
                    },
                ],
                Some(&[
                    Constant::Str("x"),
                    Constant::Str("foo"),
                    Constant::Number(42.0),
                    Constant::Str("bar"),
                    Constant::Str("hello"),
                    Constant::Str("baz"),
                ]),
            )
        }

        #[test]
        fn ranges() {
            let source = "\
0..1
0..=1
(0 + 1)..(1 + 1)";
            check_ast(
                source,
                &[
                    Number0,
                    Number1,
                    Range {
                        start: 0,
                        end: 1,
                        inclusive: false,
                    },
                    Number0,
                    Number1,
                    Range {
                        start: 3,
                        end: 4,
                        inclusive: true,
                    }, // 5
                    Number0,
                    Number1,
                    Op {
                        op: AstOp::Add,
                        lhs: 6,
                        rhs: 7,
                    },
                    Number1,
                    Number1, // 10
                    Op {
                        op: AstOp::Add,
                        lhs: 9,
                        rhs: 10,
                    },
                    Range {
                        start: 8,
                        end: 11,
                        inclusive: false,
                    },
                    MainBlock {
                        body: vec![2, 5, 12],
                        local_count: 0,
                    },
                ],
                None,
            )
        }

        #[test]
        fn multiple_expressions() {
            let source = "0, 1, 0";
            check_ast(
                source,
                &[
                    Number0,
                    Number1,
                    Number0,
                    Expressions(vec![0, 1, 2]),
                    MainBlock {
                        body: vec![3],
                        local_count: 0,
                    },
                ],
                None,
            )
        }
    }

    mod assignment {
        use super::*;
        use crate::node::{AssignTarget, Scope};

        #[test]
        fn single() {
            let source = "a = 1";
            check_ast(
                source,
                &[
                    Id(0),
                    Number1,
                    Assign {
                        target: AssignTarget {
                            target_index: 0,
                            scope: Scope::Local,
                        },
                        expression: 1,
                    },
                    MainBlock {
                        body: vec![2],
                        local_count: 1,
                    },
                ],
                Some(&[Constant::Str("a")]),
            )
        }

        #[test]
        fn multi_2_to_1() {
            let source = "x = 1, 0";
            check_ast(
                source,
                &[
                    Id(0),
                    Number1,
                    Number0,
                    Expressions(vec![1, 2]),
                    Assign {
                        target: AssignTarget {
                            target_index: 0,
                            scope: Scope::Local,
                        },
                        expression: 3,
                    },
                    MainBlock {
                        body: vec![4],
                        local_count: 1,
                    },
                ],
                Some(&[Constant::Str("x")]),
            )
        }
    }

    mod arithmetic {
        use super::*;

        #[test]
        fn addition_subtraction() {
            let source = "1 - 0 + 1";
            check_ast(
                source,
                &[
                    Number1,
                    Number0,
                    Number1,
                    Op {
                        op: AstOp::Add,
                        lhs: 1,
                        rhs: 2,
                    },
                    Op {
                        op: AstOp::Subtract,
                        lhs: 0,
                        rhs: 3,
                    },
                    MainBlock {
                        body: vec![4],
                        local_count: 0,
                    },
                ],
                None,
            )
        }

        #[test]
        fn add_multiply() {
            let source = "1 + 0 * 1 + 0";
            check_ast(
                source,
                &[
                    Number1,
                    Number0,
                    Number1,
                    Op {
                        op: AstOp::Multiply,
                        lhs: 1,
                        rhs: 2,
                    },
                    Number0,
                    Op {
                        op: AstOp::Add,
                        lhs: 3,
                        rhs: 4,
                    },
                    Op {
                        op: AstOp::Add,
                        lhs: 0,
                        rhs: 5,
                    },
                    MainBlock {
                        body: vec![6],
                        local_count: 0,
                    },
                ],
                None,
            )
        }

        #[test]
        fn with_parentheses() {
            let source = "(1 + 0) * (1 + 0)";
            check_ast(
                source,
                &[
                    Number1,
                    Number0,
                    Op {
                        op: AstOp::Add,
                        lhs: 0,
                        rhs: 1,
                    },
                    Number1,
                    Number0,
                    Op {
                        op: AstOp::Add,
                        lhs: 3,
                        rhs: 4,
                    },
                    Op {
                        op: AstOp::Multiply,
                        lhs: 2,
                        rhs: 5,
                    },
                    MainBlock {
                        body: vec![6],
                        local_count: 0,
                    },
                ],
                None,
            )
        }

        #[test]
        fn logic() {
            let source = "0 < 1 and 1 > 0 or true";
            check_ast(
                source,
                &[
                    Number0,
                    Number1,
                    Op {
                        op: AstOp::Less,
                        lhs: 0,
                        rhs: 1,
                    },
                    Number1,
                    Number0,
                    Op {
                        op: AstOp::Greater,
                        lhs: 3,
                        rhs: 4,
                    },
                    Op {
                        op: AstOp::And,
                        lhs: 2,
                        rhs: 5,
                    },
                    BoolTrue,
                    Op {
                        op: AstOp::Or,
                        lhs: 6,
                        rhs: 7,
                    },
                    MainBlock {
                        body: vec![8],
                        local_count: 0,
                    },
                ],
                None,
            )
        }

        #[test]
        fn string_and_id() {
            let source = "\"hello\" + x";
            check_ast(
                source,
                &[
                    Str(0),
                    Id(1),
                    Op {
                        op: AstOp::Add,
                        lhs: 0,
                        rhs: 1,
                    },
                    MainBlock {
                        body: vec![2],
                        local_count: 0,
                    },
                ],
                Some(&[Constant::Str("hello"), Constant::Str("x")]),
            )
        }
    }

    mod control_flow {
        use super::*;

        #[test]
        fn if_inline() {
            let source = "1 + if true then 0 else 1";
            check_ast(
                source,
                &[
                    Number1,
                    BoolTrue,
                    Number0,
                    Number1,
                    If(AstIf {
                        condition: 1,
                        then_node: 2,
                        else_if_blocks: vec![],
                        else_node: Some(3),
                    }),
                    Op {
                        op: AstOp::Add,
                        lhs: 0,
                        rhs: 4,
                    },
                    MainBlock {
                        body: vec![5],
                        local_count: 0,
                    },
                ],
                None,
            )
        }

        #[test]
        fn if_block() {
            let source = "\
a = if false
  0
elseif true
  1
elseif false
  0
else
  1
a";
            check_ast(
                source,
                &[
                    Id(0),
                    BoolFalse,
                    Number0,
                    BoolTrue,
                    Number1,
                    BoolFalse, // 5
                    Number0,
                    Number1,
                    If(AstIf {
                        condition: 1,
                        then_node: 2,
                        else_if_blocks: vec![(3, 4), (5, 6)],
                        else_node: Some(7),
                    }),
                    Assign {
                        target: AssignTarget {
                            target_index: 0,
                            scope: Scope::Local,
                        },
                        expression: 8,
                    },
                    Id(0),
                    MainBlock {
                        body: vec![9, 10],
                        local_count: 1,
                    }, // 10
                ],
                None,
            )
        }
    }

    mod loops {
        use super::*;

        #[test]
        fn for_inline() {
            let source = "x for x in 0..1";
            check_ast(
                source,
                &[
                    Id(0),
                    Number0,
                    Number1,
                    Range {
                        start: 1,
                        end: 2,
                        inclusive: false,
                    },
                    For(AstFor {
                        args: vec![0],
                        ranges: vec![3],
                        condition: None,
                        body: 0,
                    }),
                    MainBlock {
                        body: vec![4],
                        local_count: 1,
                    },
                ],
                Some(&[Constant::Str("x")]),
            )
        }

        #[test]
        fn for_inline_conditional() {
            let source = "x for x in y if x == 0";
            check_ast(
                source,
                &[
                    Id(0),
                    Id(1),
                    Id(0),
                    Number0,
                    Op {
                        op: AstOp::Equal,
                        lhs: 2,
                        rhs: 3,
                    },
                    For(AstFor {
                        args: vec![0],
                        ranges: vec![1],
                        condition: Some(4),
                        body: 0,
                    }), // 5
                    MainBlock {
                        body: vec![5],
                        local_count: 1,
                    },
                ],
                Some(&[Constant::Str("x"), Constant::Str("y")]),
            )
        }

        #[test]
        fn for_block() {
            let source = "\
for x in y if x > 0
  f x";
            check_ast(
                source,
                &[
                    Id(1),
                    Id(0),
                    Number0,
                    Op {
                        op: AstOp::Greater,
                        lhs: 1,
                        rhs: 2,
                    },
                    Id(2),
                    Id(0), // 5
                    Call {
                        function: 4,
                        args: vec![5],
                    },
                    For(AstFor {
                        args: vec![0],   // constant 0
                        ranges: vec![0], // ast 0
                        condition: Some(3),
                        body: 6,
                    }),
                    MainBlock {
                        body: vec![7],
                        local_count: 1,
                    },
                ],
                Some(&[Constant::Str("x"), Constant::Str("y"), Constant::Str("f")]),
            )
        }

        #[test]
        fn while_inline() {
            let source = "x while true";
            check_ast(
                source,
                &[
                    Id(0),
                    BoolTrue,
                    While {
                        condition: 1,
                        body: 0,
                    },
                    MainBlock {
                        body: vec![2],
                        local_count: 0,
                    },
                ],
                Some(&[Constant::Str("x")]),
            )
        }

        #[test]
        fn until_inline() {
            let source = "y until false";
            check_ast(
                source,
                &[
                    Id(0),
                    BoolFalse,
                    Until {
                        condition: 1,
                        body: 0,
                    },
                    MainBlock {
                        body: vec![2],
                        local_count: 0,
                    },
                ],
                Some(&[Constant::Str("y")]),
            )
        }

        #[test]
        fn while_block() {
            let source = "\
while x > y
  f x";
            check_ast(
                source,
                &[
                    Id(0),
                    Id(1),
                    Op {
                        op: AstOp::Greater,
                        lhs: 0,
                        rhs: 1,
                    },
                    Id(2),
                    Id(0),
                    Call {
                        function: 3,
                        args: vec![4],
                    }, // 5
                    While {
                        condition: 2,
                        body: 5,
                    },
                    MainBlock {
                        body: vec![6],
                        local_count: 0,
                    },
                ],
                Some(&[Constant::Str("x"), Constant::Str("y"), Constant::Str("f")]),
            )
        }

        #[test]
        fn until_block() {
            let source = "\
until x < y
  f y";
            check_ast(
                source,
                &[
                    Id(0),
                    Id(1),
                    Op {
                        op: AstOp::Less,
                        lhs: 0,
                        rhs: 1,
                    },
                    Id(2),
                    Id(1),
                    Call {
                        function: 3,
                        args: vec![4],
                    }, // 5
                    Until {
                        condition: 2,
                        body: 5,
                    },
                    MainBlock {
                        body: vec![6],
                        local_count: 0,
                    },
                ],
                Some(&[Constant::Str("x"), Constant::Str("y"), Constant::Str("f")]),
            )
        }
    }

    mod functions {
        use super::*;
        use crate::node::{AssignTarget, Scope};

        #[test]
        fn inline_no_args() {
            let source = "a = || 42";
            check_ast(
                source,
                &[
                    Id(0),
                    Number(1),
                    Function(Function {
                        args: vec![],
                        captures: vec![],
                        local_count: 0,
                        body: 1,
                        is_instance_function: false,
                    }),
                    Assign {
                        target: AssignTarget {
                            target_index: 0,
                            scope: Scope::Local,
                        },
                        expression: 2,
                    },
                    MainBlock {
                        body: vec![3],
                        local_count: 1,
                    },
                ],
                Some(&[Constant::Str("a"), Constant::Number(42.0)]),
            )
        }

        #[test]
        fn inline_two_args() {
            let source = "|x y| x + y";
            check_ast(
                source,
                &[
                    Id(0),
                    Id(1),
                    Op {
                        op: AstOp::Add,
                        lhs: 0,
                        rhs: 1,
                    },
                    Function(Function {
                        args: vec![0, 1],
                        captures: vec![],
                        local_count: 2,
                        body: 2,
                        is_instance_function: false,
                    }),
                    MainBlock {
                        body: vec![3],
                        local_count: 0,
                    },
                ],
                Some(&[Constant::Str("x"), Constant::Str("y")]),
            )
        }

        #[test]
        fn with_body() {
            let source = "\
f = |x|
  y = x
  y = y + 1
  y
f 42";
            check_ast(
                source,
                &[
                    Id(0), // f
                    Id(2), // y
                    Id(1), // x
                    Assign {
                        target: AssignTarget {
                            target_index: 1,
                            scope: Scope::Local,
                        },
                        expression: 2,
                    },
                    Id(2), // y
                    Id(2), // y // 5
                    Number1,
                    Op {
                        op: AstOp::Add,
                        lhs: 5,
                        rhs: 6,
                    },
                    Assign {
                        target: AssignTarget {
                            target_index: 4,
                            scope: Scope::Local,
                        },
                        expression: 7,
                    },
                    Id(2),                // y
                    Block(vec![3, 8, 9]), // 10
                    Function(Function {
                        args: vec![1],
                        captures: vec![],
                        local_count: 2,
                        body: 10,
                        is_instance_function: false,
                    }),
                    Assign {
                        target: AssignTarget {
                            target_index: 0,
                            scope: Scope::Local,
                        },
                        expression: 11,
                    },
                    Id(0),
                    Number(3),
                    Call {
                        function: 13,
                        args: vec![14],
                    }, // 15
                    MainBlock {
                        body: vec![12, 15],
                        local_count: 1,
                    },
                ],
                Some(&[
                    Constant::Str("f"),
                    Constant::Str("x"),
                    Constant::Str("y"),
                    Constant::Number(42.0),
                ]),
            )
        }

        #[test]
        fn with_body_nested() {
            let source = "\
f = |x|
  y = |z|
    z
  y x
f 42";
            check_ast(
                source,
                &[
                    Id(0), // f
                    Id(2), // y
                    Id(3), // z
                    Function(Function {
                        args: vec![3],
                        captures: vec![],
                        local_count: 1,
                        body: 2,
                        is_instance_function: false,
                    }),
                    Assign {
                        target: AssignTarget {
                            target_index: 1,
                            scope: Scope::Local,
                        },
                        expression: 3,
                    },
                    Id(2), // y // 5
                    Id(1), // x
                    Call {
                        function: 5,
                        args: vec![6],
                    },
                    Block(vec![4, 7]),
                    Function(Function {
                        args: vec![1],
                        captures: vec![],
                        local_count: 2,
                        body: 8,
                        is_instance_function: false,
                    }),
                    Assign {
                        target: AssignTarget {
                            target_index: 0,
                            scope: Scope::Local,
                        },
                        expression: 9,
                    }, // 10
                    Id(0), // f
                    Number(4),
                    Call {
                        function: 11,
                        args: vec![12],
                    },
                    MainBlock {
                        body: vec![10, 13],
                        local_count: 1,
                    },
                ],
                Some(&[
                    Constant::Str("f"),
                    Constant::Str("x"),
                    Constant::Str("y"),
                    Constant::Str("z"),
                    Constant::Number(42.0),
                ]),
            )
        }
    }

    mod lookups {
        use super::*;

        #[test]
        fn array_indexing() {
            let source = "\
a[0] = a[1]
x[..]
y[..3]
z[10..][0]";
            check_ast(
                source,
                &[
                    Number0,
                    Lookup(vec![LookupNode::Id(0), LookupNode::Index(0)]),
                    Number1,
                    Lookup(vec![LookupNode::Id(0), LookupNode::Index(2)]),
                    Assign {
                        target: AssignTarget {
                            target_index: 1,
                            scope: Scope::Local,
                        },
                        expression: 3,
                    },
                    RangeFull, // 5
                    Lookup(vec![LookupNode::Id(1), LookupNode::Index(5)]),
                    Number(3),
                    RangeTo {
                        end: 7,
                        inclusive: false,
                    },
                    Lookup(vec![LookupNode::Id(2), LookupNode::Index(8)]),
                    Number(5), // 10
                    RangeFrom { start: 10 },
                    Number0,
                    Lookup(vec![
                        LookupNode::Id(4),
                        LookupNode::Index(11),
                        LookupNode::Index(12),
                    ]),
                    MainBlock {
                        body: vec![4, 6, 9, 13],
                        local_count: 1,
                    },
                ],
                Some(&[
                    Constant::Str("a"),
                    Constant::Str("x"),
                    Constant::Str("y"),
                    Constant::Number(3.0),
                    Constant::Str("z"),
                    Constant::Number(10.0),
                ]),
            )
        }

        #[test]
        fn map_lookup() {
            let source = "\
x.foo
x.bar()
x.bar().baz = 1";
            check_ast(
                source,
                &[
                    Lookup(vec![LookupNode::Id(0), LookupNode::Id(1)]),
                    Lookup(vec![
                        LookupNode::Id(0),
                        LookupNode::Id(2),
                        LookupNode::Call(vec![]),
                    ]),
                    Lookup(vec![
                        LookupNode::Id(0),
                        LookupNode::Id(2),
                        LookupNode::Call(vec![]),
                        LookupNode::Id(3),
                    ]),
                    Number1,
                    Assign {
                        target: AssignTarget {
                            target_index: 2,
                            scope: Scope::Local,
                        },
                        expression: 3,
                    },
                    MainBlock {
                        body: vec![0, 1, 4],
                        local_count: 1,
                    },
                ],
                Some(&[
                    Constant::Str("x"),
                    Constant::Str("foo"),
                    Constant::Str("bar"),
                    Constant::Str("baz"),
                ]),
            )
        }
    }
}
