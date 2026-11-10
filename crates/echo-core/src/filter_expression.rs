//! GPUI-free filter expression parser and evaluator.

use regex::Regex;

const MAX_FILTER_NESTING: usize = 64;
const MAX_FILTER_DEPTH: usize = 256;
const MAX_FILTER_CHAIN_TERMS: usize = 4096;

/// The message, author and channel fields a filter expression is evaluated
/// against.
#[derive(Debug, Clone, Default)]
pub struct FilterContext {
    pub content: String,
    pub author: String,
    pub author_login: String,
    pub author_user_id: String,
    pub author_color: Option<String>,
    pub moderator_name: String,
    pub moderator_login: String,
    pub moderator_user_id: String,
    pub target_name: String,
    pub target_login: String,
    pub target_user_id: String,
    pub badges: Vec<String>,
    pub external_badges: Vec<String>,
    pub sub_length: Option<u32>,
    pub channel_name: String,
    pub bits_amount: u32,
    pub reward_title: String,
    pub reward_cost: u32,
    pub reward_id: String,
    pub event_action: String,
    pub event_reason: String,
    pub event_source: String,
    pub event_status: String,
    pub event_title: String,
    pub event_detail: String,
    pub is_sub: bool,
    pub is_first_msg: bool,
    pub is_returning_chatter: bool,
    pub is_action: bool,
    pub is_reply: bool,
    pub is_whisper: bool,
    pub is_announcement: bool,
    pub is_system: bool,
    pub is_highlighted: bool,
    pub is_cheer: bool,
    pub is_shared_chat: bool,
    pub is_automod: bool,
    pub is_points_redeemed: bool,
    pub is_reward_message: bool,
    pub is_restricted: bool,
    pub is_monitored: bool,
    pub is_sub_message: bool,
    pub is_elevated_message: bool,
    pub is_hype_chat: bool,
    pub is_similar: bool,
    pub is_watch_streak: bool,
    pub channel_watching: bool,
    pub channel_live: bool,
}

#[derive(Debug, Clone)]
pub struct FilterExpression {
    expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterEvaluation {
    pub matched: bool,
    pub matched_text: Option<String>,
}

impl FilterExpression {
    pub fn parse(source: &str) -> Result<Self, String> {
        Parser::new(source)
            .and_then(|mut parser| parser.parse())
            .map(|expr| Self { expr })
    }

