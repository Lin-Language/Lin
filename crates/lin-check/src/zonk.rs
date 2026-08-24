/// Zonking pass: replace solved TypeVar(id) nodes in a TypedModule with their
/// concrete types. Any TypeVar that remains unsolved after this pass is either
/// a legitimate generic slot (intrinsic IDs ≥ 9000) or an under-constrained
/// user expression that will be reported as a warning.
///
/// This prevents TypeVar nodes from silently reaching codegen where they cause
/// incorrect tagged-value fallbacks.
use std::collections::HashMap;
use crate::typed_ir::*;
use crate::types::Type;

/// Walk `module` in-place, replacing every TypeVar(id) that appears in `subs`
/// with its solution. TypeVars not in `subs` are left unchanged.
pub fn zonk_module(module: &mut TypedModule, subs: &HashMap<u32, Type>) {
    if subs.is_empty() {
        return;
    }
    for stmt in &mut module.statements {
        zonk_stmt(stmt, subs);
    }
}

pub(crate) fn zonk_type(ty: &Type, subs: &HashMap<u32, Type>) -> Type {
    zonk_type_guarded(ty, subs, &mut Vec::new())
}

/// `zonk_type` with an explicit stack of TypeVar ids currently being expanded.
///
/// BACKSTOP against a cyclic substitution map (`a := a`, or `a := b` / `b := a`). The checker's
/// occurs check (`helpers::occurs_in`) is what actually prevents such an entry from being recorded;
/// this guard exists because zonking is the point where a cycle turns into unbounded recursion, and
/// an infinite loop here takes the whole process down — including the language server, which zonks
/// every workspace file. On re-entering a var already on the stack we stop and leave it as an
/// unsolved `TypeVar`, which downstream already handles (it is the same shape a genuinely
/// under-constrained variable has).
fn zonk_type_guarded(ty: &Type, subs: &HashMap<u32, Type>, expanding: &mut Vec<u32>) -> Type {
    match ty {
        Type::TypeVar(id) => {
            if let Some(concrete) = subs.get(id) {
                if expanding.contains(id) {
                    // Cyclic substitution — stop unfolding and keep the variable unsolved.
                    return ty.clone();
                }
                expanding.push(*id);
                // Recursively zonk the solution in case it also contains TypeVars.
                let out = zonk_type_guarded(concrete, subs, expanding);
                expanding.pop();
                out
            } else {
                ty.clone()
            }
        }
        Type::Array(inner) => Type::Array(Box::new(zonk_type_guarded(inner, subs, expanding))),
        Type::FixedArray(ts) => Type::FixedArray(ts.iter().map(|t| zonk_type_guarded(t, subs, expanding)).collect()),
        Type::Iterator(inner) => Type::Iterator(Box::new(zonk_type_guarded(inner, subs, expanding))),
        Type::Shared(inner) => Type::Shared(Box::new(zonk_type_guarded(inner, subs, expanding))),
        Type::Stream(inner) => Type::Stream(Box::new(zonk_type_guarded(inner, subs, expanding))),
        Type::Promise(inner) => Type::Promise(Box::new(zonk_type_guarded(inner, subs, expanding))),
        Type::Union(ts) => Type::flatten_union(ts.iter().map(|t| zonk_type_guarded(t, subs, expanding)).collect()),
        Type::Function { params, ret, required, lset } => Type::Function {
            params: params.iter().map(|p| zonk_type_guarded(p, subs, expanding)).collect(),
            ret: Box::new(zonk_type_guarded(ret, subs, expanding)),
            required: *required,
            lset: lset.clone(),
        },
        Type::Object { fields, sealed, name } => {
            // Zonking substitutes TypeVars in field types; it must PRESERVE the sealed marker
            // and the alias name so a zonked named-record type stays sealed and named (the alias
            // name must survive to Display/LSP after zonking).
            let mut out = indexmap::IndexMap::new();
            for (k, v) in fields {
                out.insert(k.clone(), zonk_type_guarded(v, subs, expanding));
            }
            Type::Object { fields: out, sealed: *sealed, name: name.clone() }
        }
        _ => ty.clone(),
    }
}

