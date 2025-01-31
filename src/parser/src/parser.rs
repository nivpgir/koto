#![cfg_attr(feature = "panic_on_parser_error", allow(unreachable_code))]

use {
    crate::{constant_pool::ConstantPoolBuilder, error::*, *},
    koto_lexer::{Lexer, Span, Token},
    std::{collections::HashSet, iter::FromIterator, str::FromStr},
};

macro_rules! make_internal_error {
    ($error:ident, $parser:expr) => {{
        ParserError::new(InternalError::$error.into(), $parser.current_span())
    }};
}

macro_rules! internal_error {
    ($error:ident, $parser:expr) => {{
        let error = make_internal_error!($error, $parser);

        #[cfg(feature = "panic_on_parser_error")]
        panic!("{}", error);

        #[cfg(not(feature = "panic_on_parser_error"))]
        Err(error)
    }};
}

macro_rules! parser_error {
    ($error:ident, $parser:expr, $error_type:ident) => {{
        let error = ParserError::new($error_type::$error.into(), $parser.current_span());

        #[cfg(feature = "panic_on_parser_error")]
        panic!("{}", error);

        #[cfg(not(feature = "panic_on_parser_error"))]
        Err(error)
    }};
}

macro_rules! indentation_error {
    ($error:ident, $parser:expr) => {{
        parser_error!($error, $parser, ExpectedIndentation)
    }};
}

macro_rules! syntax_error {
    ($error:ident, $parser:expr) => {{
        parser_error!($error, $parser, SyntaxError)
    }};
}

enum ConstantIndexOrWildcard {
    Index(ConstantIndex),
    Wildcard,
}

#[derive(Debug, Default)]
struct Frame {
    // If a frame contains yield then it represents a generator function
    contains_yield: bool,
    // IDs that have been assigned within the current frame
    ids_assigned_in_scope: HashSet<ConstantIndex>,
    // IDs and lookup roots which have been accessed without being locally assigned previously
    accessed_non_locals: HashSet<ConstantIndex>,
    // While an expression is being parsed we keep track of lhs assignments and rhs accesses.
    // At the end of the expresson (see `finish_expression`) accessed IDs that aren't locally
    // assigned are then counted as non-local accesses.
    pending_accesses: HashSet<ConstantIndex>,
    pending_assignments: HashSet<ConstantIndex>,
}

impl Frame {
    fn local_count(&self) -> usize {
        self.ids_assigned_in_scope.len()
    }

    // Non-locals accessed in a nested frame need to be declared as also accessed in this
    // frame. This ensures that captures from the outer frame will be available when
    // creating the nested inner scope.
    fn add_nested_accessed_non_locals(&mut self, nested_frame: &Frame) {
        for non_local in nested_frame.accessed_non_locals.iter() {
            if !self.pending_assignments.contains(non_local) {
                self.add_id_access(*non_local);
            }
        }
    }

    fn add_id_access(&mut self, id: ConstantIndex) {
        self.pending_accesses.insert(id);
    }

    fn remove_id_access(&mut self, id: ConstantIndex) {
        self.pending_accesses.remove(&id);
    }

    fn add_id_assignment(&mut self, id: ConstantIndex) {
        self.pending_assignments.insert(id);
    }

    fn finish_expression(&mut self) {
        for id in self.pending_accesses.drain() {
            if !self.ids_assigned_in_scope.contains(&id) {
                self.accessed_non_locals.insert(id);
            }
        }

        self.ids_assigned_in_scope
            .extend(self.pending_assignments.drain());
    }
}

#[derive(Clone, Copy, Debug)]
struct ExpressionContext {
    // e.g.
    //
    // match x
    //   foo.bar if x == 0 then...
    //
    // Without the flag, `if f == 0...` would be parsed as being an argument for a call to foo.bar.
    allow_space_separated_call: bool,
    // e.g. f = |x|
    //        x + x
    // This function can have an indented body.
    //
    // foo
    //   bar,
    //   baz
    // This function call can be broken over lines.
    //
    // while x < f y
    //   ...
    // Here, `f y` can't be broken over lines as the while expression expects an indented block.
    allow_linebreaks: bool,
    // When true, a map block is allowed in the current context.
    // e.g.
    //
    // x = foo: 42
    //        ^~~ A map block requires an indented block, so here the flag should be false
    //
    // return
    //   foo: 42
    //      ^~~ A colon following the foo identifier signifies the start of a map block.
    //          Consuming tokens through the indentation sets the flag to true,
    //          see consume_until_next_token()
    //
    // x = ||
    //   foo: 42
    //      ^~~ The first line in an indented block will have the flag set to true to allow the
    //          block to be parsed as a map, see parse_indented_block().
    allow_map_block: bool,
    // - Greater: indentation is required for an expression to be continued on a following line.
    // - Equal: indentation should match the expected indentation.
    // - GreaterOrEqual: indentation should be at or above the expected indentation.
    expected_indentation: Indentation,
}

#[derive(Clone, Copy, Debug)]
enum Indentation {
    Greater,
    GreaterOrEqual(usize),
    Equal(usize),
}

impl Indentation {
    // Converts fixed 'equal' indentation into 'greater or equal' indentation
    fn greater_or_equal(&self) -> Self {
        use Indentation::*;
        match *self {
            Equal(indent) => GreaterOrEqual(indent),
            other => other,
        }
    }
}

impl ExpressionContext {
    fn permissive() -> Self {
        Self {
            allow_space_separated_call: true,
            allow_linebreaks: true,
            allow_map_block: false,
            expected_indentation: Indentation::Greater,
        }
    }

    fn restricted() -> Self {
        Self {
            allow_space_separated_call: false,
            allow_linebreaks: false,
            allow_map_block: false,
            expected_indentation: Indentation::Greater,
        }
    }

    fn inline() -> Self {
        Self {
            allow_space_separated_call: true,
            allow_linebreaks: false,
            allow_map_block: false,
            expected_indentation: Indentation::Greater,
        }
    }

    fn start_new_expression(&self) -> Self {
        Self {
            allow_space_separated_call: true,
            allow_linebreaks: self.allow_linebreaks,
            allow_map_block: false,
            expected_indentation: Indentation::Greater,
        }
    }
}

/// Koto's parser
pub struct Parser<'source> {
    ast: Ast,
    constants: ConstantPoolBuilder,
    lexer: Lexer<'source>,
    frame_stack: Vec<Frame>,
}

impl<'source> Parser<'source> {
    /// Takes in a source script, and produces an Ast
    pub fn parse(source: &'source str) -> Result<Ast, ParserError> {
        let capacity_guess = source.len() / 4;
        let mut parser = Parser {
            ast: Ast::with_capacity(capacity_guess),
            constants: ConstantPoolBuilder::default(),
            lexer: Lexer::new(source),
            frame_stack: Vec::new(),
        };

        let main_block = parser.parse_main_block()?;
        parser.ast.set_entry_point(main_block);
        parser.ast.set_constants(parser.constants.build());

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
        self.frame_stack.push(Frame::default());

        let start_span = self.current_span();

        let mut context = ExpressionContext::permissive();
        context.expected_indentation = Indentation::Equal(0);

        let mut body = Vec::new();
        while self.peek_next_token(&context).is_some() {
            self.consume_until_next_token(&mut context);

            if let Some(expression) = self.parse_line(&mut ExpressionContext::permissive())? {
                body.push(expression);

                match self.peek_next_token_on_same_line() {
                    Some(Token::NewLine) | Some(Token::NewLineIndented) => continue,
                    None => break,
                    _ => {
                        self.consume_next_token_on_same_line();
                        return syntax_error!(UnexpectedToken, self);
                    }
                }
            } else {
                self.lexer.next();
                return syntax_error!(ExpectedExpressionInMainBlock, self);
            }
        }

        // Check that all tokens were consumed
        self.consume_until_next_token(&mut ExpressionContext::permissive());
        if self.peek_token().is_some() {
            self.consume_token(); // Consume the token so that the error span is correct
            return syntax_error!(UnexpectedToken, self);
        }

        let result = self.push_node_with_start_span(
            Node::MainBlock {
                body,
                local_count: self.frame()?.local_count(),
            },
            start_span,
        )?;

        self.frame_stack.pop();
        Ok(result)
    }

    fn parse_nested_function_args(
        &mut self,
        arg_ids: &mut Vec<ConstantIndex>,
    ) -> Result<Vec<AstIndex>, ParserError> {
        let mut nested_args = Vec::new();

        let mut args_context = ExpressionContext::permissive();
        while self.peek_next_token(&args_context).is_some() {
            self.consume_until_next_token(&mut args_context);
            match self.parse_id_or_wildcard(&mut args_context)? {
                Some(ConstantIndexOrWildcard::Index(constant_index)) => {
                    if self.constants.get_str(constant_index) == "self" {
                        return syntax_error!(SelfArgNotInFirstPosition, self);
                    }

                    arg_ids.push(constant_index);
                    nested_args.push(self.push_node(Node::Id(constant_index))?);
                }
                Some(ConstantIndexOrWildcard::Wildcard) => {
                    nested_args.push(self.push_node(Node::Wildcard)?)
                }
                None => match self.peek_token() {
                    Some(Token::SquareOpen) => {
                        self.consume_token();

                        let list_args = self.parse_nested_function_args(arg_ids)?;
                        nested_args.push(self.push_node(Node::List(list_args))?);

                        if self.consume_next_token(&mut args_context) != Some(Token::SquareClose) {
                            return syntax_error!(ExpectedListEnd, self);
                        }
                    }
                    Some(Token::RoundOpen) => {
                        self.consume_token();

                        let tuple_args = self.parse_nested_function_args(arg_ids)?;
                        nested_args.push(self.push_node(Node::Tuple(tuple_args))?);

                        if self.consume_next_token(&mut args_context) != Some(Token::RoundClose) {
                            return syntax_error!(ExpectedCloseParen, self);
                        }
                    }
                    _ => break,
                },
            }

            if self.peek_next_token_on_same_line() == Some(Token::Comma) {
                self.consume_next_token_on_same_line();
            } else {
                break;
            }
        }

        Ok(nested_args)
    }

