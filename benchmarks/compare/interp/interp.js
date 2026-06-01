// interp.js — arithmetic expression interpreter (tokenize -> recursive-descent
// parse -> tree-walking eval) over 8 fixed expressions, REPS times. Faithful port
// of the Lin/calc algorithm. Integer math uses Math.trunc on division to match
// C/Rust/Go/Lin truncating division. Prints exactly one stdout line "RESULT=<int>".
//
// Parameters (identical across all languages): REPS=10000 over 8 fixed exprs.
'use strict';

const REPS = 10000;

const EXPRS = [
  "2 + 3 * 4",
  "(2 + 3) * 4",
  "100 / 5 / 2",
  "2 * (3 + (4 - 1)) * 2",
  "1 + 2 + 3 + 4 + 5 + 6",
  "((8 - 2) * (4 + 1)) / 3",
  "9 * 9 - 8 * 7 + 6",
  "1000 - 7 * (11 + 13) / 2",
];

function isDigit(c) { return c >= '0' && c <= '9'; }
function isSpace(c) { return c === ' ' || c === '\t' || c === '\n' || c === '\r'; }

function tokenize(src) {
  const toks = [];
  let i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i];
    if (isSpace(c)) { i++; }
    else if (isDigit(c)) {
      let j = i;
      while (j < n && isDigit(src[j])) j++;
      toks.push(["num", src.slice(i, j)]);
      i = j;
    } else if (c === '(') { toks.push(["lparen", "("]); i++; }
    else if (c === ')') { toks.push(["rparen", ")"]); i++; }
    else { toks.push(["op", c]); i++; }
  }
  return toks;
}

// Parser: each fn returns [node, pos]. node = ["num", value] | ["binop", op, left, right].
function kindAt(toks, pos) { return pos >= toks.length ? "eof" : toks[pos][0]; }

function parseFactor(toks, pos) {
  if (kindAt(toks, pos) === "num") {
    return [["num", parseInt(toks[pos][1], 10)], pos + 1];
  }
  const [inner, p] = parseExpr(toks, pos + 1); // skip '('
  return [inner, p + 1];                       // skip ')'
}

function parseTermLoop(toks, left, pos) {
  while (kindAt(toks, pos) === "op" && (toks[pos][1] === "*" || toks[pos][1] === "/")) {
    const op = toks[pos][1];
    const [right, p] = parseFactor(toks, pos + 1);
    left = ["binop", op, left, right];
    pos = p;
  }
  return [left, pos];
}

function parseTerm(toks, pos) {
  const [first, p] = parseFactor(toks, pos);
  return parseTermLoop(toks, first, p);
}

function parseExprLoop(toks, left, pos) {
  while (kindAt(toks, pos) === "op" && (toks[pos][1] === "+" || toks[pos][1] === "-")) {
    const op = toks[pos][1];
    const [right, p] = parseTerm(toks, pos + 1);
    left = ["binop", op, left, right];
    pos = p;
  }
  return [left, pos];
}

function parseExpr(toks, pos) {
  const [first, p] = parseTerm(toks, pos);
  return parseExprLoop(toks, first, p);
}

function evalNode(node) {
  if (node[0] === "num") return node[1];
  const a = evalNode(node[2]);
  const b = evalNode(node[3]);
  const op = node[1];
  if (op === "+") return a + b;
  if (op === "-") return a - b;
  if (op === "*") return a * b;
  return Math.trunc(a / b);
}

function eval1(src) {
  const [node] = parseExpr(tokenize(src), 0);
  return evalNode(node);
}

function main() {
  let total = 0;
  for (let r = 0; r < REPS; r++) {
    for (const e of EXPRS) total += eval1(e);
  }
  console.log(`RESULT=${total}`);
}

main();