    pub fn evaluate(&self, context: &FilterContext) -> Result<FilterEvaluation, String> {
        let mut evaluator = Evaluator::new(context);
        let value = evaluator.eval(&self.expr)?;
        Ok(match value {
            FilterValue::Bool(matched) => FilterEvaluation {
                matched,
                matched_text: evaluator.matched_text,
            },
            _ => FilterEvaluation {
                matched: false,
                matched_text: None,
            },
        })
    }
}

#[derive(Debug, Clone)]
enum Expr {
    Literal(FilterValue),
    Variable(String),
    List(Vec<Expr>),
    Not(Box<Expr>),
    /// A flat `&&` or `||` run, evaluated left to right with short-circuit, so
    /// long saved chains cost no recursion depth.
    Chain {
        op: BinaryOp,
        terms: Vec<Expr>,
    },
    Binary {
        lhs: Box<Expr>,
        op: BinaryOp,
        rhs: Box<Expr>,
    },
}

#[derive(Debug, Clone)]
enum FilterValue {
    Bool(bool),
    Int(i64),
    String(String),
    Regex(RegexLiteral),
    List(Vec<FilterValue>),
    Missing,
}

impl FilterValue {
    fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn display_string(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Regex(regex) => regex.pattern.clone(),
            Self::List(values) => values
                .iter()
                .map(FilterValue::display_string)
                .collect::<Vec<_>>()
                .join(","),
            Self::Missing => String::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct RegexLiteral {
    pattern: String,
    regex: Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryOp {
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    StartsWith,
    EndsWith,
    Match,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

struct Evaluator<'a> {
    ctx: &'a FilterContext,
    matched_text: Option<String>,
}

impl<'a> Evaluator<'a> {
    fn new(ctx: &'a FilterContext) -> Self {
        Self {
            ctx,
            matched_text: None,
        }
    }

    fn eval(&mut self, expr: &Expr) -> Result<FilterValue, String> {
        match expr {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Variable(name) => Ok(self.variable_value(name)),
            Expr::List(values) => {
                let mut evaluated = Vec::with_capacity(values.len());
                for value in values {
                    evaluated.push(self.eval(value)?);
                }
                Ok(FilterValue::List(evaluated))
            }
            Expr::Not(value) => {
                let value = self.eval(value)?;
                Ok(FilterValue::Bool(!value.as_bool().unwrap_or(false)))
            }
            Expr::Chain { op, terms } => {
                let short_circuit = *op == BinaryOp::Or;
                for term in terms {
                    if self.eval(term)?.as_bool().unwrap_or(false) == short_circuit {
                        return Ok(FilterValue::Bool(short_circuit));
                    }
                }
                Ok(FilterValue::Bool(!short_circuit))
            }
            Expr::Binary { lhs, op, rhs } => match op {
                BinaryOp::And => {
                    let lhs = self.eval(lhs)?.as_bool().unwrap_or(false);
                    if !lhs {
                        return Ok(FilterValue::Bool(false));
                    }
                    let rhs = self.eval(rhs)?.as_bool().unwrap_or(false);
                    Ok(FilterValue::Bool(rhs))
                }
                BinaryOp::Or => {
                    let lhs = self.eval(lhs)?.as_bool().unwrap_or(false);
                    if lhs {
                        return Ok(FilterValue::Bool(true));
                    }
                    let rhs = self.eval(rhs)?.as_bool().unwrap_or(false);
                    Ok(FilterValue::Bool(rhs))
                }
                _ => {
                    let lhs = self.eval(lhs)?;
                    let rhs = self.eval(rhs)?;
                    self.eval_binary(lhs, *op, rhs)
                }
            },
        }
    }

    fn eval_binary(
        &mut self,
        lhs: FilterValue,
        op: BinaryOp,
        rhs: FilterValue,
    ) -> Result<FilterValue, String> {
        let value = match op {
            BinaryOp::Eq => FilterValue::Bool(values_equal(&lhs, &rhs)),
            BinaryOp::Ne => FilterValue::Bool(!values_equal(&lhs, &rhs)),
            BinaryOp::Lt => compare_int(lhs, rhs, |left, right| left < right),
            BinaryOp::Le => compare_int(lhs, rhs, |left, right| left <= right),
            BinaryOp::Gt => compare_int(lhs, rhs, |left, right| left > right),
            BinaryOp::Ge => compare_int(lhs, rhs, |left, right| left >= right),
            BinaryOp::Contains => self.contains(lhs, rhs),
            BinaryOp::StartsWith => self.starts_with(lhs, rhs),
            BinaryOp::EndsWith => self.ends_with(lhs, rhs),
            BinaryOp::Match => self.matches(lhs, rhs),
            BinaryOp::Add => match (&lhs, &rhs) {
                (FilterValue::Int(left), FilterValue::Int(right)) => left
                    .checked_add(*right)
                    .map_or(FilterValue::Missing, FilterValue::Int),
                (FilterValue::String(left), _) => {
                    FilterValue::String(format!("{}{}", left, rhs.display_string()))
                }
                _ => FilterValue::Missing,
            },
            BinaryOp::Sub => arithmetic(lhs, rhs, i64::checked_sub),
            BinaryOp::Mul => arithmetic(lhs, rhs, i64::checked_mul),
            BinaryOp::Div => arithmetic(lhs, rhs, i64::checked_div),
            BinaryOp::Mod => arithmetic(lhs, rhs, i64::checked_rem),
            BinaryOp::And | BinaryOp::Or => unreachable!("handled before eager evaluation"),
        };

        Ok(value)
    }

    fn contains(&mut self, lhs: FilterValue, rhs: FilterValue) -> FilterValue {
        match (lhs, rhs) {
            (FilterValue::String(text), FilterValue::String(needle)) => {
                let matched = contains_ignore_ascii_case(&text, &needle);
                if matched {
                    self.matched_text.get_or_insert(needle);
                }
                FilterValue::Bool(matched)
            }
            (FilterValue::List(values), needle) => FilterValue::Bool(
                values
                    .iter()
                    .any(|value| values_equal_loose(value, &needle)),
            ),
            _ => FilterValue::Bool(false),
        }
    }

    fn starts_with(&mut self, lhs: FilterValue, rhs: FilterValue) -> FilterValue {
        match (lhs, rhs) {
            (FilterValue::String(text), FilterValue::String(needle)) => {
                let matched = starts_with_ignore_ascii_case(&text, &needle);
                if matched {
                    self.matched_text.get_or_insert(needle);
                }
                FilterValue::Bool(matched)
            }
            (FilterValue::List(values), needle) => FilterValue::Bool(
                values
                    .first()
                    .map(|value| values_equal_loose(value, &needle))
                    .unwrap_or(false),
            ),
            _ => FilterValue::Bool(false),
        }
    }

    fn ends_with(&mut self, lhs: FilterValue, rhs: FilterValue) -> FilterValue {
        match (lhs, rhs) {
            (FilterValue::String(text), FilterValue::String(needle)) => {
                let matched = ends_with_ignore_ascii_case(&text, &needle);
                if matched {
                    self.matched_text.get_or_insert(needle);
                }
                FilterValue::Bool(matched)
            }
            (FilterValue::List(values), needle) => FilterValue::Bool(
                values
                    .last()
                    .map(|value| values_equal_loose(value, &needle))
                    .unwrap_or(false),
            ),
            _ => FilterValue::Bool(false),
        }
    }

    fn matches(&mut self, lhs: FilterValue, rhs: FilterValue) -> FilterValue {
        let Some(text) = lhs.as_string() else {
            return FilterValue::Bool(false);
        };

        match rhs {
            FilterValue::Regex(regex) => {
                let matched = regex.regex.find(text);
                if let Some(matched) = matched {
                    self.matched_text
                        .get_or_insert_with(|| matched.as_str().to_string());
                    FilterValue::Bool(true)
                } else {
                    FilterValue::Bool(false)
                }
            }
            FilterValue::List(values) if values.len() == 2 => {
                let Some(FilterValue::Regex(regex)) = values.first() else {
                    return FilterValue::Bool(false);
                };
                let Some(index) = values.get(1).and_then(FilterValue::as_int) else {
                    return FilterValue::Bool(false);
                };
                let Some(captures) = regex.regex.captures(text) else {
                    return FilterValue::Bool(false);
                };
                let captured = captures
                    .get(index.max(0) as usize)
                    .map(|capture| capture.as_str().to_string())
                    .unwrap_or_default();
                if !captured.is_empty() {
                    self.matched_text.get_or_insert_with(|| captured.clone());
                }
                FilterValue::String(captured)
            }
            _ => FilterValue::Bool(false),
        }
    }

    fn variable_value(&self, name: &str) -> FilterValue {
        match name {
            "author.badges" | "author.badge" => FilterValue::List(
                self.ctx
                    .badges
                    .iter()
                    .map(|badge| FilterValue::String(badge.clone()))
                    .collect(),
            ),
            "author.external_badges" | "author.external_badge" => FilterValue::List(
                self.ctx
                    .external_badges
                    .iter()
                    .map(|badge| FilterValue::String(badge.clone()))
                    .collect(),
            ),
            "author.color" => {
                FilterValue::String(self.ctx.author_color.clone().unwrap_or_default())
            }
            "author.name" | "author.display_name" => FilterValue::String(self.ctx.author.clone()),
            "author.login" | "author.username" => {
                FilterValue::String(if self.ctx.author_login.is_empty() {
                    self.ctx.author.clone()
                } else {
                    self.ctx.author_login.clone()
                })
            }
            "author.user_id" | "author.id" => FilterValue::String(self.ctx.author_user_id.clone()),
            "moderator.name" | "moderation.moderator" | "moderation.moderator_name" => {
                FilterValue::String(self.ctx.moderator_name.clone())
            }
            "moderator.login" | "moderation.moderator_login" => {
                FilterValue::String(self.ctx.moderator_login.clone())
            }
            "moderator.user_id" | "moderation.moderator_user_id" => {
                FilterValue::String(self.ctx.moderator_user_id.clone())
            }
            "target.name" | "moderation.target" | "moderation.target_name" => {
                FilterValue::String(self.ctx.target_name.clone())
            }
            "target.login" | "moderation.target_login" => {
                FilterValue::String(self.ctx.target_login.clone())
            }
            "target.user_id" | "moderation.target_user_id" => {
                FilterValue::String(self.ctx.target_user_id.clone())
            }
            "author.no_color" => {
                FilterValue::Bool(self.ctx.author_color.as_deref().unwrap_or("").is_empty())
            }
            "author.subbed" | "author.subscribed" | "author.subscriber" => {
                FilterValue::Bool(self.ctx.is_sub)
            }
            "author.sub_length" => FilterValue::Int(self.ctx.sub_length.unwrap_or(0) as i64),
            "bits.amount" | "bits" => FilterValue::Int(self.ctx.bits_amount as i64),
            "channel.name" | "channel" => FilterValue::String(self.ctx.channel_name.clone()),
            "channel.watching" => FilterValue::Bool(self.ctx.channel_watching),
            "channel.live" => FilterValue::Bool(self.ctx.channel_live),
            "flags.action" => FilterValue::Bool(self.ctx.is_action),
            "flags.automod" => FilterValue::Bool(self.ctx.is_automod),
            "flags.cheer_message" | "flags.cheer" | "flags.bits" => {
                FilterValue::Bool(self.ctx.is_cheer || self.ctx.bits_amount > 0)
            }
            "flags.first_message" | "flags.first_msg" => FilterValue::Bool(self.ctx.is_first_msg),
            "flags.elevated_message" | "flags.hype_chat" => {
                FilterValue::Bool(self.ctx.is_elevated_message || self.ctx.is_hype_chat)
            }
            "flags.highlighted" | "flags.highlight" => FilterValue::Bool(self.ctx.is_highlighted),
            "flags.points_redeemed"
            | "flags.channel_points"
            | "flags.channel_point_redeemed"
            | "flags.redemption" => FilterValue::Bool(self.ctx.is_points_redeemed),
            "flags.reply" => FilterValue::Bool(self.ctx.is_reply),
            "flags.restricted" => FilterValue::Bool(self.ctx.is_restricted),
            "flags.monitored" => FilterValue::Bool(self.ctx.is_monitored),
            "flags.reward_message" | "flags.reward" => {
                FilterValue::Bool(self.ctx.is_reward_message)
            }
            "flags.returning_chatter" | "flags.returning" => {
                FilterValue::Bool(self.ctx.is_returning_chatter)
            }
            "flags.shared" | "flags.shared_chat" => FilterValue::Bool(self.ctx.is_shared_chat),
            "flags.sub_message" | "flags.subscription" => {
                FilterValue::Bool(self.ctx.is_sub_message)
            }
            "flags.system_message" | "flags.system" => FilterValue::Bool(self.ctx.is_system),
            "flags.whisper" => FilterValue::Bool(self.ctx.is_whisper),
            "flags.announcement" => FilterValue::Bool(self.ctx.is_announcement),
            "flags.similar" => FilterValue::Bool(self.ctx.is_similar),
            "flags.watch_streak" | "flags.watchstreak" => {
                FilterValue::Bool(self.ctx.is_watch_streak)
            }
            "message.content" | "message.text" => FilterValue::String(self.ctx.content.clone()),
            "message.length" => FilterValue::Int(self.ctx.content.chars().count() as i64),
            "event.action" | "event.type" | "moderation.action" => {
                FilterValue::String(self.ctx.event_action.clone())
            }
            "event.reason" | "moderation.reason" | "reason" => {
                FilterValue::String(self.ctx.event_reason.clone())
            }
            "event.source" | "moderation.source" | "source" => {
                FilterValue::String(self.ctx.event_source.clone())
            }
            "event.status" | "moderation.status" | "status" => {
                FilterValue::String(self.ctx.event_status.clone())
            }
            "event.title" => FilterValue::String(self.ctx.event_title.clone()),
            "event.detail" => FilterValue::String(self.ctx.event_detail.clone()),
            "reward.title" | "reward.name" => FilterValue::String(self.ctx.reward_title.clone()),
            "reward.cost" | "reward.points" => FilterValue::Int(self.ctx.reward_cost as i64),
            "reward.id" | "reward.redemption_id" => FilterValue::String(self.ctx.reward_id.clone()),
            _ => FilterValue::Missing,
        }
    }
}

fn arithmetic(
    lhs: FilterValue,
    rhs: FilterValue,
    op: impl FnOnce(i64, i64) -> Option<i64>,
) -> FilterValue {
    match (lhs.as_int(), rhs.as_int()) {
        (Some(left), Some(right)) => op(left, right).map_or(FilterValue::Missing, FilterValue::Int),
        _ => FilterValue::Missing,
    }
}

fn compare_int(
    lhs: FilterValue,
    rhs: FilterValue,
    op: impl FnOnce(i64, i64) -> bool,
) -> FilterValue {
    match (lhs.as_int(), rhs.as_int()) {
        (Some(left), Some(right)) => FilterValue::Bool(op(left, right)),
        _ => FilterValue::Bool(false),
    }
}

fn values_equal(lhs: &FilterValue, rhs: &FilterValue) -> bool {
    match (lhs, rhs) {
        (FilterValue::Bool(left), FilterValue::Bool(right)) => left == right,
        (FilterValue::Int(left), FilterValue::Int(right)) => left == right,
        (FilterValue::String(left), FilterValue::String(right)) => left == right,
        (FilterValue::Regex(left), FilterValue::Regex(right)) => left.pattern == right.pattern,
        (FilterValue::List(left), FilterValue::List(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| values_equal(left, right))
        }
        (FilterValue::Missing, FilterValue::Missing) => true,
        _ => false,
    }
}

fn values_equal_loose(lhs: &FilterValue, rhs: &FilterValue) -> bool {
    match (lhs, rhs) {
        (FilterValue::String(left), FilterValue::String(right)) => left.eq_ignore_ascii_case(right),
        _ => values_equal(lhs, rhs),
    }
}

fn contains_ignore_ascii_case(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if text.len() < needle.len() {
        return false;
    }
    text.as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn starts_with_ignore_ascii_case(text: &str, needle: &str) -> bool {
    text.as_bytes()
        .get(..needle.len())
        .map(|prefix| prefix.eq_ignore_ascii_case(needle.as_bytes()))
        .unwrap_or(false)
}

fn ends_with_ignore_ascii_case(text: &str, needle: &str) -> bool {
    text.as_bytes()
        .get(text.len().saturating_sub(needle.len())..)
        .map(|suffix| suffix.eq_ignore_ascii_case(needle.as_bytes()))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Int(i64),
    String(String),
    Regex(LexedRegex),
    True,
    False,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Bang,
    Op(BinaryOp),
}

struct Lexer<'a> {
    source: &'a str,
    cursor: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, cursor: 0 }
    }

    fn tokenize(mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();

        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.bump_char();
                continue;
            }

            let token = match ch {
                '(' => {
                    self.bump_char();
                    Token::LParen
                }
                ')' => {
                    self.bump_char();
                    Token::RParen
                }
                '{' => {
                    self.bump_char();
                    Token::LBrace
                }
                '}' => {
                    self.bump_char();
                    Token::RBrace
                }
                ',' => {
                    self.bump_char();
                    Token::Comma
                }
                '!' => {
                    self.bump_char();
                    if self.consume_char('=') {
                        Token::Op(BinaryOp::Ne)
                    } else {
                        Token::Bang
                    }
                }
                '=' => {
                    self.bump_char();
                    if self.consume_char('=') {
                        Token::Op(BinaryOp::Eq)
                    } else {
                        return Err("expected '=' after '='".to_string());
                    }
                }
                '<' => {
                    self.bump_char();
                    if self.consume_char('=') {
                        Token::Op(BinaryOp::Le)
                    } else {
                        Token::Op(BinaryOp::Lt)
                    }
                }
                '>' => {
                    self.bump_char();
                    if self.consume_char('=') {
                        Token::Op(BinaryOp::Ge)
                    } else {
                        Token::Op(BinaryOp::Gt)
                    }
                }
                '&' => {
                    self.bump_char();
                    if self.consume_char('&') {
                        Token::Op(BinaryOp::And)
                    } else {
                        return Err("expected '&' after '&'".to_string());
                    }
                }
                '|' => {
                    self.bump_char();
                    if self.consume_char('|') {
                        Token::Op(BinaryOp::Or)
                    } else {
                        return Err("expected '|' after '|'".to_string());
                    }
                }
                '+' => {
                    self.bump_char();
                    Token::Op(BinaryOp::Add)
                }
                '-' => {
                    self.bump_char();
                    Token::Op(BinaryOp::Sub)
                }
                '*' => {
                    self.bump_char();
                    Token::Op(BinaryOp::Mul)
                }
                '%' => {
                    self.bump_char();
                    Token::Op(BinaryOp::Mod)
                }
                '/' => self.lex_slash_or_regex()?,
                '"' => Token::String(self.lex_quoted_string()?),
                '0'..='9' => self.lex_int()?,
                _ if is_ident_start(ch) => self.lex_ident_or_keyword()?,
                _ => return Err(format!("unexpected character '{ch}'")),
            };

            tokens.push(token);
        }

