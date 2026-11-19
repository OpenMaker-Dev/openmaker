"""Print FileMaker's SQL grammar from the serialized ANTLR ATNs.

Input: the output of `run.sh atn` (fqlgrammar.cpp): rule names, the
vocabulary and the serialized ATN (format version 4, ANTLR 4.10 and later)
of FQLSQLLexer, FQLSQLParser and FQLSQLSemanticAnalyzer. The ATN is the
state machine ANTLR compiles a grammar into; its construction is regular
(one state pattern per block, optional, star and plus loop), so walking it
gives back the grammar in EBNF. Format and construction are those of the
public ANTLR 4 runtime (ATNDeserializer, ParserATNFactory, LexerATNFactory).

usage: python3 -I atn_grammar.py grammar-dump.txt [recognizer ...]
"""
import sys

# ATNState types and Transition types of the ANTLR 4 serialization.
BASIC, RULE_START, BLOCK_START, PLUS_BLOCK_START, STAR_BLOCK_START, TOKEN_START, RULE_STOP, \
    BLOCK_END, STAR_LOOP_BACK, STAR_LOOP_ENTRY, PLUS_LOOP_BACK, LOOP_END = range(1, 13)
EPSILON, RANGE, RULE, PREDICATE, ATOM, ACTION, SET, NOT_SET, WILDCARD, PRECEDENCE = range(1, 11)
LEXER_ACTIONS = {0: 'channel', 1: 'custom', 2: 'mode', 3: 'more', 4: 'popMode', 5: 'pushMode', 6: 'skip', 7: 'type'}


class State:
    def __init__(self, n, kind, rule):
        self.n, self.kind, self.rule = n, kind, rule
        self.end = self.loopback = None
        self.edges = []


