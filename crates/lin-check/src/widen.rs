use crate::types::Type;

pub fn widen_numeric(left: &Type, right: &Type) -> Option<Type> {
    if !left.is_numeric() || !right.is_numeric() {
        return None;
    }
    if left == right {
        return Some(left.clone());
    }

    match (left.is_float(), right.is_float()) {
        (true, true) => Some(widen_two_floats(left, right)),
        (true, false) => Some(widen_int_float(right, left)),
        (false, true) => Some(widen_int_float(left, right)),
        (false, false) => Some(widen_two_ints(left, right)),
    }
}

fn widen_two_floats(a: &Type, b: &Type) -> Type {
    let wa = a.bit_width().unwrap();
    let wb = b.bit_width().unwrap();
    if wa >= wb { a.clone() } else { b.clone() }
}

fn widen_int_float(int_ty: &Type, float_ty: &Type) -> Type {
    let int_bits = int_ty.bit_width().unwrap();
    let float_bits = float_ty.bit_width().unwrap();
    if float_bits >= 64 || (float_bits >= 32 && int_bits <= 24) {
        return float_ty.clone();
    }
    Type::Float64
}

fn widen_two_ints(a: &Type, b: &Type) -> Type {
    match (a.is_signed(), b.is_signed()) {
        (true, true) | (false, false) => {
            let wa = a.bit_width().unwrap();
            let wb = b.bit_width().unwrap();
            if wa >= wb { a.clone() } else { b.clone() }
        }
        _ => {
            let wa = a.bit_width().unwrap();
            let wb = b.bit_width().unwrap();
            let max_bits = wa.max(wb);
            match max_bits {
                8 => Type::Int16,
                16 => Type::Int32,
                32 => Type::Int64,
                _ => Type::Int64,
            }
        }
    }
}
