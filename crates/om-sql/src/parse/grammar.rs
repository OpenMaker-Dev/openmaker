//! The FQL grammar (execute-sql.md 3.2) as text, and its reading into
//! rules of alternatives.
//!
//! The text is section 3.2's, with one change ANTLR itself makes: the
//! left-recursive `value_expression` becomes its primary alternative
//! followed by a loop of its suffix (`( '+' | '-' ) 'INTERVAL' ...`). Blocks
//! of two or more single-token alternatives become token sets, as ANTLR's
//! `BlockSetTransformer` does for parsers (`comparison`, `( '+' | '-' )`).

use super::lex::{self, TokenType};

/// The grammar text. Literals in quotes are keywords or punctuation,
/// upper-case words token classes, lower-case words rules.
pub const GRAMMAR: &str = r#"
statement               : ddl_statement EOF | dml_statement EOF ;

dml_statement           : query_statement | update_statement | delete_statement
                        | insert_statement | declare_cursor_statement
                        | deallocate_cursor_statement | open_cursor_statement
                        | close_cursor_statement | fetch_cursor_statement
                        | truncate_table_statement ;

query_statement         : query_expression order_by? result_offset? fetch_first?
                          ( 'FOR' 'UPDATE' ( 'OF' column_reference_list )? )? ;
query_expression        : query_expression_primary query_expression_union* ;
query_expression_union  : 'UNION' query_dup_opt? query_expression_primary ;
query_expression_primary: 'TABLE' IDENTIFIER | query_spec | '(' query_expression ')' ;
query_spec              : 'SELECT' query_dup_opt? select_list from_clause
                          where_clause? group_by_clause? having_clause? ;
query_dup_opt           : 'ALL' | 'DISTINCT' ;
select_list             : '*' | select_list_item ( ',' select_list_item )* ;
select_list_item        : IDENTIFIER '.' '*'
                        | 'GETAS' '(' value_expression ',' ( STRING_CONST | 'DEFAULT' ) ')'
                          ( 'AS'? IDENTIFIER )?
                        | value_expression ( 'AS'? IDENTIFIER )? ;

from_clause             : 'FROM' table_reference ( ',' table_reference )* ;
table_reference         : simple_table_reference table_join_reference* ;
simple_table_reference  : IDENTIFIER ( 'AS'? IDENTIFIER )? | '(' table_reference ')' ;
table_join_reference    : 'INNER'? 'JOIN' simple_table_reference 'ON' search_term
                        | 'LEFT' 'OUTER'? 'JOIN' simple_table_reference 'ON' search_term
                        | 'CROSS' 'JOIN' simple_table_reference ;

where_clause            : 'WHERE' search_term ;
group_by_clause         : 'GROUP' 'BY' column_reference_list ;
having_clause           : 'HAVING' search_term ;
column_reference_list   : column_reference ( ',' column_reference )* ;
order_by                : 'ORDER' 'BY' order_spec ( ',' order_spec )* ;
order_spec              : value_expression ( 'ASC' | 'DESC' )? ;
result_offset           : 'OFFSET' constant ( 'ROW' | 'ROWS' ) ;
fetch_first             : 'FETCH' ( 'FIRST' | 'NEXT' ) ( constant 'PERCENT'? )?
                          ( 'ROW' | 'ROWS' ) ( 'ONLY' | 'WITH' 'TIES' ) ;

search_term             : boolean_term ( 'OR' boolean_term )* ;
boolean_term            : boolean_factor ( 'AND' boolean_factor )* ;
boolean_factor          : 'NOT'? boolean_primary ;
boolean_primary         : value_expression comparison quantifier_op
                          '(' ( value_expression_list | subquery ) ')'
                        | value_expression comparison value_expression
                        | value_expression 'NOT'? 'IN' '(' ( value_expression_list | subquery ) ')'
                        | value_expression 'IS' 'NOT'? ( 'FALSE' | 'NULL' | 'TRUE' | 'UNKNOWN' )
                        | value_expression 'NOT'? 'BETWEEN' value_expression 'AND' value_expression
                        | value_expression 'NOT'? 'LIKE' value_expression
                        | value_expression 'NOT'? 'PART' 'OF' value_expression
                        | value_expression ( '+' | '-' ) 'INTERVAL' value_expression extract_operation
                        | ( 'EXISTS' | 'UNIQUE' ) subquery
                        | '(' search_term ')' ;