    fn parse_function(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let start_indent = self.current_indent();

        if self.consume_next_token_on_same_line() != Some(Token::Function) {
            return internal_error!(FunctionParseFailure, self);
        }

        let span_start = self.current_span().start;

        // Parse function's args
        let mut arg_nodes = Vec::new();
        let mut arg_ids = Vec::new();
        let mut is_instance_function = false;
        let mut is_variadic = false;

        let mut args_context = ExpressionContext::permissive();
        while self.peek_next_token(&args_context).is_some() {
            self.consume_until_next_token(&mut args_context);
            match self.parse_id_or_wildcard(context)? {
                Some(ConstantIndexOrWildcard::Index(constant_index)) => {
                    if self.constants.get_str(constant_index) == "self" {
                        if !arg_nodes.is_empty() {
                            return syntax_error!(SelfArgNotInFirstPosition, self);
                        }
                        is_instance_function = true;
                    }

                    arg_ids.push(constant_index);
                    arg_nodes.push(self.push_node(Node::Id(constant_index))?);

                    if self.peek_token() == Some(Token::Ellipsis) {
                        self.consume_token();
                        is_variadic = true;
                        break;
                    }
                }
                Some(ConstantIndexOrWildcard::Wildcard) => {
                    arg_nodes.push(self.push_node(Node::Wildcard)?)
                }
                None => match self.peek_token() {
                    Some(Token::SquareOpen) => {
                        self.consume_token();

                        let list_args = self.parse_nested_function_args(&mut arg_ids)?;
                        arg_nodes.push(self.push_node(Node::List(list_args))?);

                        if self.consume_next_token(&mut args_context) != Some(Token::SquareClose) {
                            return syntax_error!(ExpectedListEnd, self);
                        }
                    }
                    Some(Token::RoundOpen) => {
                        self.consume_token();

                        let tuple_args = self.parse_nested_function_args(&mut arg_ids)?;
                        arg_nodes.push(self.push_node(Node::Tuple(tuple_args))?);

                        if self.consume_next_token(&mut args_context) != Some(Token::RoundClose) {
                            return syntax_error!(ExpectedCloseParen, self);
                        }
                    }
                    _ => break,
                },
            }

            if self.peek_next_token_on_same_line() == Some(Token::Comma) {
                self.consume_next_token_on_same_line();
            } else {
                break;
            }
        }

        // Check for function args end
        let mut function_end_context = ExpressionContext::permissive();
        function_end_context.expected_indentation = Indentation::Equal(start_indent);
        if self.consume_next_token(&mut function_end_context) != Some(Token::Function) {
            return syntax_error!(ExpectedFunctionArgsEnd, self);
        }

        // body
        let mut function_frame = Frame::default();
        function_frame.ids_assigned_in_scope.extend(arg_ids.iter());
        self.frame_stack.push(function_frame);

        let body = if let Some(block) = self.parse_indented_block()? {
            // If the body is a Map block, then finish_expressions is needed here to finalise the
            // captures for the Map values. Normally parse_line takes care of calling
            // finish_expressions, but this is a situation where it can be bypassed.
            self.frame_mut()?.finish_expression();
            block
        } else {
            self.consume_until_next_token_on_same_line();
            if let Some(body) = self.parse_line(&mut ExpressionContext::permissive())? {
                body
            } else {
                return indentation_error!(FunctionBody, self);
            }
        };

        let function_frame = self
            .frame_stack
            .pop()
            .ok_or_else(|| make_internal_error!(MissingScope, self))?;

        self.frame_mut()?
            .add_nested_accessed_non_locals(&function_frame);

        let local_count = function_frame.local_count();

        let span_end = self.current_span().end;

        let result = self.ast.push(
            Node::Function(Function {
                args: arg_nodes,
                local_count,
                accessed_non_locals: Vec::from_iter(function_frame.accessed_non_locals),
                body,
                is_instance_function,
                is_variadic,
                is_generator: function_frame.contains_yield,
            }),
            Span {
                start: span_start,
                end: span_end,
            },
        )?;

        Ok(Some(result))
    }

    fn parse_line(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let result = if let Some(result) = self.parse_for_loop(context)? {
            Some(result)
        } else if let Some(result) = self.parse_loop_block()? {
            Some(result)
        } else if let Some(result) = self.parse_while_loop()? {
            Some(result)
        } else if let Some(result) = self.parse_until_loop()? {
            Some(result)
        } else if let Some(result) = self.parse_export(context)? {
            Some(result)
        } else {
            self.parse_expressions(context, TempResult::No)?
        };

        self.frame_mut()?.finish_expression();

        Ok(result)
    }

    // Parse a comma separated series of expressions
    //
    // If only a single expression is encountered then that expression's node is the only thing
    // generated.
    //
    // Otherwise, for multiple expressions, the result of the expression can be temporary
    // (i.e. not assigned to an identifier) in which case a TempTuple is generated,
    // otherwise the result will be a Tuple.
    fn parse_expressions(
        &mut self,
        context: &mut ExpressionContext,
        temp_result: TempResult,
    ) -> Result<Option<AstIndex>, ParserError> {
        let mut expression_context = ExpressionContext {
            allow_space_separated_call: true,
            ..*context
        };

        if let Some(first) = self.parse_expression(&mut expression_context)? {
            let mut expressions = vec![first];
            let mut encountered_comma = false;

            while let Some(Token::Comma) = self.peek_next_token_on_same_line() {
                self.consume_next_token_on_same_line();
                encountered_comma = true;

                if let Some(next_expression) =
                    self.parse_expression_with_lhs(Some(&expressions), &mut expression_context)?
                {
                    match self.ast.node(next_expression).node {
                        Node::Assign { .. }
                        | Node::MultiAssign { .. }
                        | Node::For(_)
                        | Node::While { .. }
                        | Node::Until { .. } => {
                            // These nodes will have consumed the parsed expressions,
                            // so there's no further work to do.
                            // e.g.
                            //   x, y for x, y in a, b
                            //   a, b = c, d
                            //   a, b, c = x
                            return Ok(Some(next_expression));
                        }
                        _ => {}
                    }

                    expressions.push(next_expression);
                }
            }

            if expressions.len() == 1 && !encountered_comma {
                Ok(Some(first))
            } else {
                let result = match temp_result {
                    TempResult::No => Node::Tuple(expressions),
                    TempResult::Yes => Node::TempTuple(expressions),
                };
                Ok(Some(self.push_node(result)?))
            }
        } else {
            Ok(None)
        }
    }

    fn parse_expression_with_lhs(
        &mut self,
        lhs: Option<&[AstIndex]>,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        self.parse_expression_start(lhs, 0, context)
    }

    fn parse_expression(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        self.parse_expression_with_min_precedence(0, context)
    }

    fn parse_expression_with_min_precedence(
        &mut self,
        min_precedence: u8,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let result = self.parse_expression_start(None, min_precedence, context)?;

        let result = match self.peek_next_token_on_same_line() {
            Some(Token::Range) | Some(Token::RangeInclusive) => {
                self.parse_range(result, context)?
            }
            _ => result,
        };

        Ok(result)
    }

    fn parse_expression_start(
        &mut self,
        lhs: Option<&[AstIndex]>,
        min_precedence: u8,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let expression_start = match self.parse_term(context)? {
            Some(term) => term,
            None => return Ok(None),
        };

        if let Some(lhs) = lhs {
            let mut lhs_with_expression_start = lhs.to_vec();
            lhs_with_expression_start.push(expression_start);
            self.parse_expression_continued(&lhs_with_expression_start, min_precedence, context)
        } else {
            self.parse_expression_continued(&[expression_start], min_precedence, context)
        }
    }

    fn parse_expression_continued(
        &mut self,
        lhs: &[AstIndex],
        min_precedence: u8,
        context: &ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        use Token::*;

        let last_lhs = match lhs.last() {
            Some(last) => *last,
            None => return internal_error!(MissingContinuedExpressionLhs, self),
        };

        let mut context = ExpressionContext {
            expected_indentation: context.expected_indentation.greater_or_equal(),
            ..*context
        };

        if let Some(next) = self.peek_next_token(&context) {
            match next.token {
                Assign => return self.parse_assign_expression(lhs, AssignOp::Equal, &mut context),
                AssignAdd => return self.parse_assign_expression(lhs, AssignOp::Add, &mut context),
                AssignSubtract => {
                    return self.parse_assign_expression(lhs, AssignOp::Subtract, &mut context)
                }
                AssignMultiply => {
                    return self.parse_assign_expression(lhs, AssignOp::Multiply, &mut context)
                }
                AssignDivide => {
                    return self.parse_assign_expression(lhs, AssignOp::Divide, &mut context)
                }
                AssignModulo => {
                    return self.parse_assign_expression(lhs, AssignOp::Modulo, &mut context)
                }
                _ => {
                    if let Some((left_priority, right_priority)) = operator_precedence(next.token) {
                        if left_priority >= min_precedence {
                            let op = self.consume_next_token(&mut context).unwrap();

                            // Move on to the token after the operator
                            if self.peek_next_token(&context).is_none() {
                                return indentation_error!(RhsExpression, self);
                            }
                            self.consume_until_next_token(&mut context);

                            let rhs = if let Some(rhs_expression) =
                                self.parse_expression_start(None, right_priority, &mut context)?
                            {
                                rhs_expression
                            } else {
                                return indentation_error!(RhsExpression, self);
                            };

                            let op_node = self.push_ast_op(op, last_lhs, rhs)?;
                            return self.parse_expression_continued(
                                &[op_node],
                                min_precedence,
                                &context,
                            );
                        }
                    }
                }
            }
        }

        Ok(Some(last_lhs))
    }

    fn parse_assign_expression(
        &mut self,
        lhs: &[AstIndex],
        assign_op: AssignOp,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        self.consume_next_token(context); // The assign op has already been matched

        let mut targets = Vec::new();

        for lhs_expression in lhs.iter() {
            match self.ast.node(*lhs_expression).node.clone() {
                Node::Id(id_index) => {
                    if matches!(assign_op, AssignOp::Equal) {
                        self.frame_mut()?.add_id_assignment(id_index);
                        self.frame_mut()?.remove_id_access(id_index);
                    }
                }
                Node::Lookup(_) | Node::Wildcard => {}
                _ => return syntax_error!(ExpectedAssignmentTarget, self),
            }

            targets.push(AssignTarget {
                target_index: *lhs_expression,
                scope: Scope::Local,
            });
        }

        if targets.is_empty() {
            return internal_error!(MissingAssignmentTarget, self);
        }

        let single_target = targets.len() == 1;

        let temp_result = if single_target {
            TempResult::No
        } else {
            TempResult::Yes
        };

        if let Some(rhs) =
            self.parse_expressions(&mut ExpressionContext::permissive(), temp_result)?
        {
            let node = if single_target {
                Node::Assign {
                    target: *targets.first().unwrap(),
                    op: assign_op,
                    expression: rhs,
                }
            } else {
                Node::MultiAssign {
                    targets,
                    expression: rhs,
                }
            };
            Ok(Some(self.push_node(node)?))
        } else {
            indentation_error!(RhsExpression, self)
        }
    }

    fn add_string_constant(&mut self, s: &str) -> Result<ConstantIndex, ParserError> {
        match self.constants.add_string(s) {
            Ok(result) => Ok(result),
            Err(_) => internal_error!(ConstantPoolCapacityOverflow, self),
        }
    }

