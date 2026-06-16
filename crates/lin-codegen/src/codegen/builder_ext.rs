//! Builder façade extension trait.
//!
//! Inkwell's `build_*` methods all return `Result<_, BuilderError>`. In codegen a
//! failed `build_*` is a compiler bug, so panicking (via `.unwrap()`) is the correct
//! behaviour — but writing `.unwrap()` on every single builder call is pure visual
//! noise. This trait provides a thin forwarding method per `build_*` function we use,
//! named by dropping the `build_` prefix, that calls the real inkwell method and
//! unwraps the result.
//!
//! The signatures mirror inkwell 0.9.0's exactly (with the `llvm22-1` / opaque-pointer
//! feature set), only changing the return type from `Result<X, BuilderError>` to `X`.
//! `gep` / `in_bounds_gep` are `unsafe` in inkwell, so the forwarders are too.

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::types::{BasicType, FloatMathType, FunctionType, IntMathType, PointerMathType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, CallSiteValue, FloatMathValue,
    FunctionValue, InstructionValue, IntMathValue, IntValue, PhiValue, PointerMathValue,
    PointerValue,
};
use inkwell::{FloatPredicate, IntPredicate};

#[allow(clippy::too_many_arguments)]
pub trait BuilderExt<'ctx> {
    // --- Calls ---
    fn call(
        &self,
        function: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> CallSiteValue<'ctx>;

    fn indirect_call(
        &self,
        function_type: FunctionType<'ctx>,
        function_pointer: PointerValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> CallSiteValue<'ctx>;

    // --- Memory ---
    fn store<V: BasicValue<'ctx>>(&self, ptr: PointerValue<'ctx>, value: V)
        -> InstructionValue<'ctx>;

    fn load<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        name: &str,
    ) -> BasicValueEnum<'ctx>;

    fn alloca<T: BasicType<'ctx>>(&self, ty: T, name: &str) -> PointerValue<'ctx>;

    /// # Safety
    /// GEP segfaults if indexes are used incorrectly; see inkwell's `build_gep`.
    unsafe fn gep<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> PointerValue<'ctx>;

    /// # Safety
    /// GEP segfaults if indexes are used incorrectly; see inkwell's `build_in_bounds_gep`.
    unsafe fn in_bounds_gep<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> PointerValue<'ctx>;

    fn struct_gep<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        index: u32,
        name: &str,
    ) -> PointerValue<'ctx>;

    // --- Control flow ---
    fn r#return(&self, value: Option<&dyn BasicValue<'ctx>>) -> InstructionValue<'ctx>;

    fn unconditional_branch(
        &self,
        destination_block: BasicBlock<'ctx>,
    ) -> InstructionValue<'ctx>;

    fn conditional_branch(
        &self,
        comparison: IntValue<'ctx>,
        then_block: BasicBlock<'ctx>,
        else_block: BasicBlock<'ctx>,
    ) -> InstructionValue<'ctx>;

    fn switch(
        &self,
        value: IntValue<'ctx>,
        else_block: BasicBlock<'ctx>,
        cases: &[(IntValue<'ctx>, BasicBlock<'ctx>)],
    ) -> InstructionValue<'ctx>;

    fn unreachable(&self) -> InstructionValue<'ctx>;

    fn phi<T: BasicType<'ctx>>(&self, type_: T, name: &str) -> PhiValue<'ctx>;

    // --- Integer arithmetic ---
    fn int_add<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn int_sub<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn int_mul<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn int_neg<T: IntMathValue<'ctx>>(&self, value: T, name: &str) -> T;
    fn int_signed_div<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn int_unsigned_div<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn int_signed_rem<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn int_unsigned_rem<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;

    // --- Bitwise ---
    fn and<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn or<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn xor<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn not<T: IntMathValue<'ctx>>(&self, value: T, name: &str) -> T;
    fn left_shift<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn right_shift<T: IntMathValue<'ctx>>(
        &self,
        lhs: T,
        rhs: T,
        sign_extend: bool,
        name: &str,
    ) -> T;

    // --- Integer casts ---
    fn int_s_extend<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T;
    fn int_z_extend<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T;
    fn int_s_extend_or_bit_cast<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T;
    fn int_z_extend_or_bit_cast<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T;
    fn int_truncate<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T;
    fn int_truncate_or_bit_cast<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T;

    // --- Comparisons ---
    fn int_compare<T: IntMathValue<'ctx>>(
        &self,
        op: IntPredicate,
        lhs: T,
        rhs: T,
        name: &str,
    ) -> <T::BaseType as IntMathType<'ctx>>::ValueType;

    fn float_compare<T: FloatMathValue<'ctx>>(
        &self,
        op: FloatPredicate,
        lhs: T,
        rhs: T,
        name: &str,
    ) -> <<T::BaseType as FloatMathType<'ctx>>::MathConvType as IntMathType<'ctx>>::ValueType;

    // --- Float arithmetic ---
    fn float_sub<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn float_mul<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn float_div<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn float_rem<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T;
    fn float_neg<T: FloatMathValue<'ctx>>(&self, value: T, name: &str) -> T;

    // --- Float casts ---
    fn float_ext<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        float_type: T::BaseType,
        name: &str,
    ) -> T;
    fn float_trunc<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        float_type: T::BaseType,
        name: &str,
    ) -> T;
    fn float_cast<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        float_type: T::BaseType,
        name: &str,
    ) -> T;
    fn float_to_signed_int<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        int_type: <T::BaseType as FloatMathType<'ctx>>::MathConvType,
        name: &str,
    ) -> <<T::BaseType as FloatMathType<'ctx>>::MathConvType as IntMathType<'ctx>>::ValueType;
    fn signed_int_to_float<T: IntMathValue<'ctx>>(
        &self,
        int: T,
        float_type: <T::BaseType as IntMathType<'ctx>>::MathConvType,
        name: &str,
    ) -> <<T::BaseType as IntMathType<'ctx>>::MathConvType as FloatMathType<'ctx>>::ValueType;

    // --- Pointer / value casts ---
    fn bit_cast<T, V>(&self, val: V, ty: T, name: &str) -> BasicValueEnum<'ctx>
    where
        T: BasicType<'ctx>,
        V: BasicValue<'ctx>;

    fn pointer_cast<T: PointerMathValue<'ctx>>(
        &self,
        from: T,
        to: T::BaseType,
        name: &str,
    ) -> T;

    fn ptr_to_int<T: PointerMathValue<'ctx>>(
        &self,
        ptr: T,
        int_type: <T::BaseType as PointerMathType<'ctx>>::PtrConvType,
        name: &str,
    ) -> <<T::BaseType as PointerMathType<'ctx>>::PtrConvType as IntMathType<'ctx>>::ValueType;

    fn select<BV: BasicValue<'ctx>>(
        &self,
        condition: IntValue<'ctx>,
        then_value: BV,
        else_value: BV,
        name: &str,
    ) -> BasicValueEnum<'ctx>;
}

