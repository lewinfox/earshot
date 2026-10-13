//! The search language, used by the library's search box, the Flow's search popup
//! and smart playlists. One parser, one AST ([`Expr`]), two backends: [`eval`]
//! filters tracks in memory for the lists, [`to_sql`] runs the same query in the
//! database (smart playlists, export).
//!
//! ```text
//! deep "warm up"            words and phrases, anywhere in the text fields
//! artist:daft  title="One"  a field contains / equals
//! bpm:124  bpm:120-126      bpm:124 means 123.5-124.5
//! bpm>=128  time<5:00       comparisons: > >= < <= =
//! key:8A  key:Am            a key, in Camelot or by name
//! energy:7-10  rating>=4    energy is 1-10, ranked across the library
//! tag:vocal  in:12          a tag; in collection 12
//! plays>=3  played<7        times played; days since it was last played
//! is:analysed  is:dynamic  is:drifting  is:missing
//! status:busy               analysis: none, done, busy (queued or running), attention
//! -tag:vocal  NOT x         leave out
//! a OR b  a | b  (a b) c    either; grouping. Terms side by side all have to match.
//! sort:bpm  sort:-energy    order (`-`: highest first); the first one wins ties last
//! ```

use std::cmp::Ordering;
use std::fmt;

use rusqlite::types::Value as SqlValue;

use super::keys::{Camelot, key_name, parse_key};
use super::track::{Status, TextField, Track, TrackId};

/// What a query can look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Field {
    Title,
    Artist,
    Album,
    Genre,
    Comment,
    Path,
    Tag,
    Bpm,
    Key,
    /// 1-10, ranked across the library.
    Energy,
    Rating,
    Colour,
    /// Seconds.
    Duration,
    /// Times played, in any session.
    Plays,
    /// Days since it was last played (none if it never has been).
    LastPlayed,
    Analysed,
    Dynamic,
    Drifts,
    /// The analysis in one word ([`Status`]): it depends on the analysis queue too,
    /// which [`Context::status`] knows.
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Num,
    Key,
    Bool,
    Status,
}

impl Field {
    fn kind(self) -> Kind {
        use Field::*;
        match self {
            Title | Artist | Album | Genre | Comment | Path | Tag => Kind::Text,
            Bpm | Energy | Rating | Colour | Duration | Plays | LastPlayed => Kind::Num,
            Key => Kind::Key,
            Analysed | Dynamic | Drifts => Kind::Bool,
            Status => Kind::Status,
        }
    }