    fn parse_id(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<ConstantIndex>, ParserError> {
        match self.peek_next_token(context) {
            Some(PeekInfo {
                token: Token::Id, ..
            }) => {
                self.consume_next_token(context);
                let result = self.add_string_constant(self.lexer.slice())?;
                Ok(Some(result))
            }
            _ => Ok(None),
        }
    }

    fn parse_id_or_wildcard(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<ConstantIndexOrWildcard>, ParserError> {
        match self.peek_next_token(context) {
            Some(PeekInfo {
                token: Token::Id, ..
            }) => {
                self.consume_next_token(context);
                let result = self.add_string_constant(self.lexer.slice())?;
                Ok(Some(ConstantIndexOrWildcard::Index(result)))
            }
            Some(PeekInfo {
                token: Token::Wildcard,
                ..
            }) => {
                self.consume_next_token_on_same_line();
                Ok(Some(ConstantIndexOrWildcard::Wildcard))
            }
            _ => Ok(None),
        }
    }

    fn parse_meta_key(
        &mut self,
    ) -> Result<Option<(MetaKeyId, Option<ConstantIndex>)>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::At) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        let mut meta_name = None;

        let meta_key_id = match self.consume_token() {
            Some(Token::Add) => MetaKeyId::Add,
            Some(Token::Subtract) => MetaKeyId::Subtract,
            Some(Token::Multiply) => MetaKeyId::Multiply,
            Some(Token::Divide) => MetaKeyId::Divide,
            Some(Token::Modulo) => MetaKeyId::Modulo,
            Some(Token::Less) => MetaKeyId::Less,
            Some(Token::LessOrEqual) => MetaKeyId::LessOrEqual,
            Some(Token::Greater) => MetaKeyId::Greater,
            Some(Token::GreaterOrEqual) => MetaKeyId::GreaterOrEqual,
            Some(Token::Equal) => MetaKeyId::Equal,
            Some(Token::NotEqual) => MetaKeyId::NotEqual,
            Some(Token::Not) => MetaKeyId::Not,
            Some(Token::Id) => match self.lexer.slice() {
                "display" => MetaKeyId::Display,
                "negate" => MetaKeyId::Negate,
                "tests" => MetaKeyId::Tests,
                "pre_test" => MetaKeyId::PreTest,
                "post_test" => MetaKeyId::PostTest,
                "test" => match self.consume_next_token_on_same_line() {
                    Some(Token::Id) => {
                        let test_name = self.add_string_constant(self.lexer.slice())?;
                        meta_name = Some(test_name);
                        MetaKeyId::Test
                    }
                    _ => return syntax_error!(ExpectedTestName, self),
                },
                "meta" => match self.consume_next_token_on_same_line() {
                    Some(Token::Id) => {
                        let id = self.add_string_constant(self.lexer.slice())?;
                        meta_name = Some(id);
                        MetaKeyId::Named
                    }
                    _ => return syntax_error!(ExpectedMetaId, self),
                },
                "type" => MetaKeyId::Type,
                _ => return syntax_error!(UnexpectedMetaKey, self),
            },
            Some(Token::SquareOpen) => match self.consume_token() {
                Some(Token::SquareClose) => MetaKeyId::Index,
                _ => return syntax_error!(UnexpectedMetaKey, self),
            },
            _ => return syntax_error!(UnexpectedMetaKey, self),
        };

        Ok(Some((meta_key_id, meta_name)))
    }

    fn parse_map_key(&mut self) -> Result<Option<MapKey>, ParserError> {
        let result = if let Some(id) = self.parse_id(&mut ExpressionContext::restricted())? {
            Some(MapKey::Id(id))
        } else if let Some((meta_key_id, meta_name)) = self.parse_meta_key()? {
            Some(MapKey::Meta(meta_key_id, meta_name))
        } else if let Some((string_key, _span)) =
            self.parse_string(&mut ExpressionContext::restricted())?
        {
            Some(MapKey::Str(string_key))
        } else {
            None
        };

        Ok(result)
    }

