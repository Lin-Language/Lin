// interp.go — arithmetic expression interpreter (tokenize -> recursive-descent
// parse -> tree-walking eval) over 8 fixed expressions, REPS times. Faithful port
// of the Lin/calc algorithm: same grammar, same AST shape, int64 truncating
// division (Go `/` truncates toward zero). Prints exactly one stdout line
// "RESULT=<int>".
//
// Parameters (identical across all languages): REPS=10000 over 8 fixed exprs.
package main

import (
	"fmt"
	"strconv"
)

const reps = 10000

var exprs = [8]string{
	"2 + 3 * 4",
	"(2 + 3) * 4",
	"100 / 5 / 2",
	"2 * (3 + (4 - 1)) * 2",
	"1 + 2 + 3 + 4 + 5 + 6",
	"((8 - 2) * (4 + 1)) / 3",
	"9 * 9 - 8 * 7 + 6",
	"1000 - 7 * (11 + 13) / 2",
}

type tok struct {
	kind string // "num" | "op" | "lparen" | "rparen"
	num  int64
	op   byte
}

type ast struct {
	isNum       bool
	value       int64
	op          byte
	left, right *ast
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }
func isSpace(c byte) bool { return c == ' ' || c == '\t' || c == '\n' || c == '\r' }

func tokenize(src string) []tok {
	var toks []tok
	i, n := 0, len(src)
	for i < n {
		c := src[i]
		switch {
		case isSpace(c):
			i++
		case isDigit(c):
			j := i
			for j < n && isDigit(src[j]) {
				j++
			}
			v, _ := strconv.ParseInt(src[i:j], 10, 64)
			toks = append(toks, tok{kind: "num", num: v})
			i = j
		case c == '(':
			toks = append(toks, tok{kind: "lparen"})
			i++
		case c == ')':
			toks = append(toks, tok{kind: "rparen"})
			i++
		default:
			toks = append(toks, tok{kind: "op", op: c})
			i++
		}
	}
	return toks
}

func kindAt(toks []tok, pos int) string {
	if pos >= len(toks) {
		return "eof"
	}
	return toks[pos].kind
}

func parseFactor(toks []tok, pos int) (*ast, int) {
	if kindAt(toks, pos) == "num" {
		return &ast{isNum: true, value: toks[pos].num}, pos + 1
	}
	inner, p := parseExpr(toks, pos+1) // skip '('
	return inner, p + 1                // skip ')'
}

func parseTerm(toks []tok, pos int) (*ast, int) {
	left, pos := parseFactor(toks, pos)
	for kindAt(toks, pos) == "op" && (toks[pos].op == '*' || toks[pos].op == '/') {
		op := toks[pos].op
		right, p := parseFactor(toks, pos+1)
		left = &ast{op: op, left: left, right: right}
		pos = p
	}
	return left, pos
}

func parseExpr(toks []tok, pos int) (*ast, int) {
	left, pos := parseTerm(toks, pos)
	for kindAt(toks, pos) == "op" && (toks[pos].op == '+' || toks[pos].op == '-') {
		op := toks[pos].op
		right, p := parseTerm(toks, pos+1)
		left = &ast{op: op, left: left, right: right}
		pos = p
	}
	return left, pos
}

func evalNode(node *ast) int64 {
	if node.isNum {
		return node.value
	}
	a := evalNode(node.left)
	b := evalNode(node.right)
	switch node.op {
	case '+':
		return a + b
	case '-':
		return a - b
	case '*':
		return a * b
	default:
		return a / b
	}
}

func eval1(src string) int64 {
	node, _ := parseExpr(tokenize(src), 0)
	return evalNode(node)
}

func main() {
	var total int64 = 0
	for r := 0; r < reps; r++ {
		for _, e := range exprs {
			total += eval1(e)
		}
	}
	fmt.Printf("RESULT=%d\n", total)
}