    /// From the name typed before `:` (or after `sort:`).
    pub fn parse(name: &str) -> Option<Field> {
        use Field::*;
        Some(match name.to_lowercase().as_str() {
            "title" => Title,
            "artist" => Artist,
            "album" => Album,
            "genre" => Genre,
            "comment" => Comment,
            "path" | "file" => Path,
            "tag" => Tag,
            "bpm" | "tempo" => Bpm,
            "key" => Key,
            "energy" => Energy,
            "rating" | "stars" => Rating,
            "colour" | "color" => Colour,
            "time" | "length" | "duration" => Duration,
            "plays" => Plays,
            "played" | "lastplayed" => LastPlayed,
            "status" | "analysis" => Status,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        use Field::*;
        match self {
            Title => "title",
            Artist => "artist",
            Album => "album",
            Genre => "genre",
            Comment => "comment",
            Path => "path",
            Tag => "tag",
            Bpm => "bpm",
            Key => "key",
            Energy => "energy",
            Rating => "rating",
            Colour => "colour",
            Duration => "time",
            Plays => "plays",
            LastPlayed => "played",
            Analysed => "analysed",
            Dynamic => "dynamic",
            Drifts => "drifting",
            Status => "status",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `:` on a text field.
    Contains,
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Lowercase.
    Text(String),
    Num(f64),
    Key(Camelot),
    Bool(bool),
    Status(Status),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// All must match (none: matches everything).
    And(Vec<Expr>),
    /// Any must match (none: matches nothing).
    Or(Vec<Expr>),
    Not(Box<Expr>),
    /// Lowercase; found in any text field.
    Text(String),
    Cmp(Field, Op, Value),
    /// Inclusive.
    Range(Field, f64, f64),
    /// In a crate, playlist or Flow.
    InCollection(i64),
    /// The track's file can't be found.
    Missing,
}

impl Expr {
    pub const ALL: Expr = Expr::And(vec![]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Dir {
    Asc,
    Desc,
}

/// Sort keys, most important first. Empty: the list's own order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sort(pub Vec<(Field, Dir)>);

/// A parsed query: what to show and in what order.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub expr: Expr,
    pub sort: Sort,
}

impl Default for Query {
    fn default() -> Self {
        Query {
            expr: Expr::ALL,
            sort: Sort::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(s: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(s.into()))
}

// ---- parsing ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Open,
    Close,
    Or,
    Not,
    /// `field op value`, or just a value (`field` empty).
    Atom {
        field: String,
        op: &'static str,
        value: String,
        quoted: bool,
    },
}

const OPS: [&str; 6] = [">=", "<=", ">", "<", "=", ":"];

fn lex(s: &str) -> Result<Vec<Tok>, ParseError> {
    let mut out = vec![];
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        match c {
            '(' => {
                chars.next();
                out.push(Tok::Open);
                continue;
            }
            ')' => {
                chars.next();
                out.push(Tok::Close);
                continue;
            }
            '|' => {
                chars.next();
                out.push(Tok::Or);
                continue;
            }
            '-' => {
                chars.next();
                // A lone "-" is a word; "-x" leaves x out.
                match chars.peek() {
                    Some(n) if !n.is_whitespace() && *n != ')' => out.push(Tok::Not),
                    _ => out.push(atom("", "", "-".into(), false)),
                }
                continue;
            }
            _ => {}
        }
        // An atom: up to whitespace or a bracket, with quoted parts kept whole.
        let mut buf = String::new();
        let mut field = String::new();
        let mut op = "";
        let mut quoted = false;
        let mut in_quote = false;
        while let Some(&c) = chars.peek() {
            if !in_quote && (c.is_whitespace() || c == '(' || c == ')') {
                break;
            }
            chars.next();
            if c == '"' {
                in_quote = !in_quote;
                quoted = true;
                continue;
            }
            // The first operator after a plain word splits off a field.
            if !in_quote
                && !quoted
                && op.is_empty()
                && !buf.is_empty()
                && buf.chars().all(|c| c.is_ascii_alphabetic())
                && let Some(o) = OPS.iter().find(|o| o.starts_with(c))
            {
                let two = chars
                    .peek()
                    .is_some_and(|&n| o.len() == 2 && o.ends_with(n));
                let o = if o.len() == 2 && !two {
                    OPS.iter().find(|x| **x == c.to_string()).unwrap()
                } else {
                    o
                };
                if o.len() == 2 {
                    chars.next();
                }
                op = o;
                field = std::mem::take(&mut buf);
                continue;
            }
            buf.push(c);
        }
        if in_quote {
            return err("a quote isn't closed");
        }
        out.push(match (field.as_str(), op, buf.as_str(), quoted) {
            ("", _, "OR", false) => Tok::Or,
            ("", _, "NOT", false) => Tok::Not,
            ("", _, "AND", false) => continue,
            _ => atom(&field, op, buf, quoted),
        });
    }
    Ok(out)
}

fn atom(field: &str, op: &'static str, value: String, quoted: bool) -> Tok {
    Tok::Atom {
        field: field.to_string(),
        op,
        value,
        quoted,
    }
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
    sort: Vec<(Field, Dir)>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn or(&mut self) -> Result<Expr, ParseError> {
        let mut any = vec![self.and()?];
        while self.peek() == Some(&Tok::Or) {
            self.at += 1;
            any.push(self.and()?);
        }
        Ok(if any.len() == 1 {
            any.pop().unwrap()
        } else {
            Expr::Or(any)
        })
    }

    fn and(&mut self) -> Result<Expr, ParseError> {
        let mut all = vec![];
        while let Some(t) = self.peek() {
            if matches!(t, Tok::Or | Tok::Close) {
                break;
            }
            match self.unary()? {
                Expr::And(v) if v.is_empty() => {} // a sort: term
                e => all.push(e),
            }
        }
        if all.is_empty() && matches!(self.peek(), Some(Tok::Or)) {
            return err("nothing before OR");
        }
        Ok(if all.len() == 1 {
            all.pop().unwrap()
        } else {
            Expr::And(all)
        })
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        let Some(t) = self.peek().cloned() else {
            return err("the query ends too soon");
        };
        self.at += 1;
        match t {
            Tok::Not => {
                if matches!(self.peek(), None | Some(Tok::Or | Tok::Close)) {
                    return err("nothing after NOT");
                }
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            Tok::Open => {
                let e = self.or()?;
                if self.peek() != Some(&Tok::Close) {
                    return err("a bracket isn't closed");
                }
                self.at += 1;
                Ok(e)
            }
            Tok::Close | Tok::Or => err("unexpected bracket or OR"),
            Tok::Atom {
                field,
                op,
                value,
                quoted,
            } => self.term(&field, op, &value, quoted),
        }
    }

    fn term(
        &mut self,
        field: &str,
        op: &str,
        value: &str,
        quoted: bool,
    ) -> Result<Expr, ParseError> {
        if field.is_empty() {
            return Ok(Expr::Text(value.to_lowercase()));
        }
        let lower = field.to_lowercase();
        if value.is_empty() {
            return err(format!("nothing after \"{field}{op}\""));
        }
        match lower.as_str() {
            "sort" if op == ":" => {
                let (name, dir) = match value.strip_prefix('-') {
                    Some(n) => (n, Dir::Desc),
                    None => (value, Dir::Asc),
                };
                let Some(f) = Field::parse(name) else {
                    return err(format!("can't sort by \"{name}\""));
                };
                self.sort.push((f, dir));
                return Ok(Expr::ALL);
            }
            "is" if op == ":" => {
                return Ok(match value.to_lowercase().as_str() {
                    "analysed" | "analyzed" => {
                        Expr::Cmp(Field::Analysed, Op::Eq, Value::Bool(true))
                    }
                    "dynamic" => Expr::Cmp(Field::Dynamic, Op::Eq, Value::Bool(true)),
                    "drifting" | "drifts" => Expr::Cmp(Field::Drifts, Op::Eq, Value::Bool(true)),
                    "missing" => Expr::Missing,
                    v => {
                        return err(format!(
                            "is:{v}? Try is:analysed, is:dynamic, is:drifting or is:missing"
                        ));
                    }
                });
            }
            "in" if op == ":" => {
                return match value.parse() {
                    Ok(id) => Ok(Expr::InCollection(id)),
                    Err(_) => err(format!("in: needs a collection number, not \"{value}\"")),
                };
            }
            _ => {}
        }
        let Some(f) = Field::parse(&lower) else {
            return err(format!(
                "no field called \"{field}\" (put it in quotes to search for the text)"
            ));
        };
        let op = match op {
            ":" => Op::Contains,
            "=" => Op::Eq,
            "<" => Op::Lt,
            "<=" => Op::Le,
            ">" => Op::Gt,
            ">=" => Op::Ge,
            _ => unreachable!(),
        };
        match f.kind() {
            Kind::Text => match op {
                Op::Contains | Op::Eq => Ok(Expr::Cmp(f, op, Value::Text(value.to_lowercase()))),
                _ => err(format!("{} is text: use {}:word", f.name(), f.name())),
            },
            Kind::Key => {
                if !matches!(op, Op::Contains | Op::Eq) {
                    return err("compare keys with key:8A");
                }
                match parse_key(value) {
                    Some(k) => Ok(Expr::Cmp(f, Op::Eq, Value::Key(k))),
                    None => err(format!("\"{value}\" isn't a key (try 8A or Am)")),
                }
            }
            Kind::Num => {
                let num = |s: &str| -> Result<f64, ParseError> {
                    let n = if f == Field::Duration {
                        parse_time(s)
                    } else {
                        s.parse().ok()
                    };
                    n.filter(|n: &f64| n.is_finite())
                        .ok_or_else(|| ParseError(format!("\"{s}\" isn't a number")))
                };
                if !quoted
                    && matches!(op, Op::Contains | Op::Eq)
                    && let Some((a, b)) = value.split_once('-')
                    && !a.is_empty()
                {
                    let (a, b) = (num(a)?, num(b)?);
                    return Ok(Expr::Range(f, a.min(b), a.max(b)));
                }
                let n = num(value)?;
                Ok(match op {
                    // "bpm:124" means about 124.
                    Op::Contains if f == Field::Bpm => Expr::Range(f, n - 0.5, n + 0.5),
                    // "played:3": three days ago.
                    Op::Contains if f == Field::LastPlayed => Expr::Range(f, n, n + 1.0),
                    Op::Contains => Expr::Cmp(f, Op::Eq, Value::Num(n)),
                    op => Expr::Cmp(f, op, Value::Num(n)),
                })
            }
            Kind::Status => {
                if !matches!(op, Op::Contains | Op::Eq) {
                    return err("use status:done (or none, busy, attention)");
                }
                match Status::parse(value) {
                    Some(s) => Ok(Expr::Cmp(f, Op::Eq, Value::Status(s))),
                    None => err(format!(
                        "status:{value}? Try status:none, done, busy or attention"
                    )),
                }
            }
            Kind::Bool => unreachable!("is: handles these"),
        }
    }
}

/// "5:30" or "330" (seconds).
fn parse_time(s: &str) -> Option<f64> {
    match s.split_once(':') {
        Some((m, sec)) => Some(m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?),
        None => s.parse().ok(),
    }
}

/// Parse a search. An empty one matches everything.
pub fn parse(s: &str) -> Result<Query, ParseError> {
    let mut p = Parser {
        toks: lex(s)?,
        at: 0,
        sort: vec![],
    };
    let expr = if p.toks.is_empty() {
        Expr::ALL
    } else {
        p.or()?
    };
    if p.at < p.toks.len() {
        return err("a closing bracket has no opening one");
    }
    Ok(Query {
        expr,
        sort: Sort(p.sort),
    })
}

// ---- in memory ----------------------------------------------------------------------

/// What [`eval`] needs beyond the track itself.
pub trait Context {
    /// Is the track in this crate, playlist or Flow?
    fn contains(&self, collection: i64, track: TrackId) -> bool;

    /// The track's analysis status. Without the analysis queue (the default), a
    /// track is never busy.
    fn status(&self, t: &Track) -> Status {
        Status::of(t, None)
    }
}

/// No collections.
impl Context for () {
    fn contains(&self, _: i64, _: TrackId) -> bool {
        false
    }
}

fn text_field(t: &Track, f: Field) -> &str {
    match f {
        Field::Title => t.text(TextField::Title),
        Field::Artist => t.text(TextField::Artist),
        Field::Album => t.text(TextField::Album),
        Field::Genre => t.text(TextField::Genre),
        Field::Comment => t.text(TextField::Comment),
        Field::Path => t.text(TextField::FileName),
        _ => "",
    }
}

fn num_field(t: &Track, f: Field) -> Option<f64> {
    match f {
        Field::Bpm => t.bpm,
        Field::Energy => t.energy_level(),
        Field::Rating => t.rating.map(f64::from),
        Field::Colour => t.colour.map(f64::from),
        Field::Duration => t.duration,
        Field::Plays => Some(t.plays as f64),
        Field::LastPlayed => t.last_played.map(days_since),
        _ => None,
    }
}

/// Days from a unix time to now.
fn days_since(unix: i64) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    (now - unix as f64) / 86_400.0
}

fn bool_field(t: &Track, f: Field) -> bool {
    match f {
        Field::Analysed => t.analysed(),
        Field::Dynamic => t.dynamic(),
        Field::Drifts => t.drifts,
        _ => false,
    }
}

/// Does the track match? The index holds only tracks whose file is present, so
/// [`Expr::Missing`] is always false here.
pub fn eval(e: &Expr, t: &Track, cx: &dyn Context) -> bool {
    match e {
        Expr::And(v) => v.iter().all(|e| eval(e, t, cx)),
        Expr::Or(v) => v.iter().any(|e| eval(e, t, cx)),
        Expr::Not(e) => !eval(e, t, cx),
        Expr::Text(s) => t.search_text.contains(s.as_str()),
        Expr::InCollection(c) => cx.contains(*c, t.id),
        Expr::Missing => false,
        Expr::Range(f, lo, hi) => num_field(t, *f).is_some_and(|n| n >= *lo && n <= *hi),
        Expr::Cmp(Field::Tag, op, Value::Text(s)) => t.tags.iter().any(|tag| {
            let tag = tag.to_lowercase();
            match op {
                Op::Eq => tag == *s,
                _ => tag.contains(s.as_str()),
            }
        }),
        Expr::Cmp(f, op, v) => match v {
            Value::Text(s) => {
                let have = text_field(t, *f);
                match op {
                    Op::Eq => have == s,
                    _ => have.contains(s.as_str()),
                }
            }
            Value::Num(n) => num_field(t, *f).is_some_and(|have| cmp(have, *op, *n)),
            Value::Key(k) => t.key == Some(*k),
            Value::Bool(b) => bool_field(t, *f) == *b,
            Value::Status(s) => cx.status(t) == *s,
        },
    }
}

fn cmp(a: f64, op: Op, b: f64) -> bool {
    match op {
        Op::Contains | Op::Eq => a == b,
        Op::Lt => a < b,
        Op::Le => a <= b,
        Op::Gt => a > b,
        Op::Ge => a >= b,
    }
}

/// Order two tracks by the sort keys. Tracks without a value go last, whichever
/// way the sort runs.
pub fn compare(sort: &Sort, a: &Track, b: &Track, cx: &dyn Context) -> Ordering {
    for &(f, dir) in &sort.0 {
        let o = match f.kind() {
            Kind::Text if f == Field::Tag => a.text(TextField::Tags).cmp(b.text(TextField::Tags)),
            Kind::Text => text_field(a, f).cmp(text_field(b, f)),
            Kind::Num => match (num_field(a, f), num_field(b, f)) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (x, y) => {
                    let o = y.is_some().cmp(&x.is_some());
                    if o != Ordering::Equal {
                        return o;
                    }
                    Ordering::Equal
                }
            },
            Kind::Key => match (a.key, b.key) {
                (Some(x), Some(y)) => (x.n, x.minor).cmp(&(y.n, y.minor)),
                (x, y) => {
                    let o = y.is_some().cmp(&x.is_some());
                    if o != Ordering::Equal {
                        return o;
                    }
                    Ordering::Equal
                }
            },
            Kind::Bool => bool_field(a, f).cmp(&bool_field(b, f)),
            Kind::Status => cx.status(a).cmp(&cx.status(b)),
        };
        let o = if dir == Dir::Desc { o.reverse() } else { o };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

// ---- SQL ---------------------------------------------------------------------------

/// The query as SQL over the baseline schema: `SELECT t.id FROM tracks t …`, in
/// order, with its parameters. Only present, non-deleted tracks are returned unless
/// the query asks for `is:missing`. Differences from [`eval`]: SQLite's `lower()`
/// only folds ASCII, a title is the tag's (no file-name fallback), and energy is
/// ranked among every measured track, not only those under the library folders.
pub fn to_sql(q: &Query) -> (String, Vec<SqlValue>) {
    let mut args = vec![];
    let cond = sql(&q.expr, &mut args);
    let present =
        "EXISTS (SELECT 1 FROM files f WHERE f.track_id = t.id AND f.missing_since IS NULL)";
    let present = if mentions_missing(&q.expr) {
        "1"
    } else {
        present
    };
    let mut order: Vec<String> = q
        .sort
        .0
        .iter()
        .map(|&(f, dir)| {
            let col = sql_field(f);
            // NULLs last both ways, as in memory.
            format!(
                "({col}) IS NULL, {} {}",
                col,
                if dir == Dir::Desc { "DESC" } else { "ASC" }
            )
        })
        .collect();
    order.push("t.id".into());
    (
        format!(
            "SELECT t.id FROM tracks t WHERE t.deleted_at IS NULL AND {present} AND ({cond}) ORDER BY {}",
            order.join(", ")
        ),
        args,
    )
}

fn mentions_missing(e: &Expr) -> bool {
    match e {
        Expr::Missing => true,
        Expr::And(v) | Expr::Or(v) => v.iter().any(mentions_missing),
        Expr::Not(e) => mentions_missing(e),
        _ => false,
    }
}

fn meta(field: &str) -> String {
    format!(
        "(SELECT value FROM track_meta_resolved m WHERE m.track_id = t.id AND m.field = '{field}')"
    )
}

/// The file name of the track's present file.
const FILE_NAME: &str = "(SELECT replace(p, rtrim(p, replace(p, '/', '')), '') FROM
    (SELECT CAST(path AS TEXT) AS p FROM files f WHERE f.track_id = t.id AND f.missing_since IS NULL
     ORDER BY f.id LIMIT 1))";

/// An SQL expression for a field's value (NULL when there isn't one).
fn sql_field(f: Field) -> String {
    match f {
        Field::Title => meta("title"),
        Field::Artist => meta("artist"),
        Field::Album => meta("album"),
        Field::Genre => meta("genre"),
        Field::Comment => meta("comment"),
        Field::Path => FILE_NAME.into(),
        Field::Tag => {
            "(SELECT group_concat(g.name, ' ') FROM track_tags tt JOIN tags g ON g.id = tt.tag_id
                        WHERE tt.track_id = t.id)"
                .into()
        }
        Field::Bpm => format!(
            "coalesce(t.bpm, CAST(nullif(trim({}), '') AS REAL))",
            meta("bpm")
        ),
        Field::Key => format!("lower(trim({}))", meta("key")),
        Field::Energy => {
            "(SELECT r FROM (SELECT track_id, percent_rank() OVER (ORDER BY value) * 9 + 1 AS r
                           FROM track_features WHERE name = 'energy') e WHERE e.track_id = t.id)"
                .into()
        }
        Field::Rating => "t.rating".into(),
        Field::Colour => "t.color".into(),
        Field::Duration => "t.duration_s".into(),
        Field::Plays => "(SELECT count(*) FROM plays p WHERE p.track_id = t.id)".into(),
        Field::LastPlayed => "((unixepoch('subsec') - (SELECT max(started_at) FROM plays p
                                WHERE p.track_id = t.id)) / 86400.0)"
            .into(),
        Field::Analysed => "(t.active_grid_id IS NOT NULL)".into(),
        Field::Dynamic => "EXISTS (SELECT 1 FROM beatgrids g WHERE g.id = t.active_grid_id
                           AND g.source = 'analysis-dynamic')"
            .into(),
        Field::Drifts => "EXISTS (SELECT 1 FROM track_features x WHERE x.track_id = t.id
                          AND x.name = 'drifts' AND x.value <> 0)"
            .into(),
        // As `Status`'s order: none 0, done 1, busy 2, attention 3. A failed analysis
        // is the stored error (in memory it lasts until the app restarts), and busy
        // is a queued job.
        Field::Status => format!(
            "CASE WHEN EXISTS (SELECT 1 FROM analysis_results r WHERE r.track_id = t.id
                               AND r.error IS NOT NULL) THEN {attention}
                  WHEN EXISTS (SELECT 1 FROM jobs j WHERE j.track_id = t.id
                               AND j.kind LIKE 'analyse:%') THEN {busy}
                  WHEN {drifts} AND NOT {dynamic} THEN {attention}
                  WHEN {analysed} THEN {done} ELSE {none} END",
            drifts = sql_field(Field::Drifts),
            dynamic = sql_field(Field::Dynamic),
            analysed = sql_field(Field::Analysed),
            none = Status::None as i32,
            done = Status::Done as i32,
            busy = Status::Busy as i32,
            attention = Status::Attention as i32,
        ),
    }
}