#[allow(clippy::too_many_arguments)]
impl<'ctx> BuilderExt<'ctx> for Builder<'ctx> {
    fn call(
        &self,
        function: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> CallSiteValue<'ctx> {
        self.build_call(function, args, name).unwrap()
    }

    fn indirect_call(
        &self,
        function_type: FunctionType<'ctx>,
        function_pointer: PointerValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> CallSiteValue<'ctx> {
        self.build_indirect_call(function_type, function_pointer, args, name)
            .unwrap()
    }

    fn store<V: BasicValue<'ctx>>(
        &self,
        ptr: PointerValue<'ctx>,
        value: V,
    ) -> InstructionValue<'ctx> {
        self.build_store(ptr, value).unwrap()
    }

    fn load<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        self.build_load(pointee_ty, ptr, name).unwrap()
    }

    fn alloca<T: BasicType<'ctx>>(&self, ty: T, name: &str) -> PointerValue<'ctx> {
        self.build_alloca(ty, name).unwrap()
    }

    unsafe fn gep<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> PointerValue<'ctx> {
        self.build_gep(pointee_ty, ptr, ordered_indexes, name).unwrap()
    }

    unsafe fn in_bounds_gep<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> PointerValue<'ctx> {
        self.build_in_bounds_gep(pointee_ty, ptr, ordered_indexes, name)
            .unwrap()
    }

    fn struct_gep<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        index: u32,
        name: &str,
    ) -> PointerValue<'ctx> {
        self.build_struct_gep(pointee_ty, ptr, index, name).unwrap()
    }

    fn r#return(&self, value: Option<&dyn BasicValue<'ctx>>) -> InstructionValue<'ctx> {
        self.build_return(value).unwrap()
    }

    fn unconditional_branch(
        &self,
        destination_block: BasicBlock<'ctx>,
    ) -> InstructionValue<'ctx> {
        self.build_unconditional_branch(destination_block).unwrap()
    }

    fn conditional_branch(
        &self,
        comparison: IntValue<'ctx>,
        then_block: BasicBlock<'ctx>,
        else_block: BasicBlock<'ctx>,
    ) -> InstructionValue<'ctx> {
        self.build_conditional_branch(comparison, then_block, else_block)
            .unwrap()
    }

    fn switch(
        &self,
        value: IntValue<'ctx>,
        else_block: BasicBlock<'ctx>,
        cases: &[(IntValue<'ctx>, BasicBlock<'ctx>)],
    ) -> InstructionValue<'ctx> {
        self.build_switch(value, else_block, cases).unwrap()
    }

    fn unreachable(&self) -> InstructionValue<'ctx> {
        self.build_unreachable().unwrap()
    }

    fn phi<T: BasicType<'ctx>>(&self, type_: T, name: &str) -> PhiValue<'ctx> {
        self.build_phi(type_, name).unwrap()
    }

    fn int_add<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_int_add(lhs, rhs, name).unwrap()
    }

    fn int_sub<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_int_sub(lhs, rhs, name).unwrap()
    }

    fn int_mul<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_int_mul(lhs, rhs, name).unwrap()
    }

    fn int_neg<T: IntMathValue<'ctx>>(&self, value: T, name: &str) -> T {
        self.build_int_neg(value, name).unwrap()
    }

    fn int_signed_div<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_int_signed_div(lhs, rhs, name).unwrap()
    }

    fn int_unsigned_div<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_int_unsigned_div(lhs, rhs, name).unwrap()
    }

    fn int_signed_rem<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_int_signed_rem(lhs, rhs, name).unwrap()
    }

    fn int_unsigned_rem<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_int_unsigned_rem(lhs, rhs, name).unwrap()
    }

    fn and<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_and(lhs, rhs, name).unwrap()
    }

    fn or<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_or(lhs, rhs, name).unwrap()
    }

    fn xor<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_xor(lhs, rhs, name).unwrap()
    }

    fn not<T: IntMathValue<'ctx>>(&self, value: T, name: &str) -> T {
        self.build_not(value, name).unwrap()
    }

    fn left_shift<T: IntMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_left_shift(lhs, rhs, name).unwrap()
    }

    fn right_shift<T: IntMathValue<'ctx>>(
        &self,
        lhs: T,
        rhs: T,
        sign_extend: bool,
        name: &str,
    ) -> T {
        self.build_right_shift(lhs, rhs, sign_extend, name).unwrap()
    }

    fn int_s_extend<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_int_s_extend(int_value, int_type, name).unwrap()
    }

    fn int_z_extend<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_int_z_extend(int_value, int_type, name).unwrap()
    }

    fn int_s_extend_or_bit_cast<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_int_s_extend_or_bit_cast(int_value, int_type, name)
            .unwrap()
    }

    fn int_z_extend_or_bit_cast<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_int_z_extend_or_bit_cast(int_value, int_type, name)
            .unwrap()
    }

    fn int_truncate<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_int_truncate(int_value, int_type, name).unwrap()
    }

    fn int_truncate_or_bit_cast<T: IntMathValue<'ctx>>(
        &self,
        int_value: T,
        int_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_int_truncate_or_bit_cast(int_value, int_type, name)
            .unwrap()
    }

    fn int_compare<T: IntMathValue<'ctx>>(
        &self,
        op: IntPredicate,
        lhs: T,
        rhs: T,
        name: &str,
    ) -> <T::BaseType as IntMathType<'ctx>>::ValueType {
        self.build_int_compare(op, lhs, rhs, name).unwrap()
    }

    fn float_compare<T: FloatMathValue<'ctx>>(
        &self,
        op: FloatPredicate,
        lhs: T,
        rhs: T,
        name: &str,
    ) -> <<T::BaseType as FloatMathType<'ctx>>::MathConvType as IntMathType<'ctx>>::ValueType {
        self.build_float_compare(op, lhs, rhs, name).unwrap()
    }

    fn float_sub<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_float_sub(lhs, rhs, name).unwrap()
    }

    fn float_mul<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_float_mul(lhs, rhs, name).unwrap()
    }

    fn float_div<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_float_div(lhs, rhs, name).unwrap()
    }

    fn float_rem<T: FloatMathValue<'ctx>>(&self, lhs: T, rhs: T, name: &str) -> T {
        self.build_float_rem(lhs, rhs, name).unwrap()
    }

    fn float_neg<T: FloatMathValue<'ctx>>(&self, value: T, name: &str) -> T {
        self.build_float_neg(value, name).unwrap()
    }

    fn float_ext<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        float_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_float_ext(float, float_type, name).unwrap()
    }

    fn float_trunc<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        float_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_float_trunc(float, float_type, name).unwrap()
    }

    fn float_cast<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        float_type: T::BaseType,
        name: &str,
    ) -> T {
        self.build_float_cast(float, float_type, name).unwrap()
    }

    fn float_to_signed_int<T: FloatMathValue<'ctx>>(
        &self,
        float: T,
        int_type: <T::BaseType as FloatMathType<'ctx>>::MathConvType,
        name: &str,
    ) -> <<T::BaseType as FloatMathType<'ctx>>::MathConvType as IntMathType<'ctx>>::ValueType {
        self.build_float_to_signed_int(float, int_type, name).unwrap()
    }

    fn signed_int_to_float<T: IntMathValue<'ctx>>(
        &self,
        int: T,
        float_type: <T::BaseType as IntMathType<'ctx>>::MathConvType,
        name: &str,
    ) -> <<T::BaseType as IntMathType<'ctx>>::MathConvType as FloatMathType<'ctx>>::ValueType {
        self.build_signed_int_to_float(int, float_type, name).unwrap()
    }

    fn bit_cast<T, V>(&self, val: V, ty: T, name: &str) -> BasicValueEnum<'ctx>
    where
        T: BasicType<'ctx>,
        V: BasicValue<'ctx>,
    {
        self.build_bit_cast(val, ty, name).unwrap()
    }

    fn pointer_cast<T: PointerMathValue<'ctx>>(
        &self,
        from: T,
        to: T::BaseType,
        name: &str,
    ) -> T {
        self.build_pointer_cast(from, to, name).unwrap()
    }

    fn ptr_to_int<T: PointerMathValue<'ctx>>(
        &self,
        ptr: T,
        int_type: <T::BaseType as PointerMathType<'ctx>>::PtrConvType,
        name: &str,
    ) -> <<T::BaseType as PointerMathType<'ctx>>::PtrConvType as IntMathType<'ctx>>::ValueType {
        self.build_ptr_to_int(ptr, int_type, name).unwrap()
    }

    fn select<BV: BasicValue<'ctx>>(
        &self,
        condition: IntValue<'ctx>,
        then_value: BV,
        else_value: BV,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        self.build_select(condition, then_value, else_value, name).unwrap()
    }
}