        Ok(tokens)
    }

    fn lex_int(&mut self) -> Result<Token, String> {
        let start = self.cursor;
        while self
            .peek_char()
            .map(|ch| ch.is_ascii_digit())
            .unwrap_or(false)
        {
            self.bump_char();
        }
        self.source[start..self.cursor]
            .parse::<i64>()
            .map(Token::Int)
            .map_err(|error| format!("invalid integer: {error}"))
    }

    fn lex_ident_or_keyword(&mut self) -> Result<Token, String> {
        let start = self.cursor;
        while self.peek_char().map(is_ident_continue).unwrap_or(false) {
            self.bump_char();
        }
        let ident = &self.source[start..self.cursor];

        if (ident == "r" || ident == "ri") && self.peek_char() == Some('"') {
            let pattern = self.lex_regex_quoted_string()?;
            let regex = compile_filter_regex(&pattern, ident == "ri")?;
            return Ok(Token::Regex(LexedRegex(regex)));
        }

        Ok(match ident {
            "true" | "True" => Token::True,
            "false" | "False" => Token::False,
            "contains" => Token::Op(BinaryOp::Contains),
            "startswith" => Token::Op(BinaryOp::StartsWith),
            "endswith" => Token::Op(BinaryOp::EndsWith),
            "match" | "matches" => Token::Op(BinaryOp::Match),
            _ => Token::Ident(ident.to_string()),
        })
    }