comparison              : NEQ | '=' | '<' | '<=' | '>' | '>=' ;
quantifier_op           : 'ALL' | 'ANY' | 'SOME' ;

value_expression        : value_term ( ( '+' | '-' | '||' ) value_term )*
                          ( ( '+' | '-' ) 'INTERVAL' value_term extract_operation )* ;
value_term              : value_pow ( ( '/' | '*' ) value_pow )* ;
value_pow               : value_factor ( RAISE value_factor )* ;
value_factor            : ( '+' | '-' )? value_primary ;
value_primary           : column_reference | constant | case_expression | cast_expression
                        | aggregation_function | sql_function | '(' value_expression ')'
                        | subquery
                        | 'NULLIF' '(' value_expression ',' value_expression ')'
                        | 'COALESCE' '(' value_expression_list ')' ;
value_expression_list   : common_expression ( ',' common_expression )* ;
common_expression       : search_term | value_expression ;
subquery                : '(' query_expression ')' | query_expression ;

column_reference        : ( IDENTIFIER '.' )? ( 'ROWID' | QUOTED_ROWID )
                        | ( IDENTIFIER '.' )? IDENTIFIER ( '[' value_expression ']' )? ;
constant                : FLOAT_CONST | UNSIGNED_CONST | STRING_CONST | '?' | ':' IDENTIFIER
                        | 'DATE' STRING_CONST | 'TIME' STRING_CONST | 'TIMESTAMP' STRING_CONST
                        | BIT_CONST | HEX_CONST | 'DEFAULT' | 'NULL' | 'TRUE' | 'FALSE' ;

aggregation_function    : 'COUNT' '(' '*' ')'
                        | ( 'AVG' | 'COUNT' | 'EVERY' | 'MAX' | 'MIN' | 'SOME' | 'SUM' )
                          '(' ( 'ALL' | 'DISTINCT' )? value_expression ')' ;
sql_function            : 'EXTRACT' '(' extract_operation 'FROM' value_expression ')'
                        | 'TRIM' '(' trim_option value_expression? 'FROM' value_expression ')'
                        | 'TRIM' '(' value_expression 'FROM' value_expression ')'
                        | 'TRIM' '(' value_expression ')'
                        | function_name ( '(' value_expression_list ')' )?
                        | IDENTIFIER '(' value_expression_list? ')'
                        | ( 'DATE' | 'TIME' | 'TIMESTAMP' ) '(' ')' ;
extract_operation       : 'DAY' | 'HOUR' | 'MINUTE' | 'MONTH' | 'SECOND' | 'YEAR' ;
trim_option             : 'BOTH' | 'LEADING' | 'TRAILING' ;
function_name           : 'CHR' | 'CURDATE' | 'CURRENT_DATE' | 'CURRENT_TIME' | 'CURRENT_TIMESTAMP'
                        | 'CURRENT_USER' | 'CURTIME' | 'CURTIMESTAMP' | 'DATEVAL' | 'DAY'
                        | 'DAYNAME' | 'DAYOFWEEK' | 'HOUR' | 'INT' | 'LEFT' | 'LENGTH' | 'LOWER'
                        | 'LTRIM' | 'MAX' | 'MIN' | 'MINUTE' | 'MONTH' | 'MONTHNAME' | 'NUMVAL'
                        | 'PART' | 'RIGHT' | 'ROUND' | 'RTRIM' | 'SECOND' | 'SPACE' | 'STRVAL'
                        | 'SUBSTR' | 'SUBSTRING' | 'TIMESTAMPVAL' | 'TIMEVAL' | 'TODAY' | 'UPPER'
                        | 'USER' | 'USERNAME' | 'YEAR' ;
case_expression         : 'CASE' case_searched_when_then+ case_else 'END'
                        | 'CASE' value_expression case_simple_when_then+ case_else 'END' ;