/// `%s%`, with LIKE's wildcards escaped (by `\`).
fn like(s: &str) -> SqlValue {
    let esc = s
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    SqlValue::Text(format!("%{esc}%"))
}

/// Every spelling of `k` that [`parse_key`] reads, lowercase: "8a", "am", "a min" …
/// The tag holds the key as text, so SQL matches it against these.
fn key_spellings(k: Camelot) -> Vec<String> {
    let mut out = vec![
        key_name(k).to_lowercase(),
        format!("0{}", key_name(k).to_lowercase()),
    ];
    for note in ["c", "d", "e", "f", "g", "a", "b"] {
        for acc in ["", "#", "b", "♯", "♭"] {
            for suffix in [
                "", "m", "min", "minor", "maj", "major", " m", " min", " minor", " maj", " major",
            ] {
                let s = format!("{note}{acc}{suffix}");
                if parse_key(&s) == Some(k) {
                    out.push(s);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A condition that is always 0 or 1 (never NULL), so NOT works as it does in memory.
fn sql(e: &Expr, args: &mut Vec<SqlValue>) -> String {
    match e {
        Expr::And(v) if v.is_empty() => "1".into(),
        Expr::Or(v) if v.is_empty() => "0".into(),
        Expr::And(v) => format!(
            "({})",
            v.iter()
                .map(|e| sql(e, args))
                .collect::<Vec<_>>()
                .join(" AND ")
        ),
        Expr::Or(v) => format!(
            "({})",
            v.iter()
                .map(|e| sql(e, args))
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
        Expr::Not(e) => format!("NOT ({})", sql(e, args)),
        Expr::Text(s) => {
            let n = args.len() + 1;
            args.push(like(s));
            format!(
                "(EXISTS (SELECT 1 FROM track_meta_resolved m WHERE m.track_id = t.id
                   AND m.field IN ('title', 'artist', 'album', 'genre', 'comment')
                   AND lower(m.value) LIKE ?{n} ESCAPE '\\')
                 OR coalesce(lower({FILE_NAME}) LIKE ?{n} ESCAPE '\\', 0)
                 OR EXISTS (SELECT 1 FROM track_tags tt JOIN tags g ON g.id = tt.tag_id
                   WHERE tt.track_id = t.id AND lower(g.name) LIKE ?{n} ESCAPE '\\'))"
            )
        }
        Expr::InCollection(c) => {
            let n = args.len() + 1;
            args.push(SqlValue::Integer(*c));
            format!(
                "t.id IN (SELECT track_id FROM crate_tracks WHERE collection_id = ?{n}
                 UNION SELECT track_id FROM playlist_entries WHERE collection_id = ?{n}
                 UNION SELECT track_id FROM flow_nodes WHERE collection_id = ?{n})"
            )
        }
        Expr::Missing => "NOT EXISTS (SELECT 1 FROM files f WHERE f.track_id = t.id
                          AND f.missing_since IS NULL)"
            .into(),
        Expr::Range(f, lo, hi) => {
            let col = sql_field(*f);
            let n = args.len() + 1;
            args.push(SqlValue::Real(*lo));
            args.push(SqlValue::Real(*hi));
            format!("coalesce({col} BETWEEN ?{n} AND ?{}, 0)", n + 1)
        }
        Expr::Cmp(Field::Tag, op, Value::Text(s)) => {
            let n = args.len() + 1;
            let test = match op {
                Op::Eq => {
                    args.push(SqlValue::Text(s.clone()));
                    format!("lower(g.name) = ?{n}")
                }
                _ => {
                    args.push(like(s));
                    format!("lower(g.name) LIKE ?{n} ESCAPE '\\'")
                }
            };
            format!(
                "EXISTS (SELECT 1 FROM track_tags tt JOIN tags g ON g.id = tt.tag_id
                 WHERE tt.track_id = t.id AND {test})"
            )
        }
        Expr::Cmp(f, op, v) => {
            let col = sql_field(*f);
            let n = args.len() + 1;
            match v {
                Value::Text(s) => match op {
                    Op::Eq => {
                        args.push(SqlValue::Text(s.clone()));
                        format!("coalesce(lower({col}) = ?{n}, 0)")
                    }
                    _ => {
                        args.push(like(s));
                        format!("coalesce(lower({col}) LIKE ?{n} ESCAPE '\\', 0)")
                    }
                },
                Value::Num(x) => {
                    args.push(SqlValue::Real(*x));
                    let o = match op {
                        Op::Contains | Op::Eq => "=",
                        Op::Lt => "<",
                        Op::Le => "<=",
                        Op::Gt => ">",
                        Op::Ge => ">=",
                    };
                    format!("coalesce({col} {o} ?{n}, 0)")
                }
                Value::Key(k) => {
                    let list: Vec<String> = key_spellings(*k)
                        .into_iter()
                        .map(|s| {
                            args.push(SqlValue::Text(s));
                            format!("?{}", args.len())
                        })
                        .collect();
                    format!("coalesce({col} IN ({}), 0)", list.join(", "))
                }
                Value::Bool(b) => format!("({col}) = {}", *b as i32),
                Value::Status(s) => format!("({col}) = {}", *s as i32),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::track::{GridSource, lib_track};

    fn q(s: &str) -> Expr {
        parse(s).unwrap().expr
    }

    fn text(s: &str) -> Expr {
        Expr::Text(s.into())
    }

    #[test]
    fn words_fields_and_ranges() {
        assert_eq!(q(""), Expr::ALL);
        assert_eq!(q("Deep"), text("deep"));
        assert_eq!(
            q("deep \"warm up\" bpm:120-126 key:Am"),
            Expr::And(vec![
                text("deep"),
                text("warm up"),
                Expr::Range(Field::Bpm, 120.0, 126.0),
                Expr::Cmp(Field::Key, Op::Eq, Value::Key(parse_key("8A").unwrap())),
            ])
        );
        assert_eq!(q("bpm:124"), Expr::Range(Field::Bpm, 123.5, 124.5));
        assert_eq!(
            q("bpm>=128"),
            Expr::Cmp(Field::Bpm, Op::Ge, Value::Num(128.0))
        );
        assert_eq!(
            q("time<5:30"),
            Expr::Cmp(Field::Duration, Op::Lt, Value::Num(330.0))
        );
        assert_eq!(
            q("artist:\"Daft Punk\""),
            Expr::Cmp(Field::Artist, Op::Contains, Value::Text("daft punk".into()))
        );
        assert_eq!(
            q("title=One"),
            Expr::Cmp(Field::Title, Op::Eq, Value::Text("one".into()))
        );
        // Two bpm: terms both apply (the old parser kept only the last).
        assert_eq!(
            q("bpm>120 bpm<130"),
            Expr::And(vec![
                Expr::Cmp(Field::Bpm, Op::Gt, Value::Num(120.0)),
                Expr::Cmp(Field::Bpm, Op::Lt, Value::Num(130.0)),
            ])
        );
        // A quoted colon is just text.
        assert_eq!(q("\"feat: x\""), text("feat: x"));
    }

    #[test]
    fn not_or_and_brackets() {
        assert_eq!(
            q("a OR b c"),
            Expr::Or(vec![text("a"), Expr::And(vec![text("b"), text("c")])])
        );
        assert_eq!(q("a | b"), Expr::Or(vec![text("a"), text("b")]));
        assert_eq!(
            q("(a OR b) -tag:vocal"),
            Expr::And(vec![
                Expr::Or(vec![text("a"), text("b")]),
                Expr::Not(Box::new(Expr::Cmp(
                    Field::Tag,
                    Op::Contains,
                    Value::Text("vocal".into())
                ))),
            ])
        );
        assert_eq!(q("NOT is:analysed"), q("-is:analysed"));
        assert_eq!(q("a AND b"), Expr::And(vec![text("a"), text("b")]));
        assert_eq!(
            q("in:7 is:missing"),
            Expr::And(vec![Expr::InCollection(7), Expr::Missing])
        );
        assert_eq!(q("x - y"), Expr::And(vec![text("x"), text("-"), text("y")]));
    }

    #[test]
    fn mistakes_are_errors_not_words() {
        for bad in [
            "genre>3",
            "colour:red",
            "bpm:fast",
            "key:H",
            "\"open",
            "(a",
            "a)",
            "OR a",
            "dj:x",
            "bpm:",
            "is:loud",
            "in:abc",
            "sort:nothing",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("dj:x").unwrap_err().0.contains("no field"));
    }

    #[test]
    fn sort_is_part_of_the_query() {
        let p = parse("deep sort:-bpm sort:title").unwrap();
        assert_eq!(p.expr, text("deep"));
        assert_eq!(
            p.sort,
            Sort(vec![(Field::Bpm, Dir::Desc), (Field::Title, Dir::Asc)])
        );
    }

    fn track(id: i64, title: &str, artist: &str, bpm: Option<f64>, key: &str) -> Track {
        let mut l = lib_track(id, title, artist);
        l.bpm = bpm;
        l.grid_source = bpm.map(|_| GridSource::Analysis);
        l.key = Some(key.into());
        l.tags = vec!["Vocal".into()];
        Track::new(l)
    }

    struct InSeven;
    impl Context for InSeven {
        fn contains(&self, c: i64, t: TrackId) -> bool {
            c == 7 && t == 1
        }
    }

    #[test]
    fn eval_matches_like_the_old_search_and_more() {
        let t = track(1, "Deep Space", "Someone", Some(124.0), "8A");
        let m = |s: &str| eval(&q(s), &t, &InSeven);
        assert!(m("deep bpm:120-126 key:Am"));
        assert!(!m("deep bpm:128"));
        assert!(!m("shallow"));
        assert!(m("space someone"));
        assert!(m("\"deep space\""));
        assert!(m("space.mp3"), "the file name is searched too");
        assert!(m("tag:vocal") && m("tag=vocal") && !m("tag=voc"));
        assert!(m("is:analysed -is:dynamic"));
        assert!(m("shallow OR deep"));
        assert!(!m("-deep"));
        assert!(m("in:7") && !m("in:8"));
        assert!(!m("is:missing"));
        assert!(m("time:300") && m("time>=4:59"));
        assert!(!m("rating>=1"), "no rating: no match, and NOT flips that");
        assert!(m("-rating>=1"));
        let untagged = track(2, "Other", "X", None, "");
        assert!(!eval(&q("bpm<200"), &untagged, &()));
        assert!(!eval(&q("is:analysed"), &untagged, &()));
    }

    #[test]
    fn sorting_puts_missing_values_last() {
        let a = track(1, "A", "x", Some(130.0), "8A");
        let b = track(2, "B", "x", None, "8A");
        let c = track(3, "C", "x", Some(120.0), "8A");
        let mut v = [&a, &b, &c];
        let s = parse("sort:bpm").unwrap().sort;
        v.sort_by(|x, y| compare(&s, x, y, &()));
        assert_eq!(v.map(|t| t.id), [3, 1, 2]);
        let s = parse("sort:-bpm").unwrap().sort;
        v.sort_by(|x, y| compare(&s, x, y, &()));
        assert_eq!(v.map(|t| t.id), [1, 3, 2]);
        let s = parse("sort:-title").unwrap().sort;
        v.sort_by(|x, y| compare(&s, x, y, &()));
        assert_eq!(v.map(|t| t.id), [3, 2, 1]);
    }

    /// Plays and last played, in memory and in SQL.
    #[test]
    fn plays_and_days_since_played() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let mut l = crate::core::track::lib_track(1, "A", "x");
        let never = Track::new(l.clone());
        l.id = 2;
        l.plays = 3;
        l.last_played = Some(now - 2 * 86_400 - 60);
        let twice = Track::new(l);
        let m = |s: &str, t: &Track| eval(&q(s), t, &());
        assert!(m("plays>=3", &twice) && !m("plays>=3", &never));
        assert!(m("plays=0", &never));
        assert!(m("played<7", &twice) && !m("played<2", &twice));
        assert!(m("played:2", &twice), "two days ago");
        assert!(!m("played<7", &never), "never played: no days");
        // Most recent first, never played last.
        let mut old = twice.clone();
        old.id = 3;
        old.last_played = Some(now - 30 * 86_400);
        let mut v = [&never, &old, &twice];
        let s = parse("sort:played").unwrap().sort;
        v.sort_by(|x, y| compare(&s, x, y, &()));
        assert_eq!(v.map(|t| t.id), [2, 3, 1]);
        let s = parse("sort:-plays").unwrap().sort;
        v.sort_by(|x, y| compare(&s, x, y, &()));
        assert_eq!(v[2].id, 1, "no plays: lowest");
    }

    #[test]
    fn key_spellings_cover_the_names() {
        let s = key_spellings(parse_key("8A").unwrap());
        for k in ["8a", "08a", "am", "a min", "aminor"] {
            assert!(s.contains(&k.to_string()), "{k}");
        }
        assert!(!s.contains(&"c".to_string()));
    }
}
