//! Serialization predicates shared by the wire types.

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

pub(crate) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}