case_simple_when_then   : 'WHEN' common_expression 'THEN' value_expression ;
case_searched_when_then : 'WHEN' search_term 'THEN' value_expression ;
case_else               : 'ELSE' ( value_expression | 'NULL' ) | ;
cast_expression         : 'CAST' '(' value_expression 'AS' data_type ')' ;
data_type               : 'INT'
                        | ( 'DECIMAL' | 'NUMERIC' ) ( '(' UNSIGNED_CONST ( ',' UNSIGNED_CONST )? ')' )?
                        | 'BOOLEAN' | 'DATE'
                        | 'TIME' ( '(' UNSIGNED_CONST ')' )?
                        | 'TIMESTAMP' ( '(' UNSIGNED_CONST ')' )?
                        | ( 'VARCHAR' | 'CHARACTER' 'VARYING' ) ( '(' UNSIGNED_CONST ')' )?
                        | ( 'BLOB' | 'VARBINARY' | 'LONGVARBINARY' | 'BINARY' 'VARYING' )
                          ( 'EXTERNAL' STRING_CONST ( 'SECURE' 'FEWER_FOLDERS'? | 'OPEN' STRING_CONST ) )? ;

update_statement        : 'UPDATE' IDENTIFIER 'SET' update_assignment ( ',' update_assignment )*
                          ( where_clause? | where_cursor? ) ;
where_cursor            : 'WHERE' 'CURRENT' 'OF' IDENTIFIER ;
update_assignment       : IDENTIFIER ( '[' value_expression ']' )? '='
                          ( value_expression
                          | 'PUTAS' '(' value_expression ',' STRING_CONST ')'
                          | value_expression 'AS' STRING_CONST ) ;
delete_statement        : 'DELETE' 'FROM' IDENTIFIER ( where_clause? | where_cursor ) ;
insert_statement        : 'INSERT' 'INTO' IDENTIFIER ( '(' insert_columns ')' )? insert_value_source
                        | 'INSERT' 'INTO' IDENTIFIER ( '(' insert_columns ')' )? query_expression order_by?
                        | 'INSERT' 'INTO' IDENTIFIER 'DEFAULT' 'VALUES' ;
insert_value_source     : 'VALUES' row_value_list_entry ( ',' row_value_list_entry )* ;
insert_columns          : insert_column_item ( ',' insert_column_item )* | ;
insert_column_item      : IDENTIFIER ( '[' UNSIGNED_CONST ']' )? ;
row_value_list_entry    : '(' row_value_element ( ',' row_value_element )* ')' ;
row_value_element       : 'PUTAS' '(' value_expression ',' STRING_CONST ')'
                        | value_expression 'AS' STRING_CONST | value_expression ;
declare_cursor_statement    : 'DECLARE' IDENTIFIER 'CURSOR' 'FOR' query_statement ;
open_cursor_statement       : 'OPEN' IDENTIFIER ;
close_cursor_statement      : 'CLOSE' IDENTIFIER ;
deallocate_cursor_statement : 'DEALLOCATE' IDENTIFIER ;
fetch_cursor_statement      : 'FETCH' fetch_type 'FROM'? IDENTIFIER ;
fetch_type              : 'NEXT' | 'PRIOR' | 'FIRST' | 'LAST'
                        | ( 'ABSOLUTE' | 'RELATIVE' ) '-'? UNSIGNED_CONST | ;
truncate_table_statement: 'TRUNCATE' 'TABLE' IDENTIFIER ;

ddl_statement           : create_index_statement | create_table_statement | drop_index_statement
                        | drop_table_statement | alter_table_statement ;
create_table_statement  : 'CREATE' 'TABLE' IDENTIFIER '(' column_definition ( ',' column_definition )* ')' ;
column_definition       : IDENTIFIER data_type ( '[' UNSIGNED_CONST ']' )? default_clause? column_constraint*
                        | IDENTIFIER data_type 'FOREIGN' 'KEY' 'REFERENCES' IDENTIFIER '(' IDENTIFIER ')' ;
default_clause          : 'DEFAULT' ( default_literal | 'CURRENT_USER' | 'USER' | 'USERNAME' | 'NULL'
                        | 'CURDATE' | 'CURRENT_DATE' | 'CURRENT_TIME' | 'CURTIME'
                        | 'CURRENT_TIMESTAMP' | 'CURTIMESTAMP' ) ;