    fn lex_quoted_string(&mut self) -> Result<String, String> {
        if self.bump_char() != Some('"') {
            return Err("expected opening quote".to_string());
        }

        let mut value = String::new();
        while let Some(ch) = self.bump_char() {
            match ch {
                '"' => return Ok(value),
                '\\' => {
                    let Some(escaped) = self.bump_char() else {
                        return Err("unterminated escape sequence".to_string());
                    };
                    let decoded = match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        other => other,
                    };
                    value.push(decoded);
                }
                other => value.push(other),
            }
        }

        Err("unterminated string literal".to_string())
    }

    fn lex_regex_quoted_string(&mut self) -> Result<String, String> {
        if self.bump_char() != Some('"') {
            return Err("expected opening quote".to_string());
        }

        let mut value = String::new();
        while let Some(ch) = self.bump_char() {
            match ch {
                '"' => return Ok(value),
                '\\' => {
                    let Some(escaped) = self.bump_char() else {
                        return Err("unterminated escape sequence".to_string());
                    };
                    value.push('\\');
                    value.push(escaped);
                }
                other => value.push(other),
            }
        }

        Err("unterminated regex literal".to_string())
    }

    fn lex_slash_or_regex(&mut self) -> Result<Token, String> {
        let start = self.cursor;
        self.bump_char();

        let mut escaped = false;
        let mut end = None;
        for (offset, ch) in self.source[self.cursor..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '/' {
                end = Some(self.cursor + offset);
                break;
            }
        }

        let Some(end) = end else {
            self.cursor = start + 1;
            return Ok(Token::Op(BinaryOp::Div));
        };

        let pattern = self.source[self.cursor..end].to_string();
        self.cursor = end + 1;

        let flags_start = self.cursor;
        while self
            .peek_char()
            .map(|ch| ch.is_ascii_alphabetic())
            .unwrap_or(false)
        {
            self.bump_char();
        }
        let flags = &self.source[flags_start..self.cursor];
        let regex = compile_filter_regex(&pattern, flags.contains('i'))?;
        Ok(Token::Regex(LexedRegex(regex)))
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.cursor..].chars().next()
    }

    fn bump_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.cursor += ch.len_utf8();
        Some(ch)
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.')
}

