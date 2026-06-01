# interp.py — arithmetic expression interpreter (tokenize -> recursive-descent
# parse -> tree-walking eval) over 8 fixed expressions, REPS times. Faithful port
# of the Lin/calc algorithm: same grammar, same AST shape, same truncating integer
# division. Prints exactly one stdout line "RESULT=<int>".
#
# Parameters (identical across all languages): REPS=10000 over 8 fixed exprs.
import sys

REPS = 10000

EXPRS = [
    "2 + 3 * 4",
    "(2 + 3) * 4",
    "100 / 5 / 2",
    "2 * (3 + (4 - 1)) * 2",
    "1 + 2 + 3 + 4 + 5 + 6",
    "((8 - 2) * (4 + 1)) / 3",
    "9 * 9 - 8 * 7 + 6",
    "1000 - 7 * (11 + 13) / 2",
]


def tokenize(src):
    toks = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c.isspace():
            i += 1
        elif c.isdigit():
            j = i
            while j < n and src[j].isdigit():
                j += 1
            toks.append(("num", src[i:j]))
            i = j
        elif c == "(":
            toks.append(("lparen", "("))
            i += 1
        elif c == ")":
            toks.append(("rparen", ")"))
            i += 1
        else:
            toks.append(("op", c))
            i += 1
    return toks


# Parser: each fn returns (node, pos). node is a tuple: ("num", value) or
# ("binop", op, left, right).
def kind_at(toks, pos):
    return "eof" if pos >= len(toks) else toks[pos][0]


def parse_factor(toks, pos):
    if kind_at(toks, pos) == "num":
        return ("num", int(toks[pos][1])), pos + 1
    inner, p = parse_expr(toks, pos + 1)  # skip '('
    return inner, p + 1  # skip ')'


def parse_term_loop(toks, left, pos):
    while kind_at(toks, pos) == "op" and toks[pos][1] in ("*", "/"):
        op = toks[pos][1]
        right, pos = parse_factor(toks, pos + 1)
        left = ("binop", op, left, right)
    return left, pos


def parse_term(toks, pos):
    first, pos = parse_factor(toks, pos)
    return parse_term_loop(toks, first, pos)


def parse_expr_loop(toks, left, pos):
    while kind_at(toks, pos) == "op" and toks[pos][1] in ("+", "-"):
        op = toks[pos][1]
        right, pos = parse_term(toks, pos + 1)
        left = ("binop", op, left, right)
    return left, pos


def parse_expr(toks, pos):
    first, pos = parse_term(toks, pos)
    return parse_expr_loop(toks, first, pos)


def trunc_div(a, b):
    # truncate toward zero (match C/Rust/Go/Lin int division)
    q = abs(a) // abs(b)
    return -q if (a < 0) != (b < 0) else q


def eval_node(node):
    if node[0] == "num":
        return node[1]
    a = eval_node(node[2])
    b = eval_node(node[3])
    op = node[1]
    if op == "+":
        return a + b
    if op == "-":
        return a - b
    if op == "*":
        return a * b
    return trunc_div(a, b)


def eval1(src):
    node, _ = parse_expr(tokenize(src), 0)
    return eval_node(node)


def main():
    total = 0
    for _ in range(REPS):
        for e in EXPRS:
            total += eval1(e)
    sys.stdout.write("RESULT=%d\n" % total)


main()