default_literal         : ( '+' | '-' )? ( UNSIGNED_CONST | FLOAT_CONST ) | STRING_CONST | BIT_CONST
                        | HEX_CONST | ( 'TIME' | 'DATE' | 'TIMESTAMP' ) STRING_CONST | 'TRUE' | 'FALSE' ;
column_constraint       : 'NOT' 'NULL' | 'UNIQUE' | 'PRIMARY' 'KEY' | 'GLOBAL' ;
drop_table_statement    : 'DROP' 'TABLE' IDENTIFIER ;
create_index_statement  : 'CREATE' 'INDEX' 'ON'? IDENTIFIER ( '.' IDENTIFIER | '(' IDENTIFIER ')' ) ;
drop_index_statement    : 'DROP' 'INDEX' 'ON'? IDENTIFIER ( '.' IDENTIFIER | '(' IDENTIFIER ')' ) ;
alter_table_statement   : 'ALTER' 'TABLE' IDENTIFIER alter_table_action ;
alter_table_action      : 'ADD' 'COLUMN'? ( column_definition | '(' column_definition ')' )
                        | 'DROP' 'COLUMN'? IDENTIFIER
                        | 'ALTER' 'COLUMN'? IDENTIFIER ( 'SET' default_clause | 'DROP' 'DEFAULT' )
                        | 'RENAME' 'TO' IDENTIFIER | 'RENAME' 'COLUMN' IDENTIFIER 'TO' IDENTIFIER ;
"#;

/// A grammar element's repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suffix {
    One,
    Optional,
    Star,
    Plus,
}

#[derive(Clone, Debug)]
pub enum Atom {
    Token(TokenType),
    /// A set of single tokens (a block of single-token alternatives).
    Set(Vec<TokenType>),
    Rule(usize),
    Block(Vec<Alt>),
}

#[derive(Clone, Debug)]
pub struct Element {
    pub atom: Atom,
    pub suffix: Suffix,
}

/// An alternative: a sequence of elements (empty for `| ;`).
pub type Alt = Vec<Element>;

#[derive(Clone, Debug)]
pub struct Rule {
    pub name: &'static str,
    pub alts: Vec<Alt>,
}

/// The rules of [`GRAMMAR`], rule 0 being `statement`.
pub fn rules() -> Vec<Rule> {
    let words = words(GRAMMAR);
    // Rule names first, so references can be resolved in one pass.
    let mut names = Vec::new();
    for w in words.windows(2) {
        if w[1] == ":" {
            names.push(w[0]);
        }
    }
    let mut reader = Reader {
        words: &words,
        at: 0,
        names: &names,
    };
    let mut rules = Vec::new();
    while reader.at < words.len() {
        let name = reader.next();
        reader.expect(":");
        let alts = reader.alts();
        reader.expect(";");
        rules.push(Rule {
            name,
            alts: to_sets(alts, true),
        });
    }
    rules
}

/// The grammar's words: quoted literals, names, and `: ; | ( ) ? * +`.
fn words(text: &'static str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'\'' {
            let end = i + 1 + text[i + 1..].find('\'').expect("closed literal");
            // `'''` is not used; a literal ends at the next quote.
            out.push(&text[i..=end]);
            i = end + 1;
        } else if c.is_ascii_alphanumeric() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(&text[start..i]);
        } else {
            out.push(&text[i..=i]);
            i += 1;
        }
    }
    out
}

struct Reader<'a> {
    words: &'a [&'static str],
    at: usize,
    names: &'a [&'static str],
}