    // Attempts to parse whitespace-separated call args
    //
    // The context is used to determine what kind of argument separation is allowed.
    //
    // The resulting Vec will be empty if no arguments were encountered.
    //
    // See also parse_parenthesized_args.
    fn parse_call_args(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Vec<AstIndex>, ParserError> {
        let mut args = Vec::new();

        let mut arg_context = ExpressionContext {
            expected_indentation: Indentation::Greater,
            ..*context
        };

        let mut last_arg_line = self.current_line_number();

        while let Some(peeked) = self.peek_next_token(&arg_context) {
            let new_line = peeked.line > last_arg_line;
            last_arg_line = peeked.line;

            if new_line {
                arg_context.expected_indentation = Indentation::Equal(peeked.indent);
            } else if !(context.allow_space_separated_call
                && self.peek_token() == Some(Token::Whitespace))
            {
                break;
            }

            if let Some(expression) = self
                .parse_expression_with_min_precedence(MIN_PRECEDENCE_AFTER_PIPE, &mut arg_context)?
            {
                args.push(expression);
            } else {
                break;
            }

            if self.peek_next_token_on_same_line() == Some(Token::Comma) {
                self.consume_next_token_on_same_line();
            } else {
                break;
            }
        }

        Ok(args)
    }

    fn parse_id_expression(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        if let Some(constant_index) = self.parse_id(context)? {
            if self.peek_token() == Some(Token::Colon) {
                self.parse_braceless_map_start(MapKey::Id(constant_index), context)
            } else {
                self.frame_mut()?.add_id_access(constant_index);

                let mut context = context.start_new_expression();
                let result = if self.next_token_is_lookup_start(&context) {
                    let id_index = self.push_node(Node::Id(constant_index))?;
                    self.parse_lookup(id_index, &mut context)?
                } else {
                    let start_span = self.current_span();
                    let args = self.parse_call_args(&mut context)?;

                    if args.is_empty() {
                        self.push_node(Node::Id(constant_index))?
                    } else {
                        self.push_node_with_start_span(
                            Node::NamedCall {
                                id: constant_index,
                                args,
                            },
                            start_span,
                        )?
                    }
                };

                Ok(Some(result))
            }
        } else {
            Ok(None)
        }
    }

    fn parse_lookup(
        &mut self,
        root: AstIndex,
        context: &mut ExpressionContext,
    ) -> Result<AstIndex, ParserError> {
        let mut lookup = Vec::new();

        let start_indent = self.current_indent();
        let mut lookup_indent = None;
        let mut node_context = ExpressionContext {
            expected_indentation: Indentation::Greater,
            ..*context
        };
        let mut node_start_span = self.current_span();

        lookup.push((LookupNode::Root(root), node_start_span));

        while let Some(token) = self.peek_token() {
            match token {
                Token::RoundOpen => {
                    node_start_span = self.current_span();
                    let args = self.parse_parenthesized_args()?;
                    lookup.push((
                        LookupNode::Call {
                            args,
                            with_parens: true,
                        },
                        self.span_with_start(node_start_span),
                    ));
                }
                Token::SquareOpen => {
                    self.consume_token();
                    node_start_span = self.current_span();

                    let mut index_context = ExpressionContext::restricted();

                    let index_expression = if let Some(index_expression) =
                        self.parse_expression(&mut index_context)?
                    {
                        match self.peek_token() {
                            Some(Token::Range) => {
                                self.consume_token();

                                if let Some(end_expression) =
                                    self.parse_expression(&mut index_context)?
                                {
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

                                if let Some(end_expression) =
                                    self.parse_expression(&mut index_context)?
                                {
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
                        // Look for RangeTo/RangeFull
                        // e.g. x[..10], y[..]
                        match self.peek_next_token_on_same_line() {
                            Some(Token::Range) => {
                                self.consume_next_token_on_same_line();

                                if let Some(end_expression) =
                                    self.parse_expression(&mut index_context)?
                                {
                                    self.push_node(Node::RangeTo {
                                        end: end_expression,
                                        inclusive: false,
                                    })?
                                } else {
                                    self.push_node(Node::RangeFull)?
                                }
                            }
                            Some(Token::RangeInclusive) => {
                                self.consume_next_token_on_same_line();

                                if let Some(end_expression) =
                                    self.parse_expression(&mut index_context)?
                                {
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

                    if let Some(Token::SquareClose) = self.peek_next_token_on_same_line() {
                        self.consume_next_token_on_same_line();
                        lookup.push((
                            LookupNode::Index(index_expression),
                            self.span_with_start(node_start_span),
                        ));
                    } else {
                        return syntax_error!(ExpectedIndexEnd, self);
                    }
                }
                Token::Dot => {
                    self.consume_token();

                    if !matches!(
                        self.peek_token(),
                        Some(Token::Id | Token::SingleQuote | Token::DoubleQuote)
                    ) {
                        return syntax_error!(ExpectedMapKey, self);
                    } else if let Some(id_index) =
                        self.parse_id(&mut ExpressionContext::restricted())?
                    {
                        lookup.push((LookupNode::Id(id_index), self.current_span()));
                    } else if let Some((lookup_string, span)) =
                        self.parse_string(&mut ExpressionContext::restricted())?
                    {
                        lookup.push((LookupNode::Str(lookup_string), span));
                    } else {
                        return syntax_error!(ExpectedMapKey, self);
                    }
                }
                // Indented Dot on the next line?
                _ if matches!(
                    self.peek_next_token(&node_context),
                    Some(PeekInfo {
                        token: Token::Dot,
                        ..
                    })
                ) =>
                {
                    // Consume up to the Dot, which will be picked up on the next iteration
                    self.consume_until_next_token(&mut node_context);

                    // Check that the next dot is on an indented line
                    if lookup_indent.is_none() {
                        let new_indent = self.current_indent();

                        if new_indent > start_indent {
                            lookup_indent = Some(new_indent);
                        } else {
                            break;
                        }
                    }

                    node_context = ExpressionContext {
                        // Starting a new line, so space separated calls are allowed
                        allow_space_separated_call: true,
                        ..node_context
                    };
                }
                _ => {
                    // Attempt to parse trailing call arguments,
                    // e.g.
                    //   x.foo 42, 99
                    //         ~~~~~~
                    //
                    //   x.foo
                    //     42, 99
                    //     ~~~~~~
                    //
                    //   foo
                    //     .bar 123
                    //          ~~~
                    //     .baz x, y
                    //          ~~~~
                    let args = self.parse_call_args(&mut node_context)?;

                    // Now that space separated args have been parsed,
                    // don't allow any more while we're on the same line.
                    node_context.allow_space_separated_call = false;

                    if args.is_empty() {
                        // No arguments found, so we're at the end of the lookup
                        break;
                    } else {
                        lookup.push((
                            LookupNode::Call {
                                args,
                                with_parens: false,
                            },
                            node_start_span,
                        ));
                    }
                }
            }
        }

        // Add the lookup nodes to the AST in reverse order:
        // the final AST index will be the lookup root node.
        let mut next_index = None;
        for (node, span) in lookup.iter().rev() {
            next_index =
                Some(self.push_node_with_span(Node::Lookup((node.clone(), next_index)), *span)?);
        }
        next_index.ok_or_else(|| make_internal_error!(LookupParseFailure, self))
    }

    fn parse_parenthesized_args(&mut self) -> Result<Vec<AstIndex>, ParserError> {
        if self.consume_next_token_on_same_line() != Some(Token::RoundOpen) {
            return internal_error!(ArgumentsParseFailure, self);
        }

        let start_indent = self.current_indent();
        let mut args = Vec::new();
        let mut args_context = ExpressionContext::permissive();

        while self.peek_next_token(&args_context).is_some() {
            self.consume_until_next_token(&mut args_context);

            if let Some(expression) = self.parse_expression(&mut ExpressionContext::inline())? {
                args.push(expression);
            } else {
                break;
            }

            if self.peek_next_token_on_same_line() == Some(Token::Comma) {
                self.consume_next_token_on_same_line();
            } else {
                break;
            }
        }

        let mut args_end_context = ExpressionContext::permissive();
        args_end_context.expected_indentation = Indentation::Equal(start_indent);
        if !matches!(
            self.peek_next_token(&args_end_context),
            Some(PeekInfo {
                token: Token::RoundClose,
                ..
            })
        ) {
            return syntax_error!(ExpectedArgsEnd, self);
        }

        self.consume_next_token(&mut args_end_context);
        Ok(args)
    }

    fn parse_range(
        &mut self,
        lhs: Option<AstIndex>,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        use Node::{Range, RangeFrom, RangeFull, RangeTo};

        let inclusive = match self.peek_next_token_on_same_line() {
            Some(Token::Range) => false,
            Some(Token::RangeInclusive) => true,
            _ => return Ok(None),
        };

        self.consume_next_token_on_same_line();

        let rhs = self.parse_expression(&mut ExpressionContext::inline())?;

        let range_node = match (lhs, rhs) {
            (Some(start), Some(end)) => Range {
                start,
                end,
                inclusive,
            },
            (Some(start), None) => RangeFrom { start },
            (None, Some(end)) => RangeTo { end, inclusive },
            (None, None) => RangeFull,
        };

        let range_node = self.push_node(range_node)?;
        let result = self.check_for_lookup_after_node(range_node, context)?;
        Ok(Some(result))
    }

    fn parse_export(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::Export) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        let export_id = if let Some(constant_index) = self.parse_id(context)? {
            self.push_node(Node::Id(constant_index))?
        } else if let Some((meta_key_id, name)) = self.parse_meta_key()? {
            self.push_node(Node::Meta(meta_key_id, name))?
        } else {
            return syntax_error!(ExpectedExportExpression, self);
        };

        match self.peek_next_token_on_same_line() {
            Some(Token::Assign) => {
                self.consume_next_token_on_same_line();

                if let Some(rhs) =
                    self.parse_expressions(&mut ExpressionContext::permissive(), TempResult::No)?
                {
                    let node = Node::Assign {
                        target: AssignTarget {
                            target_index: export_id,
                            scope: Scope::Export,
                        },
                        op: AssignOp::Equal,
                        expression: rhs,
                    };

                    Ok(Some(self.push_node(node)?))
                } else {
                    indentation_error!(RhsExpression, self)
                }
            }
            Some(Token::NewLine) | Some(Token::NewLineIndented) => Ok(Some(export_id)),
            _ => syntax_error!(UnexpectedTokenAfterExportId, self),
        }
    }

    fn parse_throw_expression(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::Throw) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        let start_span = self.current_span();

        let expression = if let Some(expression) =
            self.parse_expression(&mut ExpressionContext::permissive())?
        {
            expression
        } else {
            return syntax_error!(ExpectedExpression, self);
        };

        let result = self.push_node_with_start_span(Node::Throw(expression), start_span)?;
        Ok(Some(result))
    }

    fn parse_debug_expression(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::Debug) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        let start_position = self.current_span().start;

        self.consume_until_next_token_on_same_line();

        let mut context = ExpressionContext::permissive();
        let expression_source_start = self.lexer.source_position();
        let expression =
            if let Some(expression) = self.parse_expressions(&mut context, TempResult::No)? {
                expression
            } else {
                return syntax_error!(ExpectedExpression, self);
            };

        let expression_source_end = self.lexer.source_position();

        let expression_string = self.add_string_constant(
            &self.lexer.source()[expression_source_start..expression_source_end],
        )?;

        let result = self.ast.push(
            Node::Debug {
                expression_string,
                expression,
            },
            Span {
                start: start_position,
                end: self.current_span().end,
            },
        )?;

        Ok(Some(result))
    }

    fn parse_term(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        use Node::*;

        let start_indent = self.current_indent();
        if let Some(peeked) = self.peek_next_token(context) {
            let result = match peeked.token {
                Token::True => {
                    self.consume_next_token(context);
                    Some(self.push_node(BoolTrue)?)
                }
                Token::False => {
                    self.consume_next_token(context);
                    Some(self.push_node(BoolFalse)?)
                }
                Token::RoundOpen => self.parse_nested_expressions(context)?,
                Token::Number => self.parse_number(false, context)?,
                Token::DoubleQuote | Token::SingleQuote => {
                    let (string, span) = self.parse_string(context)?.unwrap();

                    if self.peek_token() == Some(Token::Colon) {
                        self.parse_braceless_map_start(MapKey::Str(string), context)?
                    } else {
                        let string_node = self.push_node_with_span(Str(string), span)?;
                        Some(self.check_for_lookup_after_node(string_node, context)?)
                    }
                }
                Token::Id => self.parse_id_expression(context)?,
                Token::At if context.allow_map_block || peeked.indent > start_indent => {
                    self.consume_until_next_token(context);
                    let (meta_key_id, meta_name) = self.parse_meta_key()?.unwrap();
                    self.parse_braceless_map_start(MapKey::Meta(meta_key_id, meta_name), context)?
                }
                Token::Wildcard => {
                    self.consume_next_token(context);
                    Some(self.push_node(Node::Wildcard)?)
                }
                Token::SquareOpen => self.parse_list(context)?,
                Token::CurlyOpen => self.parse_map_inline(context)?,
                Token::Num2 => {
                    self.consume_next_token(context);
                    let start_span = self.current_span();

                    let args = if self.peek_token() == Some(Token::RoundOpen) {
                        self.parse_parenthesized_args()?
                    } else {
                        self.parse_call_args(&mut ExpressionContext::permissive())?
                    };

                    if args.is_empty() {
                        return syntax_error!(ExpectedExpression, self);
                    } else if args.len() > 2 {
                        return syntax_error!(TooManyNum2Terms, self);
                    }

                    let node = self.push_node_with_start_span(Num2(args), start_span)?;
                    Some(self.check_for_lookup_after_node(node, context)?)
                }
                Token::Num4 => {
                    self.consume_next_token(context);
                    let start_span = self.current_span();

                    let args = if self.peek_token() == Some(Token::RoundOpen) {
                        self.parse_parenthesized_args()?
                    } else {
                        self.parse_call_args(&mut ExpressionContext::permissive())?
                    };

                    if args.is_empty() {
                        return syntax_error!(ExpectedExpression, self);
                    } else if args.len() > 4 {
                        return syntax_error!(TooManyNum4Terms, self);
                    }

                    let node = self.push_node_with_start_span(Num4(args), start_span)?;
                    Some(self.check_for_lookup_after_node(node, context)?)
                }
                Token::If => self.parse_if_expression(context)?,
                Token::Match => self.parse_match_expression(context)?,
                Token::Switch => self.parse_switch_expression(context)?,
                Token::Function => self.parse_function(context)?,
                Token::Subtract => match self.peek_token_n(peeked.peek_count + 1) {
                    Some(token) if token.is_whitespace() || token.is_newline() => None,
                    Some(Token::Number) => {
                        self.consume_next_token(context);
                        self.parse_number(true, context)?
                    }
                    Some(_) => {
                        self.consume_next_token(context);
                        if let Some(term) = self.parse_term(&mut ExpressionContext::restricted())? {
                            let result = self.push_node(Node::UnaryOp {
                                op: AstUnaryOp::Negate,
                                value: term,
                            })?;
                            Some(result)
                        } else {
                            return syntax_error!(ExpectedExpression, self);
                        }
                    }
                    None => None,
                },
                Token::Not => {
                    self.consume_next_token(context);
                    if let Some(expression) = self.parse_expression(&mut ExpressionContext {
                        allow_space_separated_call: true,
                        expected_indentation: Indentation::Greater,
                        ..*context
                    })? {
                        let result = self.push_node(Node::UnaryOp {
                            op: AstUnaryOp::Not,
                            value: expression,
                        })?;
                        Some(result)
                    } else {
                        return syntax_error!(ExpectedExpression, self);
                    }
                }
                Token::Yield => {
                    self.consume_next_token(context);
                    if let Some(expression) =
                        self.parse_expressions(&mut context.start_new_expression(), TempResult::No)?
                    {
                        let result = self.push_node(Node::Yield(expression))?;
                        self.frame_mut()?.contains_yield = true;
                        Some(result)
                    } else {
                        return syntax_error!(ExpectedExpression, self);
                    }
                }
                Token::Break => {
                    self.consume_next_token(context);
                    Some(self.push_node(Node::Break)?)
                }
                Token::Continue => {
                    self.consume_next_token(context);
                    Some(self.push_node(Node::Continue)?)
                }
                Token::Return => {
                    self.consume_next_token(context);
                    let return_value = self
                        .parse_expressions(&mut context.start_new_expression(), TempResult::No)?;
                    Some(self.push_node(Node::Return(return_value))?)
                }
                Token::Throw => self.parse_throw_expression()?,
                Token::Debug => self.parse_debug_expression()?,
                Token::From | Token::Import => self.parse_import_expression(context)?,
                Token::Try => self.parse_try_expression(context)?,
                Token::Error => return syntax_error!(LexerError, self),
                _ => None,
            };

            Ok(result)
        } else {
            Ok(None)
        }
    }

    // Checks to see if a lookup starts after the parsed node,
    // and either returns the node if there's no lookup,
    // or uses the node as the start of the lookup.
    fn check_for_lookup_after_node(
        &mut self,
        node: AstIndex,
        context: &ExpressionContext,
    ) -> Result<AstIndex, ParserError> {
        let mut lookup_context = context.start_new_expression();
        if self.next_token_is_lookup_start(&lookup_context) {
            self.parse_lookup(node, &mut lookup_context)
        } else {
            Ok(node)
        }
    }

    fn parse_number(
        &mut self,
        negate: bool,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        use Node::*;

        self.consume_next_token(context);

        let slice = self.lexer.slice();

        let maybe_integer = if let Some(hex) = slice.strip_prefix("0x") {
            i64::from_str_radix(hex, 16)
        } else if let Some(octal) = slice.strip_prefix("0o") {
            i64::from_str_radix(octal, 8)
        } else if let Some(binary) = slice.strip_prefix("0b") {
            i64::from_str_radix(binary, 2)
        } else {
            i64::from_str(slice)
        };

        let number_node = if let Ok(n) = maybe_integer {
            if n == 0 {
                self.push_node(Number0)?
            } else if n == 1 && !negate {
                self.push_node(Number1)?
            } else {
                let n = if negate { -n } else { n };
                match self.constants.add_i64(n) {
                    Ok(constant_index) => self.push_node(Int(constant_index))?,
                    Err(_) => return internal_error!(ConstantPoolCapacityOverflow, self),
                }
            }
        } else {
            match f64::from_str(slice) {
                Ok(n) => {
                    let n = if negate { -n } else { n };
                    match self.constants.add_f64(n) {
                        Ok(constant_index) => self.push_node(Float(constant_index))?,
                        Err(_) => return internal_error!(ConstantPoolCapacityOverflow, self),
                    }
                }
                Err(_) => {
                    return internal_error!(NumberParseFailure, self);
                }
            }
        };

        Ok(Some(
            self.check_for_lookup_after_node(number_node, context)?,
        ))
    }

    fn parse_list(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let mut list_context = *context;
        let start_indent = self.current_indent();
        let start_span = self.current_span();

        if self.consume_next_token(&mut list_context) != Some(Token::SquareOpen) {
            return internal_error!(UnexpectedToken, self);
        }

        // The end brace should have the same indentation as the start brace.
        if matches!(list_context.expected_indentation, Indentation::Greater) {
            list_context.expected_indentation = Indentation::Equal(start_indent);
        }

        let mut entries = Vec::new();

        let mut entry_context = ExpressionContext::permissive();
        while !matches!(
            self.peek_next_token(&entry_context),
            Some(PeekInfo {
                token: Token::SquareClose,
                ..
            }) | None
        ) {
            self.consume_until_next_token(&mut entry_context);

            if let Some(entry) = self.parse_expression(&mut ExpressionContext::inline())? {
                entries.push(entry);
            }

            if self.peek_next_token_on_same_line() == Some(Token::Comma) {
                self.consume_next_token_on_same_line();
            } else {
                break;
            }
        }

        // Consume the list end
        if !matches!(
            self.peek_next_token(&list_context),
            Some(PeekInfo {
                token: Token::SquareClose,
                ..
            })
        ) {
            return syntax_error!(ExpectedListEnd, self);
        }
        self.consume_next_token(&mut list_context);

        let list_node = self.push_node_with_start_span(Node::List(entries), start_span)?;
        let result = self.check_for_lookup_after_node(list_node, &list_context)?;
        Ok(Some(result))
    }

    // If a braceless map key has been detected (':' has been peeked),
    // decide if we're parsing an indented map block, or comma-separated entries
    fn parse_braceless_map_start(
        &mut self,
        first_key: MapKey,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let start_span = self.current_span();
        let start_indent = self.current_indent();

        if self.consume_token() != Some(Token::Colon) {
            return internal_error!(ExpectedMapColon, self);
        }

        let entries =
            if let Some(value) = self.parse_expression(&mut ExpressionContext::permissive())? {
                if let Some(Token::Comma) = self.peek_next_token_on_same_line() {
                    self.consume_next_token_on_same_line();
                    let mut entries = vec![(first_key, Some(value))];
                    entries.extend(self.parse_comma_separated_map_entries(context, false)?);
                    entries
                } else if context.allow_map_block {
                    let mut block_context = ExpressionContext::permissive();
                    block_context.expected_indentation = Indentation::Equal(start_indent);
                    return self.parse_map_block(
                        (first_key, Some(value)),
                        start_span,
                        &mut block_context,
                    );
                } else {
                    vec![(first_key, Some(value))]
                }
            } else {
                return syntax_error!(ExpectedMapValue, self);
            };

        let result = self.push_node_with_start_span(Node::Map(entries), start_span)?;
        Ok(Some(result))
    }

    fn parse_map_block(
        &mut self,
        first_entry: (MapKey, Option<AstIndex>),
        start_span: Span,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let mut entries = vec![first_entry];

        while self.peek_next_token(context).is_some() {
            self.consume_until_next_token(context);

            if let Some(key) = self.parse_map_key()? {
                if self.peek_next_token_on_same_line() == Some(Token::Colon) {
                    self.consume_next_token_on_same_line();

                    if let Some(value) = self.parse_expression(&mut ExpressionContext::inline())? {
                        entries.push((key, Some(value)));
                    } else {
                        // If a value wasn't found on the same line as the key,
                        // look for an indented value
                        if let Some(value) = self.parse_indented_block()? {
                            entries.push((key, Some(value)));
                        } else {
                            return syntax_error!(ExpectedMapValue, self);
                        }
                    }
                } else {
                    return syntax_error!(ExpectedMapColon, self);
                }
            } else {
                return syntax_error!(ExpectedMapEntry, self);
            }
        }

        let result = self.push_node_with_start_span(Node::Map(entries), start_span)?;
        Ok(Some(result))
    }

    fn parse_map_inline(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.consume_next_token(context) != Some(Token::CurlyOpen) {
            return internal_error!(UnexpectedToken, self);
        }

        let start_indent = self.current_indent();
        let start_span = self.current_span();

        let entries = self.parse_comma_separated_map_entries(context, true)?;

        let mut map_end_context = ExpressionContext::permissive();
        map_end_context.expected_indentation = Indentation::Equal(start_indent);
        if !matches!(
            self.peek_next_token(&map_end_context),
            Some(PeekInfo {
                token: Token::CurlyClose,
                ..
            })
        ) {
            return syntax_error!(ExpectedMapEnd, self);
        }
        self.consume_next_token(&mut map_end_context);

        let map_node = self.push_node_with_start_span(Node::Map(entries), start_span)?;
        let result = self.check_for_lookup_after_node(map_node, context)?;
        Ok(Some(result))
    }

    fn parse_comma_separated_map_entries(
        &mut self,
        context: &mut ExpressionContext,
        allow_valueless_entries: bool,
    ) -> Result<Vec<(MapKey, Option<AstIndex>)>, ParserError> {
        let mut entries = Vec::new();

        while self.peek_next_token(context).is_some() {
            self.consume_until_next_token(context);

            if let Some(key) = self.parse_map_key()? {
                if self.peek_token() == Some(Token::Colon) {
                    self.consume_token();

                    let mut value_context = ExpressionContext::permissive();
                    if self.peek_next_token(&value_context).is_none() {
                        return syntax_error!(ExpectedMapValue, self);
                    }
                    self.consume_until_next_token(&mut value_context);

                    if let Some(value) = self.parse_expression(&mut value_context)? {
                        entries.push((key, Some(value)));
                    } else {
                        return syntax_error!(ExpectedMapValue, self);
                    }
                } else if allow_valueless_entries {
                    entries.push((key, None));
                } else {
                    return syntax_error!(ExpectedMapValue, self);
                }

                if self.peek_next_token_on_same_line() == Some(Token::Comma) {
                    self.consume_next_token_on_same_line();
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(entries)
    }

    fn parse_for_loop(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::For) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        let start_span = self.current_span();

        let mut args = Vec::new();
        while let Some(id_or_wildcard) = self.parse_id_or_wildcard(context)? {
            match id_or_wildcard {
                ConstantIndexOrWildcard::Index(id_index) => {
                    args.push(Some(id_index));
                    self.frame_mut()?.ids_assigned_in_scope.insert(id_index);
                }
                ConstantIndexOrWildcard::Wildcard => args.push(None),
            }

            match self.peek_next_token_on_same_line() {
                Some(Token::Comma) => {
                    self.consume_next_token_on_same_line();
                }
                Some(Token::In) => {
                    self.consume_next_token_on_same_line();
                    break;
                }
                _ => return syntax_error!(ExpectedForInKeyword, self),
            }
        }
        if args.is_empty() {
            return syntax_error!(ExpectedForArgs, self);
        }

        let iterable = match self.parse_expression(&mut ExpressionContext::inline())? {
            Some(iterable) => iterable,
            None => return syntax_error!(ExpectedForIterable, self),
        };

        match self.parse_indented_block()? {
            Some(body) => {
                let result = self.push_node_with_start_span(
                    Node::For(AstFor {
                        args,
                        iterable,
                        body,
                    }),
                    start_span,
                )?;

                Ok(Some(result))
            }
            None => indentation_error!(ForBody, self),
        }
    }

    fn parse_loop_block(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::Loop) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        if let Some(body) = self.parse_indented_block()? {
            let result = self.push_node(Node::Loop { body })?;
            Ok(Some(result))
        } else {
            return indentation_error!(LoopBody, self);
        }
    }

    fn parse_while_loop(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::While) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        let condition =
            if let Some(condition) = self.parse_expression(&mut ExpressionContext::inline())? {
                condition
            } else {
                return syntax_error!(ExpectedWhileCondition, self);
            };

        match self.parse_indented_block()? {
            Some(body) => {
                let result = self.push_node(Node::While { condition, body })?;
                Ok(Some(result))
            }
            None => indentation_error!(WhileBody, self),
        }
    }

    fn parse_until_loop(&mut self) -> Result<Option<AstIndex>, ParserError> {
        if self.peek_next_token_on_same_line() != Some(Token::Until) {
            return Ok(None);
        }

        self.consume_next_token_on_same_line();

        let condition =
            if let Some(condition) = self.parse_expression(&mut ExpressionContext::inline())? {
                condition
            } else {
                return syntax_error!(ExpectedUntilCondition, self);
            };

        match self.parse_indented_block()? {
            Some(body) => {
                let result = self.push_node(Node::Until { condition, body })?;
                Ok(Some(result))
            }
            None => indentation_error!(UntilBody, self),
        }
    }

    fn parse_if_expression(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let expected_indentation = self.current_indent();
        context.expected_indentation = Indentation::Equal(expected_indentation);

        if self.consume_next_token(context) != Some(Token::If) {
            return internal_error!(UnexpectedToken, self);
        }

        let condition = match self.parse_expression(&mut ExpressionContext::inline())? {
            Some(condition) => condition,
            None => return syntax_error!(ExpectedIfCondition, self),
        };

        let result = if self.peek_next_token_on_same_line() == Some(Token::Then) {
            self.consume_next_token_on_same_line();
            let then_node =
                match self.parse_expressions(&mut ExpressionContext::inline(), TempResult::No)? {
                    Some(then_node) => then_node,
                    None => return syntax_error!(ExpectedThenExpression, self),
                };
            let else_node = if self.peek_next_token_on_same_line() == Some(Token::Else) {
                self.consume_next_token_on_same_line();
                match self.parse_expressions(&mut ExpressionContext::inline(), TempResult::No)? {
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
        } else {
            if !context.allow_linebreaks {
                return syntax_error!(IfBlockNotAllowedInThisContext, self);
            }

            if let Some(then_node) = self.parse_indented_block()? {
                let mut else_if_blocks = Vec::new();

                while let Some(peeked) = self.peek_next_token(context) {
                    if peeked.token != Token::ElseIf {
                        break;
                    }

                    self.consume_next_token(context);

                    if self.current_indent() != expected_indentation {
                        return syntax_error!(UnexpectedElseIfIndentation, self);
                    }

                    if let Some(else_if_condition) =
                        self.parse_expression(&mut ExpressionContext::inline())?
                    {
                        if let Some(else_if_block) = self.parse_indented_block()? {
                            else_if_blocks.push((else_if_condition, else_if_block));
                        } else {
                            return indentation_error!(ElseIfBlock, self);
                        }
                    } else {
                        return syntax_error!(ExpectedElseIfCondition, self);
                    }
                }

                let else_node = match self.peek_next_token(context) {
                    Some(peeked) if peeked.token == Token::Else => {
                        self.consume_next_token(context);

                        if self.current_indent() != expected_indentation {
                            return syntax_error!(UnexpectedElseIndentation, self);
                        }

                        if let Some(else_block) = self.parse_indented_block()? {
                            Some(else_block)
                        } else {
                            return indentation_error!(ElseBlock, self);
                        }
                    }
                    _ => None,
                };

                self.push_node(Node::If(AstIf {
                    condition,
                    then_node,
                    else_if_blocks,
                    else_node,
                }))?
            } else {
                return indentation_error!(ThenKeywordOrBlock, self);
            }
        };

        Ok(Some(result))
    }

    fn parse_switch_expression(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.consume_next_token(context) != Some(Token::Switch) {
            return Ok(None);
        }

        let current_indent = self.current_indent();
        let start_span = self.current_span();

        self.consume_until_next_token(context);

        if self.current_indent() <= current_indent {
            return indentation_error!(SwitchArm, self);
        }

        let mut arms = Vec::new();

        while self.peek_token().is_some() {
            let condition = self.parse_expression(&mut ExpressionContext::inline())?;

            let arm_body = match self.peek_next_token_on_same_line() {
                Some(Token::Else) => {
                    if condition.is_some() {
                        return syntax_error!(UnexpectedSwitchElse, self);
                    }

                    self.consume_next_token_on_same_line();

                    if let Some(expression) =
                        self.parse_expressions(&mut ExpressionContext::inline(), TempResult::No)?
                    {
                        expression
                    } else if let Some(indented_expression) = self.parse_indented_block()? {
                        indented_expression
                    } else {
                        return syntax_error!(ExpectedSwitchArmExpression, self);
                    }
                }
                Some(Token::Then) => {
                    self.consume_next_token_on_same_line();
                    match self
                        .parse_expressions(&mut ExpressionContext::inline(), TempResult::No)?
                    {
                        Some(expression) => expression,
                        None => return syntax_error!(ExpectedSwitchArmExpressionAfterThen, self),
                    }
                }
                _ => {
                    if let Some(indented_expression) = self.parse_indented_block()? {
                        indented_expression
                    } else {
                        return syntax_error!(ExpectedSwitchArmExpression, self);
                    }
                }
            };

            arms.push(SwitchArm {
                condition,
                expression: arm_body,
            });

            if self.peek_next_token(context).is_none() {
                break;
            }

            self.consume_until_next_token(context);
        }

        // Check for errors now that the match expression is complete
        for (arm_index, arm) in arms.iter().enumerate() {
            let last_arm = arm_index == arms.len() - 1;

            if arm.condition.is_none() && !last_arm {
                return Err(ParserError::new(
                    SyntaxError::SwitchElseNotInLastArm.into(),
                    start_span,
                ));
            }
        }

        Ok(Some(self.push_node_with_start_span(
            Node::Switch(arms),
            start_span,
        )?))
    }

    fn parse_match_expression(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.consume_next_token(context) != Some(Token::Match) {
            return Ok(None);
        }

        let current_indent = self.current_indent();
        let start_span = self.current_span();

        let match_expression =
            match self.parse_expressions(&mut ExpressionContext::inline(), TempResult::Yes)? {
                Some(expression) => expression,
                None => {
                    return syntax_error!(ExpectedMatchExpression, self);
                }
            };

        self.consume_until_next_token(context);

        if self.current_indent() <= current_indent {
            return indentation_error!(MatchArm, self);
        }

        let mut arms = Vec::new();

        while self.peek_token().is_some() {
            // Match patterns for a single arm, with alternatives separated by 'or'
            // e.g. match x, y
            //   0, 1 then ...
            //   2, 3 or 4, 5 then ...
            //   other then ...
            let mut arm_patterns = Vec::new();
            let mut expected_arm_count = 1;

            let condition = {
                while let Some(pattern) = self.parse_match_pattern(false)? {
                    // Match patterns, separated by commas in the case of matching multi-expressions
                    let mut patterns = vec![pattern];

                    while let Some(Token::Comma) = self.peek_next_token_on_same_line() {
                        self.consume_next_token_on_same_line();

                        match self.parse_match_pattern(false)? {
                            Some(pattern) => patterns.push(pattern),
                            None => return syntax_error!(ExpectedMatchPattern, self),
                        }
                    }

                    arm_patterns.push(match patterns.as_slice() {
                        [single_pattern] => *single_pattern,
                        _ => self.push_node(Node::TempTuple(patterns))?,
                    });

                    if let Some(Token::Or) = self.peek_next_token_on_same_line() {
                        self.consume_next_token_on_same_line();
                        expected_arm_count += 1;
                    }
                }

                if self.peek_next_token_on_same_line() == Some(Token::If) {
                    self.consume_next_token_on_same_line();

                    match self.parse_expression(&mut ExpressionContext::inline())? {
                        Some(expression) => Some(expression),
                        None => return syntax_error!(ExpectedMatchCondition, self),
                    }
                } else {
                    None
                }
            };

            let arm_body = match self.peek_next_token_on_same_line() {
                Some(Token::Else) => {
                    if !arm_patterns.is_empty() || condition.is_some() {
                        return syntax_error!(UnexpectedMatchElse, self);
                    }

                    self.consume_next_token_on_same_line();

                    if let Some(expression) =
                        self.parse_expressions(&mut ExpressionContext::inline(), TempResult::No)?
                    {
                        expression
                    } else if let Some(indented_expression) = self.parse_indented_block()? {
                        indented_expression
                    } else {
                        return syntax_error!(ExpectedMatchArmExpression, self);
                    }
                }
                Some(Token::Then) => {
                    if arm_patterns.len() != expected_arm_count {
                        return syntax_error!(ExpectedMatchPattern, self);
                    }

                    self.consume_next_token_on_same_line();
                    match self
                        .parse_expressions(&mut ExpressionContext::inline(), TempResult::No)?
                    {
                        Some(expression) => expression,
                        None => return syntax_error!(ExpectedMatchArmExpressionAfterThen, self),
                    }
                }
                Some(Token::If) => return syntax_error!(UnexpectedMatchIf, self),
                _ => {
                    if arm_patterns.len() != expected_arm_count {
                        return syntax_error!(ExpectedMatchPattern, self);
                    }

                    if let Some(indented_expression) = self.parse_indented_block()? {
                        indented_expression
                    } else {
                        return syntax_error!(ExpectedMatchArmExpression, self);
                    }
                }
            };

            arms.push(MatchArm {
                patterns: arm_patterns,
                condition,
                expression: arm_body,
            });

            if self.peek_next_token(context).is_none() {
                break;
            }

            self.consume_until_next_token(context);
        }

        // Check for errors now that the match expression is complete

        for (arm_index, arm) in arms.iter().enumerate() {
            let last_arm = arm_index == arms.len() - 1;

            if arm.patterns.is_empty() && arm.condition.is_none() && !last_arm {
                return Err(ParserError::new(
                    SyntaxError::MatchElseNotInLastArm.into(),
                    start_span,
                ));
            }
        }

        Ok(Some(self.push_node_with_start_span(
            Node::Match {
                expression: match_expression,
                arms,
            },
            start_span,
        )?))
    }

    fn parse_match_pattern(
        &mut self,
        in_nested_patterns: bool,
    ) -> Result<Option<AstIndex>, ParserError> {
        use Token::*;

        let mut pattern_context = ExpressionContext::restricted();

        let result = match self.peek_next_token(&pattern_context) {
            Some(peeked) => match peeked.token {
                True | False | Number | SingleQuote | DoubleQuote | Subtract => {
                    return self.parse_term(&mut pattern_context)
                }
                Id => match self.parse_id(&mut pattern_context)? {
                    Some(id) => {
                        let result = if self.peek_token() == Some(Ellipsis) {
                            self.consume_token();
                            if in_nested_patterns {
                                self.frame_mut()?.ids_assigned_in_scope.insert(id);
                                self.push_node(Node::Ellipsis(Some(id)))?
                            } else {
                                return syntax_error!(MatchEllipsisOutsideOfNestedPatterns, self);
                            }
                        } else {
                            let id_node = self.push_node(Node::Id(id))?;
                            if self.next_token_is_lookup_start(&pattern_context) {
                                self.frame_mut()?.add_id_access(id);
                                self.parse_lookup(id_node, &mut pattern_context)?
                            } else {
                                self.frame_mut()?.ids_assigned_in_scope.insert(id);
                                id_node
                            }
                        };
                        Some(result)
                    }
                    None => return internal_error!(IdParseFailure, self),
                },
                Wildcard => {
                    self.consume_next_token(&mut pattern_context);
                    Some(self.push_node(Node::Wildcard)?)
                }
                SquareOpen => {
                    self.consume_next_token(&mut pattern_context);

                    let list_patterns = self.parse_nested_match_patterns()?;

                    if self.consume_next_token_on_same_line() != Some(SquareClose) {
                        return syntax_error!(ExpectedListEnd, self);
                    }

                    Some(self.push_node(Node::List(list_patterns))?)
                }
                RoundOpen => {
                    self.consume_next_token(&mut pattern_context);

                    if self.peek_token() == Some(RoundClose) {
                        self.consume_token();
                        Some(self.push_node(Node::Empty)?)
                    } else {
                        let tuple_patterns = self.parse_nested_match_patterns()?;

                        if self.consume_next_token_on_same_line() != Some(RoundClose) {
                            return syntax_error!(ExpectedCloseParen, self);
                        }

                        Some(self.push_node(Node::Tuple(tuple_patterns))?)
                    }
                }
                Ellipsis if in_nested_patterns => {
                    self.consume_next_token(&mut pattern_context);
                    Some(self.push_node(Node::Ellipsis(None))?)
                }
                _ => None,
            },
            None => None,
        };

        Ok(result)
    }

    fn parse_nested_match_patterns(&mut self) -> Result<Vec<AstIndex>, ParserError> {
        let mut result = vec![];

        while let Some(pattern) = self.parse_match_pattern(true)? {
            result.push(pattern);

            if self.peek_next_token_on_same_line() != Some(Token::Comma) {
                break;
            }
            self.consume_next_token_on_same_line();
        }

        Ok(result)
    }

    fn parse_import_expression(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        let from_import = match self.consume_next_token(context) {
            Some(Token::From) => true,
            Some(Token::Import) => false,
            _ => return internal_error!(UnexpectedToken, self),
        };

        let start_span = self.current_span();

        let from = if from_import {
            let from = match self.consume_import_items()?.as_slice() {
                [from] => from.clone(),
                _ => return syntax_error!(ImportFromExpressionHasTooManyItems, self),
            };

            if self.peek_next_token_on_same_line() != Some(Token::Import) {
                return syntax_error!(ExpectedImportKeywordAfterFrom, self);
            }
            self.consume_next_token_on_same_line();

            from
        } else {
            vec![]
        };

        let items = self.consume_import_items()?;

        if let Some(token) = self.peek_next_token_on_same_line() {
            if !token.is_newline() {
                self.consume_next_token_on_same_line();
                return syntax_error!(UnexpectedTokenInImportExpression, self);
            }
        }

        // Mark any imported ids as locally assigned
        for item in items.iter() {
            match item.last() {
                Some(ImportItemNode::Id(id)) => {
                    self.frame_mut()?.ids_assigned_in_scope.insert(*id);
                }
                Some(ImportItemNode::Str(_)) => {}
                None => return internal_error!(ExpectedIdInImportItem, self),
            };
        }

        Ok(Some(self.push_node_with_start_span(
            Node::Import { from, items },
            start_span,
        )?))
    }

    fn parse_try_expression(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        if self.consume_next_token(context) != Some(Token::Try) {
            return internal_error!(UnexpectedToken, self);
        }

        context.expected_indentation = Indentation::Equal(self.current_indent());

        let start_span = self.current_span();

        let try_block = if let Some(try_block) = self.parse_indented_block()? {
            try_block
        } else {
            return indentation_error!(TryBody, self);
        };

        match self.peek_next_token(context) {
            Some(peeked) if peeked.token == Token::Catch => {}
            _ => return syntax_error!(ExpectedCatch, self),
        }

        self.consume_next_token(context);

        let catch_arg = if let Some(catch_arg) =
            self.parse_id_or_wildcard(&mut ExpressionContext::restricted())?
        {
            match catch_arg {
                ConstantIndexOrWildcard::Index(id_index) => {
                    self.frame_mut()?.ids_assigned_in_scope.insert(id_index);
                    Some(id_index)
                }

                ConstantIndexOrWildcard::Wildcard => None,
            }
        } else {
            return syntax_error!(ExpectedCatchArgument, self);
        };

        let catch_block = if let Some(catch_block) = self.parse_indented_block()? {
            catch_block
        } else {
            return indentation_error!(CatchBody, self);
        };

        let finally_block = match self.peek_next_token(context) {
            Some(peeked) if peeked.token == Token::Finally => {
                self.consume_next_token(context);
                if let Some(finally_block) = self.parse_indented_block()? {
                    Some(finally_block)
                } else {
                    return indentation_error!(FinallyBody, self);
                }
            }
            _ => None,
        };

        let result = self.push_node_with_start_span(
            Node::Try(AstTry {
                try_block,
                catch_arg,
                catch_block,
                finally_block,
            }),
            start_span,
        )?;

        Ok(Some(result))
    }

    fn consume_import_items(&mut self) -> Result<Vec<Vec<ImportItemNode>>, ParserError> {
        let mut items = vec![];
        let mut item_context = ExpressionContext::permissive();

        loop {
            let item_root = match self.parse_id(&mut item_context)? {
                Some(id) => ImportItemNode::Id(id),
                None => match self.parse_string(&mut item_context)? {
                    Some((import_string, _span)) => ImportItemNode::Str(import_string),
                    None => break,
                },
            };

            let mut item = vec![item_root];

            while self.peek_token() == Some(Token::Dot) {
                self.consume_token();

                match self.parse_id(&mut ExpressionContext::restricted())? {
                    Some(id) => item.push(ImportItemNode::Id(id)),
                    None => match self.parse_string(&mut ExpressionContext::restricted())? {
                        Some((node_string, _span)) => {
                            item.push(ImportItemNode::Str(node_string));
                        }
                        None => return syntax_error!(ExpectedImportModuleId, self),
                    },
                }
            }

            items.push(item);

            if self.peek_next_token_on_same_line() != Some(Token::Comma) {
                break;
            }
            self.consume_next_token_on_same_line();
        }

        if items.is_empty() {
            return syntax_error!(ExpectedIdInImportExpression, self);
        }

        Ok(items)
    }

    fn parse_indented_block(&mut self) -> Result<Option<AstIndex>, ParserError> {
        let mut block_context = ExpressionContext::permissive();

        let start_indent = self.current_indent();
        match self.peek_next_token(&block_context) {
            Some(peeked) if peeked.indent > start_indent => {}
            _ => return Ok(None),
        }

        self.consume_until_next_token(&mut block_context);
        let start_span = self.current_span();

        let mut block = Vec::new();
        loop {
            let mut line_context = ExpressionContext::permissive();

            if block.is_empty() {
                line_context.allow_map_block = true;
            }

            if let Some(expression) = self.parse_line(&mut line_context)? {
                block.push(expression);

                match self.peek_next_token_on_same_line() {
                    None => break,
                    Some(Token::NewLine) | Some(Token::NewLineIndented) => {}
                    _ => {
                        self.consume_next_token_on_same_line();
                        return syntax_error!(UnexpectedToken, self);
                    }
                }

                // Peek ahead to see if the indented block continues after this line
                if self.peek_next_token(&block_context).is_none() {
                    break;
                }

                self.consume_until_next_token(&mut block_context);
            } else {
                break;
            }
        }

        // If the block is a single expression then it doesn't need to be wrapped in a Block node
        if block.len() == 1 {
            Ok(Some(*block.first().unwrap()))
        } else {
            Ok(Some(self.push_node_with_start_span(
                Node::Block(block),
                start_span,
            )?))
        }
    }

    fn parse_nested_expressions(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<AstIndex>, ParserError> {
        use Token::*;

        if self.consume_next_token(context) != Some(RoundOpen) {
            return internal_error!(UnexpectedToken, self);
        }

        let expression_context = ExpressionContext {
            allow_space_separated_call: true,
            ..*context
        };
        let mut expressions = vec![];
        let mut encountered_comma = false;
        while let Some(expression) = self.parse_expression(&mut expression_context.clone())? {
            expressions.push(expression);

            if self.peek_next_token_on_same_line() == Some(Token::Comma) {
                self.consume_next_token_on_same_line();
                encountered_comma = true;
            } else {
                break;
            }
        }

        let expressions_node = match expressions.as_slice() {
            [] => self.push_node(Node::Empty)?,
            [single_expression] if !encountered_comma => {
                self.push_node(Node::Nested(*single_expression))?
            }
            _ => self.push_node(Node::Tuple(expressions))?,
        };

        if let Some(RoundClose) = self.peek_token() {
            self.consume_token();
            let result = self.check_for_lookup_after_node(expressions_node, context)?;
            Ok(Some(result))
        } else {
            syntax_error!(ExpectedCloseParen, self)
        }
    }

    fn parse_string(
        &mut self,
        context: &mut ExpressionContext,
    ) -> Result<Option<(AstString, Span)>, ParserError> {
        use Token::*;

        match self.peek_next_token(context) {
            Some(PeekInfo {
                token: SingleQuote | DoubleQuote,
                ..
            }) => {}
            _ => return Ok(None),
        }

        let string_quote = self.consume_next_token(context).unwrap();

        let start_span = self.current_span();

        let mut nodes = Vec::new();

        while let Some(next_token) = self.consume_token() {
            match next_token {
                StringLiteral => {
                    let string_literal = self.lexer.slice();

                    let mut literal = String::with_capacity(string_literal.len());
                    let mut chars = string_literal.chars().peekable();

                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => match chars.next() {
                                Some('\n') | Some('\r') => {
                                    while let Some(c) = chars.peek() {
                                        if c.is_whitespace() {
                                            chars.next();
                                        } else {
                                            break;
                                        }
                                    }
                                }
                                Some('\\') => literal.push('\\'),
                                Some('\'') => literal.push('\''),
                                Some('"') => literal.push('"'),
                                Some('n') => literal.push('\n'),
                                Some('r') => literal.push('\r'),
                                Some('t') => literal.push('\t'),
                                Some('x') => match chars.next() {
                                    Some(c1) if c1.is_ascii_hexdigit() => match chars.next() {
                                        Some(c2) if c2.is_ascii_hexdigit() => {
                                            // is_ascii_hexdigit already checked
                                            let d1 = c1.to_digit(16).unwrap();
                                            let d2 = c2.to_digit(16).unwrap();
                                            let d = d1 * 16 + d2;
                                            if d <= 0x7f {
                                                literal.push(char::from_u32(d).unwrap());
                                            } else {
                                                return syntax_error!(
                                                    AsciiEscapeCodeOutOfRange,
                                                    self
                                                );
                                            }
                                        }
                                        Some(_) => {
                                            return syntax_error!(
                                                UnexpectedCharInNumericEscapeCode,
                                                self
                                            )
                                        }
                                        None => {
                                            return syntax_error!(
                                                UnterminatedNumericEscapeCode,
                                                self
                                            )
                                        }
                                    },
                                    Some(_) => {
                                        return syntax_error!(
                                            UnexpectedCharInNumericEscapeCode,
                                            self
                                        )
                                    }
                                    None => {
                                        return syntax_error!(UnterminatedNumericEscapeCode, self)
                                    }
                                },
                                Some('u') => match chars.next() {
                                    Some('{') => {
                                        let mut code = 0;

                                        while let Some(c) = chars.peek().cloned() {
                                            if c.is_ascii_hexdigit() {
                                                chars.next();
                                                code *= 16;
                                                code += c.to_digit(16).unwrap();
                                            } else {
                                                break;
                                            }
                                        }

                                        match chars.next() {
                                            Some('}') => match char::from_u32(code) {
                                                Some(c) => literal.push(c),
                                                None => {
                                                    return syntax_error!(
                                                        UnicodeEscapeCodeOutOfRange,
                                                        self
                                                    );
                                                }
                                            },
                                            Some(_) => {
                                                return syntax_error!(
                                                    UnexpectedCharInNumericEscapeCode,
                                                    self
                                                );
                                            }
                                            None => {
                                                return syntax_error!(
                                                    UnterminatedNumericEscapeCode,
                                                    self
                                                )
                                            }
                                        }
                                    }
                                    Some(_) => {
                                        return syntax_error!(
                                            UnexpectedCharInNumericEscapeCode,
                                            self
                                        )
                                    }
                                    None => {
                                        return syntax_error!(UnterminatedNumericEscapeCode, self)
                                    }
                                },
                                _ => return syntax_error!(UnexpectedEscapeInString, self),
                            },
                            _ => literal.push(c),
                        }
                    }

                    nodes.push(StringNode::Literal(self.add_string_constant(&literal)?));
                }
                Dollar => match self.peek_token() {
                    Some(Id) => {
                        self.consume_token();
                        let id = self.add_string_constant(self.lexer.slice())?;
                        self.frame_mut()?.add_id_access(id);
                        let id_node = self.push_node(Node::Id(id))?;
                        nodes.push(StringNode::Expr(id_node));
                    }
                    Some(CurlyOpen) => {
                        self.consume_token();

                        if let Some(expression) = self
                            .parse_expressions(&mut ExpressionContext::inline(), TempResult::No)?
                        {
                            nodes.push(StringNode::Expr(expression));
                        } else {
                            return syntax_error!(ExpectedExpression, self);
                        }

                        if self.consume_token() != Some(CurlyClose) {
                            return syntax_error!(ExpectedStringPlaceholderEnd, self);
                        }
                    }
                    Some(_) => {
                        return syntax_error!(UnexpectedTokenAfterDollarInString, self);
                    }
                    None => break,
                },
                c if c == string_quote => {
                    let quotation_mark = if string_quote == SingleQuote {
                        QuotationMark::Single
                    } else {
                        QuotationMark::Double
                    };

                    if nodes.is_empty() {
                        nodes.push(StringNode::Literal(self.add_string_constant("")?));
                    }

                    return Ok(Some((
                        AstString {
                            quotation_mark,
                            nodes,
                        },
                        self.span_with_start(start_span),
                    )));
                }
                _ => return syntax_error!(UnexpectedToken, self),
            }
        }

        syntax_error!(UnterminatedString, self)
    }

    fn push_ast_op(
        &mut self,
        op: Token,
        lhs: AstIndex,
        rhs: AstIndex,
    ) -> Result<AstIndex, ParserError> {
        use Token::*;
        let ast_op = match op {
            Add => AstBinaryOp::Add,
            Subtract => AstBinaryOp::Subtract,
            Multiply => AstBinaryOp::Multiply,
            Divide => AstBinaryOp::Divide,
            Modulo => AstBinaryOp::Modulo,

            Equal => AstBinaryOp::Equal,
            NotEqual => AstBinaryOp::NotEqual,

            Greater => AstBinaryOp::Greater,
            GreaterOrEqual => AstBinaryOp::GreaterOrEqual,
            Less => AstBinaryOp::Less,
            LessOrEqual => AstBinaryOp::LessOrEqual,

            And => AstBinaryOp::And,
            Or => AstBinaryOp::Or,

            Pipe => AstBinaryOp::Pipe,

            _ => unreachable!(),
        };
        self.push_node(Node::BinaryOp {
            op: ast_op,
            lhs,
            rhs,
        })
    }

    fn next_token_is_lookup_start(&mut self, context: &ExpressionContext) -> bool {
        use Token::*;

        if matches!(
            self.peek_token(),
            Some(Dot) | Some(SquareOpen) | Some(RoundOpen)
        ) {
            return true;
        } else if context.allow_linebreaks {
            let start_line = self.current_line_number();
            let start_indent = self.current_indent();
            if let Some(peeked) = self.peek_next_token(context) {
                if peeked.line > start_line && peeked.indent > start_indent {
                    return peeked.token == Dot;
                }
            }
        }

        false
    }

    fn current_line_number(&self) -> u32 {
        self.lexer.line_number()
    }

    fn current_indent(&self) -> usize {
        self.lexer.current_indent()
    }

    fn current_span(&self) -> Span {
        self.lexer.span()
    }

    fn span_with_start(&self, start_span: Span) -> Span {
        Span {
            start: start_span.start,
            end: self.current_span().end,
        }
    }

    fn peek_token(&mut self) -> Option<Token> {
        self.lexer.peek()
    }

    fn peek_token_n(&mut self, n: usize) -> Option<Token> {
        self.lexer.peek_n(n)
    }

    fn consume_token(&mut self) -> Option<Token> {
        self.lexer.next()
    }

    fn push_node(&mut self, node: Node) -> Result<AstIndex, ParserError> {
        self.push_node_with_span(node, self.current_span())
    }

    fn push_node_with_start_span(
        &mut self,
        node: Node,
        start_span: Span,
    ) -> Result<AstIndex, ParserError> {
        self.push_node_with_span(node, self.span_with_start(start_span))
    }

    fn push_node_with_span(&mut self, node: Node, span: Span) -> Result<AstIndex, ParserError> {
        self.ast.push(node, span)
    }

    // Peeks past whitespace, comments, and newlines until the next token is found
    //
    // Tokens on following lines will only be returned if the expression context allows linebreaks.
    //
    // If expected indentation is specified in the expression context, then the next token
    // needs to have matching indentation, otherwise None is returned.
    fn peek_next_token(&mut self, context: &ExpressionContext) -> Option<PeekInfo> {
        use Token::*;

        let mut peek_count = 0;
        let start_line = self.current_line_number();
        let start_indent = self.current_indent();

        while let Some(peeked) = self.peek_token_n(peek_count) {
            match peeked {
                Whitespace | NewLine | NewLineIndented | CommentMulti | CommentSingle => {}
                token => {
                    return match self.lexer.peek_line_number(peek_count) {
                        peeked_line if peeked_line == start_line => Some(PeekInfo {
                            token,
                            line: start_line,
                            indent: start_indent,
                            peek_count,
                        }),
                        peeked_line if context.allow_linebreaks => {
                            let peeked_indent = self.lexer.peek_indent(peek_count);
                            let peek_info = PeekInfo {
                                token,
                                line: peeked_line,
                                indent: peeked_indent,
                                peek_count,
                            };
                            use Indentation::*;
                            match context.expected_indentation {
                                GreaterOrEqual(expected_indent)
                                    if peeked_indent >= expected_indent =>
                                {
                                    Some(peek_info)
                                }
                                Equal(expected_indent) if peeked_indent == expected_indent => {
                                    Some(peek_info)
                                }
                                Greater if peeked_indent > start_indent => Some(peek_info),
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                }
            }

            peek_count += 1;
        }

        None
    }

    // Consumes whitespace, comments, and newlines up until the next token
    //
    // If the expression context's indentation is set to Greater, and indentation is found,
    // then the context will be updated to expect the new indentation.
    //
    // It's expected that a peek has been performed to check that the current expression context
    // allows for the token to be consumed, see peek_next_token().
    fn consume_next_token(&mut self, context: &mut ExpressionContext) -> Option<Token> {
        let start_line = self.current_line_number();

        for token in &mut self.lexer {
            match token {
                token if token.is_whitespace() || token.is_newline() => {}
                token => {
                    if self.current_line_number() > start_line
                        && context.allow_linebreaks
                        && matches!(context.expected_indentation, Indentation::Greater)
                    {
                        context.expected_indentation = Indentation::Equal(self.current_indent());
                        context.allow_map_block = true;
                    }

                    return Some(token);
                }
            }
        }

        None
    }

    // Consumes whitespace, comments, and newlines up until the next token
    //
    // If the expression context's indentation is None, and indentation is found, then the
    // context will be updated to expect the new indentation.
    //
    // It's expected that a peek has been performed to check that the current expression context
    // allows for the token to be consumed, see peek_next_token().
    fn consume_until_next_token(&mut self, context: &mut ExpressionContext) -> Option<Token> {
        let start_line = self.current_line_number();

        while let Some(peeked) = self.peek_token_n(0) {
            match peeked {
                token if token.is_whitespace() || token.is_newline() => {}
                token => {
                    if self.lexer.peek_line_number(0) > start_line
                        && context.allow_linebreaks
                        && matches!(context.expected_indentation, Indentation::Greater)
                    {
                        context.expected_indentation =
                            Indentation::Equal(self.lexer.peek_indent(0));
                        context.allow_map_block = true;
                    }

                    return Some(token);
                }
            }

            self.lexer.next();
        }

        None
    }

    // Peeks past whitespace on the same line until the next token is found
    fn peek_next_token_on_same_line(&mut self) -> Option<Token> {
        let mut peek_count = 0;

        while let Some(peeked) = self.peek_token_n(peek_count) {
            match peeked {
                token if token.is_whitespace() => {}
                token => return Some(token),
            }

            peek_count += 1;
        }

        None
    }

    // Consumes whitespace on the same line up until the next token
    fn consume_until_next_token_on_same_line(&mut self) {
        while let Some(peeked) = self.peek_token() {
            match peeked {
                token if token.is_whitespace() => {}
                _ => return,
            }

            self.lexer.next();
        }
    }

    // Consumes whitespace on the same line and returns the next token
    fn consume_next_token_on_same_line(&mut self) -> Option<Token> {
        while let Some(peeked) = self.peek_token() {
            match peeked {
                token if token.is_whitespace() => {}
                _ => return self.lexer.next(),
            }

            self.lexer.next();
        }

        None
    }
}

enum TempResult {
    No,
    Yes,
}

// The first operator that's above the pipe operator >> in precedence.
// Q: Why is this needed?
// A: Function calls without parentheses aren't currently treated as operators (a Call operator
//    with higher precedence than Pipe would allow this to go away, but would likely take quite a
//    bit of reworking. All calls to parse_call_args will need to reworked).
//    parse_call_args needs to parse arguments as expressions with a minimum precedence that
//    excludes piping, otherwise `f g >> x` would be parsed as `f (g >> x)` instead of `(f g) >> x`.
const MIN_PRECEDENCE_AFTER_PIPE: u8 = 3;

fn operator_precedence(op: Token) -> Option<(u8, u8)> {
    use Token::*;
    let priority = match op {
        Pipe => (1, 2),
        Or => (MIN_PRECEDENCE_AFTER_PIPE, 4),
        And => (5, 6),
        // Chained comparisons require right-associativity
        Equal | NotEqual => (8, 7),
        Greater | GreaterOrEqual | Less | LessOrEqual => (10, 9),
        Add | Subtract => (11, 12),
        Multiply | Divide | Modulo => (13, 14),
        _ => return None,
    };
    Some(priority)
}

#[derive(Debug)]
struct PeekInfo {
    token: Token,
    line: u32,
    indent: usize,
    peek_count: usize,
}