fn zonk_stmt(stmt: &mut TypedStmt, subs: &HashMap<u32, Type>) {
    match stmt {
        TypedStmt::Val { value, ty, .. } => {
            *ty = zonk_type(ty, subs);
            zonk_expr(value, subs);
        }
        TypedStmt::Var { value, ty, .. } => {
            *ty = zonk_type(ty, subs);
            zonk_expr(value, subs);
        }
        TypedStmt::Import { bindings, .. } => {
            for b in bindings {
                b.ty = zonk_type(&b.ty, subs);
            }
        }
        TypedStmt::ForeignImport { bindings, .. } => {
            for b in bindings {
                b.ty = zonk_type(&b.ty, subs);
            }
        }
        TypedStmt::Destructure { value, obj_ty, fields, .. } => {
            *obj_ty = zonk_type(obj_ty, subs);
            zonk_expr(value, subs);
            for (_, _, ty) in fields {
                *ty = zonk_type(ty, subs);
            }
        }
        TypedStmt::ArrayDestructure { value, elem_ty, elements, rest, .. } => {
            *elem_ty = zonk_type(elem_ty, subs);
            zonk_expr(value, subs);
            for (_, _, ty) in elements {
                *ty = zonk_type(ty, subs);
            }
            if let Some((_, ty)) = rest {
                *ty = zonk_type(ty, subs);
            }
        }
        TypedStmt::Expr(e) => zonk_expr(e, subs),
    }
}

fn zonk_expr(expr: &mut TypedExpr, subs: &HashMap<u32, Type>) {
    match expr {
        TypedExpr::IntLit(_, ty, _) => *ty = zonk_type(ty, subs),
        TypedExpr::FloatLit(_, ty, _) => *ty = zonk_type(ty, subs),
        TypedExpr::StringLit(..) | TypedExpr::BoolLit(..) | TypedExpr::NullLit(..) => {}
        TypedExpr::LocalGet { ty, .. } => *ty = zonk_type(ty, subs),
        TypedExpr::LocalSet { value, ty, .. } => {
            *ty = zonk_type(ty, subs);
            zonk_expr(value, subs);
        }
        TypedExpr::BinaryOp { left, right, result_type, .. } => {
            zonk_expr(left, subs);
            zonk_expr(right, subs);
            *result_type = zonk_type(result_type, subs);
        }
        TypedExpr::UnaryOp { operand, result_type, .. } => {
            zonk_expr(operand, subs);
            *result_type = zonk_type(result_type, subs);
        }
        TypedExpr::Coerce { expr, from, to, .. } => {
            zonk_expr(expr, subs);
            *from = zonk_type(from, subs);
            *to = zonk_type(to, subs);
        }
        TypedExpr::Call { func, args, result_type, .. } => {
            zonk_expr(func, subs);
            for a in args { zonk_expr(a, subs); }
            *result_type = zonk_type(result_type, subs);
        }
        TypedExpr::If { cond, then_br, else_br, result_type, .. } => {
            zonk_expr(cond, subs);
            zonk_expr(then_br, subs);
            zonk_expr(else_br, subs);
            *result_type = zonk_type(result_type, subs);
        }
        TypedExpr::FromJson { target, value, result_type, named_defs, .. } => {
            *target = zonk_type(target, subs);
            zonk_expr(value, subs);
            *result_type = zonk_type(result_type, subs);
            for (_, body) in named_defs.iter_mut() {
                *body = zonk_type(body, subs);
            }
        }
        TypedExpr::Match { scrutinee, arms, result_type, .. } => {
            zonk_expr(scrutinee, subs);
            for arm in arms { zonk_match_arm(arm, subs); }
            *result_type = zonk_type(result_type, subs);
        }
        TypedExpr::Block { stmts, expr, ty, .. } => {
            for s in stmts { zonk_stmt(s, subs); }
            zonk_expr(expr, subs);
            *ty = zonk_type(ty, subs);
        }
        TypedExpr::Function { params, body, ret_type, captures, .. } => {
            for p in params { p.ty = zonk_type(&p.ty, subs); }
            zonk_expr(body, subs);
            *ret_type = zonk_type(ret_type, subs);
            for c in captures { c.ty = zonk_type(&c.ty, subs); }
        }
        TypedExpr::MakeObject { fields, spreads, computed_fields, ty, .. } => {
            for (_, e) in fields { zonk_expr(e, subs); }
            for s in spreads { zonk_expr(s, subs); }
            for (k, v) in computed_fields { zonk_expr(k, subs); zonk_expr(v, subs); }
            *ty = zonk_type(ty, subs);
        }
        TypedExpr::MakeArray { elements, spreads, ty, .. } => {
            for e in elements { zonk_expr(e, subs); }
            for (_, s) in spreads { zonk_expr(s, subs); }
            *ty = zonk_type(ty, subs);
        }
        TypedExpr::Index { object, key, result_type, .. } => {
            zonk_expr(object, subs);
            zonk_expr(key, subs);
            *result_type = zonk_type(result_type, subs);
        }
        TypedExpr::FieldGet { object, result_type, .. } => {
            zonk_expr(object, subs);
            *result_type = zonk_type(result_type, subs);
        }
        TypedExpr::IndexSet { object, key, value, obj_ty, .. } => {
            zonk_expr(object, subs);
            zonk_expr(key, subs);
            zonk_expr(value, subs);
            *obj_ty = zonk_type(obj_ty, subs);
        }
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts {
                if let TypedStringPart::Expr(e) = p { zonk_expr(e, subs); }
            }
        }
        TypedExpr::Is { expr, pattern, .. } => {
            zonk_expr(expr, subs);
            zonk_pattern(pattern, subs);
        }
        TypedExpr::Has { expr, pattern, .. } => {
            zonk_expr(expr, subs);
            zonk_pattern(pattern, subs);
        }
    }
}