impl Reader<'_> {
    fn peek(&self) -> &'static str {
        self.words.get(self.at).copied().unwrap_or("")
    }

    fn next(&mut self) -> &'static str {
        let w = self.peek();
        self.at += 1;
        w
    }

    fn expect(&mut self, word: &str) {
        let got = self.next();
        assert_eq!(got, word, "grammar: expected {word} at word {}", self.at);
    }

    fn alts(&mut self) -> Vec<Alt> {
        let mut alts = vec![self.alt()];
        while self.peek() == "|" {
            self.at += 1;
            alts.push(self.alt());
        }
        alts
    }

    fn alt(&mut self) -> Alt {
        let mut elements = Vec::new();
        while !matches!(self.peek(), "|" | ")" | ";" | "") {
            let atom = self.atom();
            let suffix = match self.peek() {
                "?" => Suffix::Optional,
                "*" => Suffix::Star,
                "+" => Suffix::Plus,
                _ => Suffix::One,
            };
            if suffix != Suffix::One {
                self.at += 1;
            }
            elements.push(Element { atom, suffix });
        }
        elements
    }

    fn atom(&mut self) -> Atom {
        let w = self.next();
        if w == "(" {
            let alts = self.alts();
            self.expect(")");
            return Atom::Block(alts);
        }
        if let Some(literal) = w.strip_prefix('\'') {
            let literal = literal.trim_end_matches('\'');
            let ty = lex::keyword(literal)
                .or_else(|| lex::punctuation(literal))
                .unwrap_or_else(|| panic!("grammar: unknown literal {w}"));
            return Atom::Token(ty);
        }
        if let Some(rule) = self.names.iter().position(|n| *n == w) {
            return Atom::Rule(rule);
        }
        Atom::Token(match w {
            "EOF" => lex::EOF,
            "IDENTIFIER" => lex::IDENTIFIER,
            "QUOTED_ROWID" => lex::QUOTED_ROWID,
            "STRING_CONST" => lex::STRING_CONST,
            "UNSIGNED_CONST" => lex::UNSIGNED_CONST,
            "FLOAT_CONST" => lex::FLOAT_CONST,
            "BIT_CONST" => lex::BIT_CONST,
            "HEX_CONST" => lex::HEX_CONST,
            "NEQ" => lex::NEQ,
            "RAISE" => lex::RAISE,
            other => panic!("grammar: unknown name {other}"),
        })
    }
}

/// A single token of an alternative that is just that token.
fn single_token(alt: &Alt) -> Option<TokenType> {
    match alt.as_slice() {
        [Element {
            atom: Atom::Token(t),
            suffix: Suffix::One,
        }] => Some(*t),
        _ => None,
    }
}

/// ANTLR's block-to-set reduction, applied bottom-up: a block of two or
/// more alternatives that are each a single token becomes a set; the rule's
/// own block becomes one alternative holding the set.
fn to_sets(alts: Vec<Alt>, rule_block: bool) -> Vec<Alt> {
    let alts: Vec<Alt> = alts
        .into_iter()
        .map(|alt| {
            alt.into_iter()
                .map(|e| match e.atom {
                    Atom::Block(inner) => {
                        let inner = to_sets(inner, false);
                        let atom = match inner.as_slice() {
                            [single] if single.len() == 1 && single[0].suffix == Suffix::One => {
                                match &single[0].atom {
                                    Atom::Set(s) => Atom::Set(s.clone()),
                                    _ => Atom::Block(inner),
                                }
                            }
                            _ => Atom::Block(inner),
                        };
                        Element {
                            atom,
                            suffix: e.suffix,
                        }
                    }
                    other => Element {
                        atom: other,
                        suffix: e.suffix,
                    },
                })
                .collect()
        })
        .collect();
    if alts.len() >= 2 {
        let tokens: Option<Vec<TokenType>> = alts.iter().map(single_token).collect();
        if let Some(tokens) = tokens {
            let set = Element {
                atom: Atom::Set(tokens),
                suffix: Suffix::One,
            };
            let _ = rule_block;
            return vec![vec![set]];
        }
    }
    alts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_rule() {
        let rules = rules();
        assert_eq!(rules[0].name, "statement");
        assert_eq!(rules.len(), 77);
        let comparison = rules.iter().find(|r| r.name == "comparison").unwrap();
        assert!(
            matches!(&comparison.alts[..], [alt] if matches!(&alt[0].atom, Atom::Set(s) if s.len() == 6))
        );
        let factor = rules.iter().find(|r| r.name == "value_factor").unwrap();
        assert!(matches!(&factor.alts[0][0].atom, Atom::Set(s) if s.len() == 2));
        assert_eq!(factor.alts[0][0].suffix, Suffix::Optional);
    }
}