class Recognizer:
    def __init__(self, name):
        self.name = name
        self.rules, self.literal, self.symbolic = [], {}, {}
        self.atn = []
        self.modes, self.channels = [], []

    def load(self):
        d = self.atn
        p = 0

        def take():
            nonlocal p
            p += 1
            return d[p - 1]
        version, self.gtype, self.max_token = take(), take(), take()
        assert version == 4, version
        self.states = []
        for i in range(take()):
            kind = take()
            if kind == 0:
                self.states.append(None)
                continue
            s = State(i, kind, take())
            if kind == LOOP_END:
                s.loopback = take()
            elif kind in (BLOCK_START, PLUS_BLOCK_START, STAR_BLOCK_START):
                s.end = take()
            self.states.append(s)
        self.nongreedy = {take() for _ in range(take())}
        self.precedence = {take() for _ in range(take())}
        self.rule_start, self.rule_token = [], []
        for _ in range(take()):
            self.rule_start.append(take())
            if self.gtype == 0:
                self.rule_token.append(take())
        self.mode_start = [take() for _ in range(take())]
        self.sets = []
        for _ in range(take()):
            n, eof = take(), take()
            iv = [(-1, -1)] if eof else []
            iv += [(take(), take()) for _ in range(n)]
            self.sets.append(iv)
        for _ in range(take()):
            src, trg, kind, a1, a2, a3 = (take() for _ in range(6))
            self.states[src].edges.append((kind, trg, a1, a2, a3))
        self.decisions = [take() for _ in range(take())]
        self.actions = []
        if self.gtype == 0:
            self.actions = [(take(), take(), take()) for _ in range(take())]
        assert p == len(d), (p, len(d))

    # ---- terminals
    def char(self, c):
        if c == -1:
            return 'EOF'
        named = {9: '\\t', 10: '\\n', 13: '\\r', 39: "\\'", 92: '\\\\'}
        if c in named:
            return named[c]
        if 32 <= c < 127:
            return chr(c)
        return '\\u%04X' % c if c < 0x10000 else '\\u{%X}' % c

    def token(self, t):
        if t == -1:
            return 'EOF'
        return self.literal.get(t) or self.symbolic.get(t) or '<%d>' % t

    def charset(self, iv, negate):
        parts = []
        for a, b in iv:
            if a == -1:
                parts.append('EOF')
                continue
            ca, cb = self.char(a).replace(']', '\\]'), self.char(b).replace(']', '\\]')
            parts.append(ca if a == b else '%s-%s' % (ca, cb))
        return ('~' if negate else '') + '[' + ''.join(parts) + ']'

    def tokenset(self, iv, negate):
        names = []
        for a, b in iv:
            names += [self.token(t) for t in range(a, b + 1)]
        body = names[0] if len(names) == 1 else '(' + ' | '.join(names) + ')'
        return ('~' if negate else '') + body

    def terminal(self, kind, a1, a2, a3):
        lexer = self.gtype == 0
        if kind == ATOM:
            t = -1 if a3 else a1
            return "'%s'" % self.char(t) if lexer and t != -1 else self.token(t)
        if kind == RANGE:
            a = -1 if a3 else a1
            return "'%s'..'%s'" % (self.char(a), self.char(a2)) if lexer else '%s..%s' % (self.token(a), self.token(a2))
        if kind in (SET, NOT_SET):
            iv = self.sets[a1]
            return self.charset(iv, kind == NOT_SET) if lexer else self.tokenset(iv, kind == NOT_SET)
        if kind == WILDCARD:
            return '.'
        if kind == PREDICATE:
            return '{pred %d:%d}?' % (a1, a2)
        if kind == PRECEDENCE:
            return '{precpred %d}?' % a1
        if kind == ACTION:
            if lexer and 0 <= a2 < len(self.actions):
                return '{action %s}' % self.lexer_action(self.actions[a2])
            return '{action %d:%d}' % (a1, a2)
        raise ValueError(kind)

    def lexer_action(self, a):
        kind, d1, d2 = a
        name = LEXER_ACTIONS.get(kind, str(kind))
        if kind == 0:
            return 'channel(%s)' % (self.channels[d1] if d1 < len(self.channels) else d1)
        if kind == 7:
            return 'type(%s)' % self.token(d1)
        if kind in (2, 5):
            return '%s(%s)' % (name, self.modes[d1] if d1 < len(self.modes) else d1)
        if kind == 1:
            return 'custom(%d,%d)' % (d1, d2)
        return name

    # ---- structure
    def walk(self, s, stop):
        """Elements from state s up to (not including) state stop."""
        out = []
        seen = set()
        while s != stop:
            if s in seen:
                out.append('<loop %d>' % s)
                break
            seen.add(s)
            st = self.states[s]
            if st.kind == RULE_STOP:
                break
            if st.kind == BLOCK_START:
                out.append(self.group([self.walk(e[1], st.end) for e in st.edges], ''))
                s = self.states[st.end].edges[0][1]
                continue
            if st.kind == STAR_LOOP_ENTRY:
                blk = next(self.states[e[1]] for e in st.edges if self.states[e[1]].kind == STAR_BLOCK_START)
                end = next(self.states[e[1]] for e in st.edges if self.states[e[1]].kind == LOOP_END)
                lazy = '?' if s in self.nongreedy else ''
                prec = ' {precedence loop}' if any(
                    self.states[e[1]].edges and self.states[e[1]].edges[0][0] == PRECEDENCE for e in blk.edges) else ''
                out.append(self.group([self.walk(e[1], blk.end) for e in blk.edges], '*' + lazy) + prec)
                s = end.edges[0][1]
                continue
            if st.kind == PLUS_BLOCK_START:
                g = self.group([self.walk(e[1], st.end) for e in st.edges], '+')
                back = self.states[self.states[st.end].edges[0][1]]
                end = next(self.states[e[1]] for e in back.edges if self.states[e[1]].kind == LOOP_END)
                out.append(g + ('?' if back.n in self.nongreedy else ''))
                s = end.edges[0][1]
                continue
            if len(st.edges) != 1:
                out.append('<state %d kind %d with %d edges>' % (s, st.kind, len(st.edges)))
                break
            kind, trg, a1, a2, a3 = st.edges[0]
            if kind == EPSILON:
                s = trg
            elif kind == RULE:
                out.append(self.rules[a2] + ('<prec %d>' % a3 if a3 else ''))
                s = trg
            else:
                out.append(self.terminal(kind, a1, a2, a3))
                s = trg
        return out

    def group(self, alts, suffix):
        empty = [a for a in alts if not a]
        alts = [a for a in alts if a]
        if empty and not suffix:
            suffix = '?'
        if len(alts) == 1 and len(alts[0]) == 1 and suffix:
            inner = alts[0][0]
            if ' ' not in inner or (inner.startswith('(') and inner.endswith(')')):
                return inner + suffix
        return '(' + ' | '.join(' '.join(a) for a in alts) + ')' + suffix

    def rule_alts(self, r):
        start = self.states[self.rule_start[r]]
        stop = next(s.n for s in self.states if s and s.kind == RULE_STOP and s.rule == r)
        s = start.edges[0][1]
        st = self.states[s]
        # a rule with several alternatives starts with a block whose end leads to the stop state
        if st.kind == BLOCK_START and self.states[st.end].edges[0][1] == stop:
            return [self.walk(e[1], st.end) for e in st.edges]
        return [self.walk(s, stop)]

    def print_grammar(self, out):
        kind = 'lexer grammar' if self.gtype == 0 else 'parser grammar'
        out.write('// %s %s: %d rules, ATN of %d states, %d decisions\n' % (
            kind, self.name, len(self.rules), len(self.states), len(self.decisions)))
        tokens_rules = set()
        if self.gtype == 0:
            for m in self.mode_start:
                for e in self.states[m].edges:
                    tokens_rules.add(self.states[e[1]].rule)
        for r, name in enumerate(self.rules):
            alts = self.rule_alts(r)
            head = name
            if self.gtype == 0 and r not in tokens_rules:
                head = 'fragment ' + name
            if self.rule_start[r] in self.precedence:
                head += '  // left-recursive (precedence rule)'
            body = ('\n    | ').join(' '.join(a) if a else '/* empty */' for a in alts)
            out.write('%s\n    : %s\n    ;\n' % (head, body))


def read(path):
    recs, cur = [], None
    for line in open(path, encoding='utf-8'):
        f = line.rstrip('\n').split('\t')
        if f[0] == 'recognizer':
            cur = Recognizer(f[1])
            recs.append(cur)
        elif f[0] == 'rule':
            cur.rules.append(f[2])
        elif f[0] == 'token':
            t = int(f[1])
            if f[2]:
                cur.literal[t] = f[2]
            if f[3]:
                cur.symbolic[t] = f[3]
        elif f[0] == 'atn':
            cur.atn = [int(x) for x in f[1].split()[1:]]
        elif f[0] == 'mode':
            recs[0].modes.append(f[1])
        elif f[0] == 'channel':
            recs[0].channels.append(f[1])
    return recs


def main():
    recs = read(sys.argv[1])
    want = set(sys.argv[2:])
    for r in recs:
        if want and r.name not in want:
            continue
        r.load()
        r.print_grammar(sys.stdout)
        sys.stdout.write('\n')


if __name__ == '__main__':
    main()