fn zonk_match_arm(arm: &mut TypedMatchArm, subs: &HashMap<u32, Type>) {
    match &mut arm.pattern {
        TypedMatchPattern::Is(p) | TypedMatchPattern::Has(p) => zonk_pattern(p, subs),
        TypedMatchPattern::Else => {}
    }
    if let Some(g) = &mut arm.guard { zonk_expr(g, subs); }
    zonk_expr(&mut arm.body, subs);
}

fn zonk_pattern(pat: &mut TypedPattern, subs: &HashMap<u32, Type>) {
    match pat {
        TypedPattern::TypeCheck(ty, _) => *ty = zonk_type(ty, subs),
        TypedPattern::TypeCheckDeep(ty, named_defs, _) => {
            *ty = zonk_type(ty, subs);
            for (_, body) in named_defs.iter_mut() {
                *body = zonk_type(body, subs);
            }
        }
        TypedPattern::Literal(e) => zonk_expr(e, subs),
        TypedPattern::Binding(_, ty, _) => *ty = zonk_type(ty, subs),
        TypedPattern::Object { fields, .. } => {
            for f in fields { f.ty = zonk_type(&f.ty, subs); }
        }
        TypedPattern::Array { elements, .. } => {
            for e in elements { zonk_pattern(e, subs); }
        }
        TypedPattern::Wildcard(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Type;

    /// A cyclic substitution map must not send `zonk_type` into unbounded recursion. The checker's
    /// occurs check should keep such an entry out of `solved_type_vars` in the first place; this
    /// pins the backstop so a future inference change can't turn a bad binding into a process-killing
    /// stack overflow (it took down the language server — see `occurs_check_*` in the checker tests).
    #[test]
    fn zonk_type_terminates_on_a_self_referential_substitution() {
        let mut subs = HashMap::new();
        subs.insert(1, Type::TypeVar(1));
        assert_eq!(zonk_type(&Type::TypeVar(1), &subs), Type::TypeVar(1));
    }

    #[test]
    fn zonk_type_terminates_on_an_indirect_substitution_cycle() {
        let mut subs = HashMap::new();
        subs.insert(1, Type::Array(Box::new(Type::TypeVar(2))));
        subs.insert(2, Type::Array(Box::new(Type::TypeVar(1))));
        // Unfolds until it re-enters `1`, then stops with the variable left unsolved.
        let out = zonk_type(&Type::TypeVar(1), &subs);
        assert_eq!(
            out,
            Type::Array(Box::new(Type::Array(Box::new(Type::TypeVar(1)))))
        );
    }

    /// The guard must not truncate a legitimately deep, ACYCLIC chain.
    #[test]
    fn zonk_type_still_fully_expands_an_acyclic_chain() {
        let mut subs = HashMap::new();
        subs.insert(1, Type::TypeVar(2));
        subs.insert(2, Type::Array(Box::new(Type::TypeVar(3))));
        subs.insert(3, Type::Int32);
        assert_eq!(
            zonk_type(&Type::TypeVar(1), &subs),
            Type::Array(Box::new(Type::Int32))
        );
    }
}