/// A regex literal compiled once by the lexer and handed to the parser as-is.
#[derive(Debug, Clone)]
struct LexedRegex(Regex);

impl PartialEq for LexedRegex {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for LexedRegex {}

fn compile_filter_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, String> {
    let pattern = if case_insensitive {
        format!("(?i:{pattern})")
    } else {
        pattern.to_string()
    };

    Regex::new(&pattern).map_err(|error| format!("invalid regex: {error}"))
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    nesting: usize,
}

impl Parser {
    fn new(source: &str) -> Result<Self, String> {
        Ok(Self {
            tokens: Lexer::new(source).tokenize()?,
            cursor: 0,
            nesting: 0,
        })
    }

    fn parse(&mut self) -> Result<Expr, String> {
        let expr = self.parse_or()?;
        if self.peek().is_some() {
            return Err("unexpected trailing input".to_string());
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        self.parse_chain(BinaryOp::Or, Self::parse_and)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        self.parse_chain(BinaryOp::And, Self::parse_not)
    }

    fn parse_chain(
        &mut self,
        op: BinaryOp,
        operand: fn(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        let first = operand(self)?;
        if !self.consume_op(op) {
            return Ok(first);
        }
        let mut terms = vec![first];
        loop {
            if terms.len() >= MAX_FILTER_CHAIN_TERMS {
                return Err("expression is too long".to_string());
            }
            terms.push(operand(self)?);
            if !self.consume_op(op) {
                break;
            }
        }
        let chain = Expr::Chain { op, terms };
        if expr_depth(&chain) > MAX_FILTER_DEPTH {
            return Err("expression is too long".to_string());
        }
        Ok(chain)
    }

    fn parse_not(&mut self) -> Result<Expr, String> {
        if matches!(self.peek(), Some(Token::Bang)) {
            self.cursor += 1;
            return Ok(Expr::Not(Box::new(self.nested(Self::parse_not)?)));
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_math()?;
        let mut depth = expr_depth(&expr);
        while let Some(op) = self.peek_comparison_op() {
            self.cursor += 1;
            let rhs = self.parse_math()?;
            expr = join_binary(expr, &mut depth, op, rhs)?;
        }
        Ok(expr)
    }

    fn parse_math(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;
        let mut depth = expr_depth(&expr);
        while let Some(op) = self.peek_math_op() {
            self.cursor += 1;
            let rhs = self.parse_primary()?;
            expr = join_binary(expr, &mut depth, op, rhs)?;
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        let token = self
            .advance()
            .ok_or_else(|| "expected expression".to_string())?;

        match token {
            Token::Ident(value) => Ok(Expr::Variable(value)),
            Token::Int(value) => Ok(Expr::Literal(FilterValue::Int(value))),
            Token::String(value) => Ok(Expr::Literal(FilterValue::String(value))),
            Token::Regex(LexedRegex(regex)) => {
                Ok(Expr::Literal(FilterValue::Regex(RegexLiteral {
                    pattern: regex.as_str().to_string(),
                    regex,
                })))
            }
            Token::True => Ok(Expr::Literal(FilterValue::Bool(true))),
            Token::False => Ok(Expr::Literal(FilterValue::Bool(false))),
            Token::LParen => {
                let expr = self.nested(Self::parse_or)?;
                if !matches!(self.advance(), Some(Token::RParen)) {
                    return Err("expected ')'".to_string());
                }
                Ok(expr)
            }
            Token::LBrace => self.nested(Self::parse_list),
            other => Err(format!("unexpected token {other:?}")),
        }
    }

    fn parse_list(&mut self) -> Result<Expr, String> {
        let mut values = Vec::new();
        if matches!(self.peek(), Some(Token::RBrace)) {
            self.cursor += 1;
            return Ok(Expr::List(values));
        }

        loop {
            values.push(self.parse_or()?);
            match self.advance() {
                Some(Token::Comma) => {}
                Some(Token::RBrace) => break,
                _ => return Err("expected ',' or '}' in list".to_string()),
            }
        }

        Ok(Expr::List(values))
    }

    fn peek_comparison_op(&self) -> Option<BinaryOp> {
        match self.peek() {
            Some(Token::Op(
                op @ (BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::Contains
                | BinaryOp::StartsWith
                | BinaryOp::EndsWith
                | BinaryOp::Match),
            )) => Some(*op),
            _ => None,
        }
    }

    fn peek_math_op(&self) -> Option<BinaryOp> {
        match self.peek() {
            Some(Token::Op(
                op
                @ (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod),
            )) => Some(*op),
            _ => None,
        }
    }

    fn consume_op(&mut self, expected: BinaryOp) -> bool {
        match self.peek() {
            Some(Token::Op(op)) if *op == expected => {
                self.cursor += 1;
                true
            }
            _ => false,
        }
    }

    fn nested(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        if self.nesting >= MAX_FILTER_NESTING {
            return Err("expression is nested too deeply".to_string());
        }
        self.nesting += 1;
        let result = parse(self);
        self.nesting -= 1;
        result
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.cursor).cloned()?;
        self.cursor += 1;
        Some(token)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }
}

fn expr_depth(expr: &Expr) -> usize {
    match expr {
        Expr::Literal(_) | Expr::Variable(_) => 1,
        Expr::List(values) => 1 + values.iter().map(expr_depth).max().unwrap_or(0),
        Expr::Not(value) => 1 + expr_depth(value),
        Expr::Chain { terms, .. } => 1 + terms.iter().map(expr_depth).max().unwrap_or(0),
        Expr::Binary { lhs, rhs, .. } => 1 + expr_depth(lhs).max(expr_depth(rhs)),
    }
}

fn join_binary(lhs: Expr, depth: &mut usize, op: BinaryOp, rhs: Expr) -> Result<Expr, String> {
    *depth = (*depth).max(expr_depth(&rhs)) + 1;
    if *depth > MAX_FILTER_DEPTH {
        return Err("expression is too long".to_string());
    }
    Ok(Expr::Binary {
        lhs: Box::new(lhs),
        op,
        rhs: Box::new(rhs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate(expr: &str, context: FilterContext) -> bool {
        FilterExpression::parse(expr)
            .and_then(|expression| expression.evaluate(&context))
            .map(|evaluation| evaluation.matched)
            .unwrap_or(false)
    }

    fn base_context() -> FilterContext {
        FilterContext {
            content: "hello 12/31/2020".to_string(),
            author: "SomeViewer".to_string(),
            author_login: "someviewer".to_string(),
            author_user_id: "1234".to_string(),
            author_color: Some("#FF0000".to_string()),
            badges: vec!["moderator".to_string(), "subscriber".to_string()],
            sub_length: Some(12),
            channel_name: "somestreamer".to_string(),
            bits_amount: 100,
            is_sub: true,
            is_cheer: true,
            ..Default::default()
        }
    }

    #[test]
    fn evaluates_compound_boolean_expressions() {
        let context = base_context();
        assert!(evaluate(
            r#"(author.badges contains "moderator") && (message.content contains "hello")"#,
            context.clone()
        ));
        assert!(evaluate(
            r#"message.length < 40 || author.subbed"#,
            context.clone()
        ));
        assert!(evaluate(
            r#"channel.name == "somestreamer" && author.badges contains "moderator""#,
            context
        ));
    }

    #[test]
    fn supports_negated_string_operators() {
        let mut context = base_context();
        context.content = "!title new stream title".to_string();
        assert!(!evaluate(
            r#"!(message.content startswith "!title")"#,
            context.clone()
        ));
        assert!(evaluate(r#"!message.content contains "!points""#, context));
    }

    #[test]
    fn supports_regex_literals_and_capture_lists() {
        let context = base_context();
        assert!(evaluate(
            r#"message.content match r"\d\d""#,
            context.clone()
        ));
        assert!(evaluate(
            r#"(message.content match {r"(\d\d)/(\d\d)/(\d\d\d\d)", 3}) == "2020""#,
            context
        ));
    }

    #[test]
    fn evaluates_math_left_to_right_without_precedence() {
        let context = FilterContext::default();
        assert!(evaluate("2 + 3 * 4 == 20", context.clone()));
        assert!(!evaluate("2 + 3 * 4 == 14", context));
    }

    #[test]
    fn rejects_trailing_input() {
        assert_eq!(
            FilterExpression::parse("true false").unwrap_err(),
            "unexpected trailing input"
        );
    }

    #[test]
    fn supports_case_insensitive_regex() {
        assert!(evaluate(
            r#"message.content match ri"HELLO""#,
            base_context()
        ));
    }

    #[test]
    fn division_by_zero_evaluates_false() {
        assert!(!evaluate("10 / 0 == 0", FilterContext::default()));
    }

    #[test]
    fn overflowing_division_evaluates_false() {
        let min = "(0 - 9223372036854775807 - 1)";
        assert!(!evaluate(
            &format!("{min} / (0 - 1) == 0"),
            FilterContext::default()
        ));
        assert!(!evaluate(
            &format!("{min} % (0 - 1) == 0"),
            FilterContext::default()
        ));
    }

    #[test]
    fn rejects_pathologically_deep_expressions() {
        let parens = format!("{}true{}", "(".repeat(10_000), ")".repeat(10_000));
        assert!(FilterExpression::parse(&parens).is_err());
        assert!(FilterExpression::parse(&format!("{}true", "!".repeat(10_000))).is_err());
        assert!(
            FilterExpression::parse(&format!("{}1{}", "{".repeat(10_000), "}".repeat(10_000)))
                .is_err()
        );
        assert!(FilterExpression::parse(&vec!["1"; 10_000].join(" + ")).is_err());
        assert!(FilterExpression::parse(&vec!["true"; 100].join(" || ")).is_ok());
        assert!(FilterExpression::parse(&vec!["true"; 10_000].join(" && ")).is_err());
    }

    #[test]
    fn long_boolean_chains_parse_and_short_circuit() {
        let mut terms = vec!["false"; 1_000];
        terms.push("true");
        assert!(evaluate(&terms.join(" || "), FilterContext::default()));
        let mut terms = vec!["true"; 1_000];
        assert!(evaluate(&terms.join(" && "), FilterContext::default()));
        terms.push("false");
        assert!(!evaluate(&terms.join(" && "), FilterContext::default()));
        assert!(evaluate("false || true && true", FilterContext::default()));
        assert!(!evaluate(
            "(true || false) && false",
            FilterContext::default()
        ));
    }

    #[test]
    fn message_length_counts_unicode_scalars() {
        let context = FilterContext {
            content: "😀é".to_string(),
            ..Default::default()
        };
        assert!(evaluate("message.length == 2", context));
    }
}
